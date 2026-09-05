//! N2 fixed-model facade. Business prompts and routing remain in Beacon.
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use harborbeacon_local_agent::runtime::fixed_models::{
    CHAT_MODEL, CHAT_SHA256, EMBEDDING_MODEL, EMBEDDING_SHA256, TOKENIZER_SHA256,
};
use harborbeacon_local_agent::service_auth::{model_api_verifier_token, VerifierTokens};
use reqwest::blocking::Client;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tiny_http::{Header, Method, Request, Response, Server};
use tokenizers::Tokenizer;

const DEADLINE: Duration = Duration::from_secs(90);
const CAPACITY: usize = 4;
const MAX_BODY: u64 = 1024 * 1024;

#[derive(Default)]
struct WorkerStatus {
    ready: AtomicBool,
    quarantined: AtomicBool,
    queued: AtomicUsize,
    active: AtomicBool,
    last_queue_wait_ms: AtomicU64,
    completed: AtomicU64,
    process: Mutex<Option<Child>>,
}

impl WorkerStatus {
    fn snapshot(&self) -> Value {
        if let Ok(mut child) = self.process.lock() {
            if child
                .as_mut()
                .is_some_and(|p| !matches!(p.try_wait(), Ok(None)))
            {
                self.ready.store(false, Ordering::SeqCst);
            }
        }
        json!({"ready": self.ready.load(Ordering::SeqCst),
            "quarantined": self.quarantined.load(Ordering::SeqCst),
            "queued": self.queued.load(Ordering::SeqCst), "capacity": CAPACITY,
            "last_queue_wait_ms": self.last_queue_wait_ms.load(Ordering::SeqCst),
            "completed": self.completed.load(Ordering::SeqCst),
            "active": self.active.load(Ordering::SeqCst)})
    }

    fn stop(&self) -> bool {
        self.ready.store(false, Ordering::SeqCst);
        let Ok(mut guard) = self.process.lock() else {
            self.quarantined.store(true, Ordering::SeqCst);
            return false;
        };
        let Some(child) = guard.as_mut() else {
            return true;
        };
        let _ = child.kill();
        drop(guard);
        let until = Instant::now() + Duration::from_secs(5);
        while Instant::now() < until {
            let Ok(mut guard) = self.process.lock() else {
                break;
            };
            let Some(child) = guard.as_mut() else {
                return true;
            };
            if matches!(child.try_wait(), Ok(Some(_))) {
                *guard = None;
                return true;
            }
            drop(guard);
            thread::sleep(Duration::from_millis(20));
        }
        self.quarantined.store(true, Ordering::SeqCst);
        false
    }
}

struct Job {
    request: Request,
    body: Value,
    admitted: Instant,
}
struct Worker {
    queue: SyncSender<Job>,
    status: Arc<WorkerStatus>,
}

struct Incoming {
    request: Request,
    admitted: Instant,
}

fn admission(worker: Worker, model: &'static str) -> SyncSender<Incoming> {
    let (sender, receiver) = mpsc::sync_channel(CAPACITY);
    thread::spawn(move || {
        for incoming in receiver {
            accept_job(&worker, incoming, model);
        }
    });
    sender
}

fn enqueue(sender: &SyncSender<Incoming>, request: Request) {
    if let Err(TrySendError::Full(incoming) | TrySendError::Disconnected(incoming)) = sender
        .try_send(Incoming {
            request,
            admitted: Instant::now(),
        })
    {
        error(incoming.request, 429, "MODEL_QUEUE_FULL", true);
    }
}

fn checked_file(path: &Path, expected: &str) -> Result<(), String> {
    let mut file = File::open(path).map_err(|error| error.to_string())?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    if format!("{:x}", digest.finalize()) != expected {
        return Err(format!("model digest mismatch: {}", path.display()));
    }
    Ok(())
}

fn answer(request: Request, status: u16, body: Value, stopped: bool) {
    let data = serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec());
    let response = Response::from_data(data)
        .with_status_code(status)
        .with_header(Header::from_bytes("Content-Type", "application/json").unwrap())
        .with_header(
            Header::from_bytes(
                "X-Harbor-Execution-Stopped",
                if stopped { "true" } else { "false" },
            )
            .unwrap(),
        );
    let _ = request.respond(response);
}

fn error(request: Request, status: u16, code: &str, stopped: bool) {
    answer(
        request,
        status,
        json!({"ok": false, "error": {"code": code, "message": code}, "execution_stopped": stopped}),
        stopped,
    );
}

fn worker<F>(run: F) -> Worker
where
    F: FnOnce(Receiver<Job>, Arc<WorkerStatus>) + Send + 'static,
{
    let (queue, receiver) = mpsc::sync_channel(CAPACITY);
    let status = Arc::new(WorkerStatus::default());
    let state = status.clone();
    thread::spawn(move || run(receiver, state));
    Worker { queue, status }
}

fn accept_job(worker: &Worker, incoming: Incoming, model: &str) {
    let Incoming {
        mut request,
        admitted,
    } = incoming;
    if admitted.elapsed() >= DEADLINE {
        error(request, 504, "MODEL_QUEUE_TIMEOUT", true);
        return;
    }
    if worker.status.quarantined.load(Ordering::SeqCst) {
        error(request, 503, "MODEL_RUNTIME_QUARANTINED", true);
        return;
    }
    if request
        .body_length()
        .is_some_and(|size| size > MAX_BODY as usize)
    {
        error(request, 413, "REQUEST_TOO_LARGE", true);
        return;
    }
    let mut bytes = Vec::new();
    if request
        .as_reader()
        .take(MAX_BODY + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() > MAX_BODY as usize
    {
        error(request, 413, "REQUEST_TOO_LARGE", true);
        return;
    }
    let Ok(mut body) = serde_json::from_slice::<Value>(&bytes) else {
        error(request, 400, "INVALID_JSON", true);
        return;
    };
    if !body.is_object()
        || body
            .get("model")
            .and_then(Value::as_str)
            .is_some_and(|name| name != model)
    {
        error(request, 403, "LOCAL_MODELS_FIXED", true);
        return;
    }
    if body.get("stream").and_then(Value::as_bool) == Some(true) {
        error(request, 400, "STREAMING_NOT_SUPPORTED", true);
        return;
    }
    body["model"] = json!(model);
    worker.status.queued.fetch_add(1, Ordering::SeqCst);
    match worker.queue.try_send(Job {
        request,
        body,
        admitted,
    }) {
        Ok(()) => {}
        Err(TrySendError::Full(job) | TrySendError::Disconnected(job)) => {
            worker.status.queued.fetch_sub(1, Ordering::SeqCst);
            error(job.request, 429, "MODEL_QUEUE_FULL", true);
        }
    }
}

fn next_job(receiver: &Receiver<Job>, status: &WorkerStatus) -> Option<Job> {
    loop {
        let job = receiver.recv().ok()?;
        status.queued.fetch_sub(1, Ordering::SeqCst);
        if job.admitted.elapsed() >= DEADLINE {
            error(job.request, 504, "MODEL_QUEUE_TIMEOUT", true);
        } else if status.quarantined.load(Ordering::SeqCst) {
            error(job.request, 503, "MODEL_RUNTIME_QUARANTINED", true);
        } else {
            status.active.store(true, Ordering::SeqCst);
            status
                .last_queue_wait_ms
                .store(job.admitted.elapsed().as_millis() as u64, Ordering::SeqCst);
            return Some(job);
        }
    }
}

fn start_chat(
    root: &Path,
    status: &WorkerStatus,
    client: &Client,
    until: Instant,
) -> Result<(), String> {
    if status.ready.load(Ordering::SeqCst) {
        return Ok(());
    }
    if !status.stop() {
        return Err("MODEL_RUNTIME_QUARANTINED".into());
    }
    if Instant::now() >= until {
        return Err("MODEL_QUEUE_TIMEOUT".into());
    }
    let vendor = PathBuf::from("/usr/lib/harboros-model-runtime/vendor");
    let child = Command::new(vendor.join("bin/llama-server"))
        .env("LLAMA_API_KEY", model_api_verifier_token()?.current)
        .env("LD_LIBRARY_PATH", vendor.join("lib"))
        .env("SPACEMIT_PERFER_CORE_ID", "12,13,14,15")
        .args([
            "--host",
            "127.0.0.1",
            "--port",
            "8793",
            "--parallel",
            "1",
            "--ctx-size",
            "4096",
            "--threads",
            "4",
            "--threads-batch",
            "4",
            "--alias",
            CHAT_MODEL,
            "--no-webui",
        ])
        .arg("--model")
        .arg(root.join("chat.gguf"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| error.to_string())?;
    *status.process.lock().map_err(|error| error.to_string())? = Some(child);
    while Instant::now() < until {
        if client
            .get("http://127.0.0.1:8793/health")
            .timeout(Duration::from_millis(500))
            .send()
            .is_ok_and(|response| response.status().is_success())
        {
            status.ready.store(true, Ordering::SeqCst);
            return Ok(());
        }
        if status
            .process
            .lock()
            .unwrap()
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(Some(_))))
        {
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }
    status.stop();
    Err("MODEL_START_FAILED".into())
}

fn chat_worker(root: PathBuf, receiver: Receiver<Job>, status: Arc<WorkerStatus>) {
    let client = Client::builder()
        .timeout(DEADLINE)
        .build()
        .expect("chat HTTP client");
    while let Some(job) = next_job(&receiver, &status) {
        let result = start_chat(&root, &status, &client, job.admitted + DEADLINE).and_then(|_| {
            let remaining = DEADLINE
                .checked_sub(job.admitted.elapsed())
                .ok_or("MODEL_QUEUE_TIMEOUT")?;
            let response = client
                .post("http://127.0.0.1:8793/v1/chat/completions")
                .bearer_auth(&model_api_verifier_token()?.current)
                .timeout(remaining)
                .json(&job.body)
                .send()
                .map_err(|error| error.to_string())?;
            let code = response.status().as_u16();
            let body = response
                .json::<Value>()
                .map_err(|error| error.to_string())?;
            Ok((code, body))
        });
        match result {
            Ok((code, body)) => answer(job.request, code, body, true),
            Err(message) => {
                eprintln!("chat request failed: {message}");
                let stopped = status.stop();
                error(job.request, 503, "MODEL_EXECUTION_FAILED", stopped);
            }
        }
        status.active.store(false, Ordering::SeqCst);
        status.completed.fetch_add(1, Ordering::SeqCst);
    }
    status.stop();
}

struct EmbeddingProcess {
    input: std::process::ChildStdin,
    output: Receiver<String>,
}

fn start_embedding(
    root: &Path,
    status: &WorkerStatus,
    until: Instant,
) -> Result<EmbeddingProcess, String> {
    if !status.stop() {
        return Err("MODEL_RUNTIME_QUARANTINED".into());
    }
    if Instant::now() >= until {
        return Err("MODEL_QUEUE_TIMEOUT".into());
    }
    let mut child = Command::new("/usr/bin/python3")
        .arg("/usr/lib/harboros-model-runtime/n2_embedding_worker.py")
        .arg(root.join("embedding.onnx"))
        .env_remove("LD_LIBRARY_PATH")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|error| error.to_string())?;
    let input = child.stdin.take().ok_or("embedding stdin unavailable")?;
    let output = child.stdout.take().ok_or("embedding stdout unavailable")?;
    *status.process.lock().map_err(|error| error.to_string())? = Some(child);
    let (sender, receiver) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut reader = BufReader::new(output);
        loop {
            let mut line = String::new();
            match reader.by_ref().take(MAX_BODY).read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) if !line.ends_with('\n') => break,
                Ok(_) => {
                    if sender.send(line).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let ready = receiver
        .recv_timeout(until.saturating_duration_since(Instant::now()))
        .map_err(|error| error.to_string())?;
    let report: Value = serde_json::from_str(&ready).map_err(|error| error.to_string())?;
    if report["ready"] != true {
        return Err("embedding startup failed".into());
    }
    status.ready.store(true, Ordering::SeqCst);
    Ok(EmbeddingProcess {
        input,
        output: receiver,
    })
}

fn embedding_inputs(body: &Value, tokenizer: &Tokenizer) -> Result<(Vec<Vec<u32>>, usize), String> {
    if body
        .get("encoding_format")
        .and_then(Value::as_str)
        .is_some_and(|value| value != "float")
        || body.get("dimensions").is_some_and(|value| value != 768)
    {
        return Err("unsupported embedding format or dimensions".into());
    }
    let texts: Vec<&str> = match &body["input"] {
        Value::String(text) => vec![text],
        Value::Array(items) if !items.is_empty() && items.len() <= 32 => items
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .ok_or("input must contain strings".to_string())
            })
            .collect::<Result<_, _>>()?,
        _ => return Err("input must be a string or a batch of 1..32 strings".into()),
    };
    let mut batch = Vec::new();
    let mut count = 0;
    for text in texts {
        if text.trim().is_empty() {
            return Err("input must not be empty".into());
        }
        let encoded = tokenizer
            .encode(text, true)
            .map_err(|error| error.to_string())?;
        if encoded.len() > 8192 {
            return Err("embedding input exceeds 8192 tokens".into());
        }
        count += encoded.len();
        batch.push(encoded.get_ids().to_vec());
    }
    Ok((batch, count))
}

fn valid_embedding_vectors(vectors: &[Value], expected_count: usize) -> bool {
    vectors.len() == expected_count
        && vectors.iter().all(|vector| {
            let Some(values) = vector.as_array() else {
                return false;
            };
            if values.len() != 768 {
                return false;
            }
            let mut squared_norm = 0.0;
            for value in values {
                let Some(value) = value.as_f64().filter(|v| v.is_finite()) else {
                    return false;
                };
                squared_norm += value * value;
            }
            (squared_norm - 1.0).abs() < 0.001
        })
}

fn embedding_worker(
    root: PathBuf,
    receiver: Receiver<Job>,
    status: Arc<WorkerStatus>,
    tokenizer: Tokenizer,
) {
    let mut process = start_embedding(&root, &status, Instant::now() + DEADLINE).ok();
    if process.is_none() {
        status.stop();
    }
    while let Some(job) = next_job(&receiver, &status) {
        let (batch, count) = match embedding_inputs(&job.body, &tokenizer) {
            Ok(input) => input,
            Err(message) => {
                answer(
                    job.request,
                    400,
                    json!({"error": {"code": "INVALID_INPUT", "message": message}}),
                    true,
                );
                status.active.store(false, Ordering::SeqCst);
                continue;
            }
        };
        let result = (|| -> Result<Value, String> {
            if process.is_none() {
                process = Some(start_embedding(&root, &status, job.admitted + DEADLINE)?);
            }
            if job.admitted.elapsed() >= DEADLINE {
                return Err("MODEL_QUEUE_TIMEOUT".into());
            }
            let worker = process.as_mut().unwrap();
            let line = serde_json::to_string(&json!({"input_ids": batch})).unwrap();
            writeln!(worker.input, "{line}").map_err(|error| error.to_string())?;
            worker.input.flush().map_err(|error| error.to_string())?;
            let remaining = DEADLINE
                .checked_sub(job.admitted.elapsed())
                .ok_or("embedding deadline")?;
            let output = worker
                .output
                .recv_timeout(remaining)
                .map_err(|error| error.to_string())?;
            let output: Value = serde_json::from_str(&output).map_err(|error| error.to_string())?;
            let vectors = output["vectors"]
                .as_array()
                .ok_or("embedding worker failed")?;
            if !valid_embedding_vectors(vectors, batch.len()) {
                return Err("invalid embedding vectors".into());
            }
            let data: Vec<Value> = vectors.iter().enumerate().map(|(index, vector)| json!({"object": "embedding", "index": index, "embedding": vector})).collect();
            Ok(
                json!({"object": "list", "model": EMBEDDING_MODEL, "data": data, "usage": {"prompt_tokens": count, "total_tokens": count}}),
            )
        })();
        match result {
            Ok(body) => answer(job.request, 200, body, true),
            Err(message) => {
                eprintln!("embedding request failed: {message}");
                let stopped = status.stop();
                process = None;
                error(job.request, 503, "MODEL_EXECUTION_FAILED", stopped);
            }
        }
        status.active.store(false, Ordering::SeqCst);
        status.completed.fetch_add(1, Ordering::SeqCst);
    }
    status.stop();
}

fn authorized(request: &Request, verifier: &VerifierTokens) -> bool {
    request
        .headers()
        .iter()
        .find(|header| header.field.equiv("Authorization"))
        .and_then(|header| header.value.as_str().strip_prefix("Bearer "))
        .is_some_and(|token| verifier.matches(token))
}

fn run() -> Result<(), String> {
    let root = PathBuf::from("/data/models/current");
    checked_file(&root.join("chat.gguf"), CHAT_SHA256)?;
    checked_file(&root.join("embedding.onnx"), EMBEDDING_SHA256)?;
    checked_file(&root.join("tokenizer.json"), TOKENIZER_SHA256)?;
    let tokenizer =
        Tokenizer::from_file(root.join("tokenizer.json")).map_err(|error| error.to_string())?;
    let verifier = model_api_verifier_token()?;
    // Reserve the listener before starting any owned process.
    let server = Server::http("127.0.0.1:8792").map_err(|error| error.to_string())?;
    let chat_root = root.clone();
    let chat = worker(move |receiver, status| chat_worker(chat_root, receiver, status));
    let embedding =
        worker(move |receiver, status| embedding_worker(root, receiver, status, tokenizer));
    let chat_status = chat.status.clone();
    let embedding_status = embedding.status.clone();
    // Body admission has its own bounded worker so slow bodies cannot delay health checks.
    let chat_queue = admission(chat, CHAT_MODEL);
    let embedding_queue = admission(embedding, EMBEDDING_MODEL);
    for request in server.incoming_requests() {
        let path = request.url().split('?').next().unwrap_or("/").to_string();
        if request.method() == &Method::Get && path == "/healthz" {
            let chat_state = chat_status.snapshot();
            let embed_state = embedding_status.snapshot();
            let ready = chat_state["ready"] == true && embed_state["ready"] == true;
            answer(
                request,
                200,
                json!({"service": "harbor-model-api", "status": if ready { "ok" } else { "degraded" },
                "local_model_policy": "fixed", "ready": ready, "chat_model": CHAT_MODEL, "embedding_model": EMBEDDING_MODEL,
                "backend": {"kind": "n2_fixed", "ready": ready, "chat_model_loaded": chat_state["ready"], "embedding_model_loaded": embed_state["ready"]},
                "queues": {"chat": chat_state, "embedding": embed_state}}),
                true,
            );
        } else if !authorized(&request, &verifier) {
            error(request, 401, "UNAUTHORIZED", true);
        } else if request.method() == &Method::Post && path == "/v1/chat/completions" {
            enqueue(&chat_queue, request);
        } else if request.method() == &Method::Post && path == "/v1/embeddings" {
            enqueue(&embedding_queue, request);
        } else {
            error(request, 404, "ROUTE_NOT_FOUND", true);
        }
    }
    chat_status.stop();
    embedding_status.stop();
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("fixed model runtime: {error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokenizer() -> Tokenizer {
        let model = tokenizers::models::wordlevel::WordLevel::builder()
            .vocab([("[UNK]".to_owned(), 0)].into_iter().collect())
            .unk_token("[UNK]".to_owned())
            .build()
            .unwrap();
        let mut tokenizer = Tokenizer::new(model);
        tokenizer.with_pre_tokenizer(Some(tokenizers::pre_tokenizers::whitespace::Whitespace {}));
        tokenizer
    }

    #[test]
    fn embedding_limits_reject_invalid_batches_and_preserve_input_order() {
        let tokenizer = tokenizer();
        let (ids, count) =
            embedding_inputs(&json!({"input": ["one", "two words"]}), &tokenizer).unwrap();
        assert_eq!(ids.iter().map(Vec::len).collect::<Vec<_>>(), vec![1, 2]);
        assert_eq!(count, 3);
        for body in [
            json!({"input": " "}),
            json!({"input": []}),
            json!({"input": [1]}),
            json!({"input": vec!["text"; 33]}),
            json!({"input": "text", "dimensions": 384}),
            json!({"input": "text", "encoding_format": "base64"}),
            json!({"input": "word ".repeat(8193)}),
        ] {
            assert!(embedding_inputs(&body, &tokenizer).is_err());
        }
    }

    #[test]
    fn worker_output_requires_finite_normalized_768_dimensional_vectors() {
        let mut vector = vec![0.0; 768];
        vector[0] = 1.0;
        assert!(valid_embedding_vectors(&[json!(vector)], 1));
        assert!(!valid_embedding_vectors(&[json!(vector)], 2));
        assert!(!valid_embedding_vectors(&[json!(vec![0.0; 768])], 1));
        assert!(!valid_embedding_vectors(&[json!(vec![1.0; 768])], 1));
        assert!(!valid_embedding_vectors(&[json!(vec![1.0; 384])], 1));
        assert!(!valid_embedding_vectors(
            &[json!(vec![Value::Null; 768])],
            1
        ));
    }

    #[test]
    fn fixture_child_process() {
        if std::env::var_os("HARBOR_TEST_CHILD").is_some() {
            thread::sleep(Duration::from_secs(60));
        }
    }

    #[test]
    fn stopped_worker_is_reaped_before_resources_can_be_reused() {
        let state = WorkerStatus::default();
        for _ in 0..2 {
            let child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "tests::fixture_child_process", "--nocapture"])
                .env("HARBOR_TEST_CHILD", "1")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            *state.process.lock().unwrap() = Some(child);
            state.ready.store(true, Ordering::SeqCst);
            assert!(state.stop());
            assert!(state.process.lock().unwrap().is_none());
            assert_eq!(state.snapshot()["ready"], false);
            assert!(!state.quarantined.load(Ordering::SeqCst));
        }
    }

    #[test]
    fn bounded_chat_admission_enforces_fixed_model_and_queue_capacity() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        let (queue, receiver) = mpsc::sync_channel(CAPACITY);
        let worker = Worker {
            queue,
            status: Arc::new(WorkerStatus::default()),
        };
        let mut clients = Vec::new();
        for index in 0..CAPACITY + 1 {
            let url = url.clone();
            clients.push(thread::spawn(move || {
                Client::new()
                    .post(url)
                    .json(&json!({"id": index, "model": CHAT_MODEL}))
                    .send()
                    .unwrap()
                    .status()
                    .as_u16()
            }));
            accept_job(
                &worker,
                Incoming {
                    request: server.recv().unwrap(),
                    admitted: Instant::now(),
                },
                CHAT_MODEL,
            );
        }
        assert_eq!(clients.pop().unwrap().join().unwrap(), 429);
        assert_eq!(worker.status.queued.load(Ordering::SeqCst), CAPACITY);
        for index in 0..CAPACITY {
            let job = next_job(&receiver, &worker.status).unwrap();
            assert_eq!(job.body["id"], index);
            answer(job.request, 200, json!({}), true);
        }
        for client in clients {
            assert_eq!(client.join().unwrap(), 200);
        }
        let client = thread::spawn(move || {
            Client::new()
                .post(url)
                .json(&json!({"model": "unofficial-model"}))
                .send()
                .unwrap()
                .status()
                .as_u16()
        });
        accept_job(
            &worker,
            Incoming {
                request: server.recv().unwrap(),
                admitted: Instant::now(),
            },
            CHAT_MODEL,
        );
        assert_eq!(client.join().unwrap(), 403);
        assert_eq!(worker.status.queued.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn admission_wait_counts_toward_deadline_without_starting_inference() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        let client = thread::spawn(move || {
            Client::new()
                .post(url)
                .json(&json!({"model": CHAT_MODEL}))
                .send()
                .unwrap()
        });
        let (queue, receiver) = mpsc::sync_channel(CAPACITY);
        let worker = Worker {
            queue,
            status: Arc::new(WorkerStatus::default()),
        };
        accept_job(
            &worker,
            Incoming {
                request: server.recv().unwrap(),
                admitted: Instant::now() - DEADLINE,
            },
            CHAT_MODEL,
        );
        let response = client.join().unwrap();
        assert_eq!(response.status().as_u16(), 504);
        assert_eq!(response.headers()["X-Harbor-Execution-Stopped"], "true");
        assert!(receiver.try_recv().is_err());
        assert_eq!(worker.status.snapshot()["active"], false);
    }
}
