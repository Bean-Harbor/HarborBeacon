//! N2 fixed-model facade. Business prompts and routing remain in Beacon.
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use harborbeacon_local_agent::runtime::ai_execution::{
    ExecutionControl, ExecutionLease, ExecutionRegistry, ExecutionTicket, EXECUTION_CANCEL_PREFIX,
    EXECUTION_ID_HEADER,
};
use harborbeacon_local_agent::runtime::ai_resource_scheduler::{
    acquire_ai_resource_lease_until, ai_resource_scheduler_snapshot, AiLeaseQuarantineReason,
    AiWorkload,
};
use harborbeacon_local_agent::runtime::classifier_rpc::{
    execute_classifier_rpc, CLASSIFIER_MAX_BODY, CLASSIFIER_RPC_PATH,
};
use harborbeacon_local_agent::runtime::fixed_models::{
    CHAT_MODEL, CHAT_SHA256, EMBEDDING_MODEL, EMBEDDING_SHA256, TOKENIZER_SHA256,
};
use harborbeacon_local_agent::runtime::owned_ai_process::OwnedAiChild;
use harborbeacon_local_agent::service_auth::{model_api_verifier_token, VerifierTokens};
use reqwest::blocking::Client;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tiny_http::{Header, Method, Request, Response, Server};
use tokenizers::Tokenizer;

const DEADLINE: Duration = Duration::from_secs(90);
const CAPACITY: usize = 4;
const MAX_BODY: u64 = 1024 * 1024;
const CLASSIFIER_DEADLINE: Duration = Duration::from_secs(80);

#[derive(Default)]
struct WorkerStatus {
    ready: AtomicBool,
    quarantined: AtomicBool,
    queued: AtomicUsize,
    active: AtomicBool,
    last_queue_wait_ms: AtomicU64,
    completed: AtomicU64,
    process: Mutex<Option<OwnedAiChild>>,
}

impl WorkerStatus {
    fn snapshot(&self) -> Value {
        if let Ok(mut child) = self.process.try_lock() {
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
        if child.stop(Duration::ZERO).is_ok() {
            *guard = None;
            return true;
        }
        self.quarantined.store(true, Ordering::SeqCst);
        false
    }
}

struct StopMonitor {
    control: ExecutionControl,
    done: mpsc::Sender<()>,
    worker: Option<thread::JoinHandle<bool>>,
}

impl StopMonitor {
    fn start(status: Arc<WorkerStatus>, control: ExecutionControl) -> Result<Self, String> {
        let (done, completed) = mpsc::channel();
        let worker_control = control.clone();
        let worker = thread::Builder::new()
            .name("model-execution-stop".into())
            .spawn(move || loop {
                if worker_control.should_stop() {
                    return status.stop();
                }
                match completed.recv_timeout(Duration::from_millis(20)) {
                    Ok(()) => return true,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return status.stop(),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
            })
            .map_err(|_| "MODEL_STOP_MONITOR_UNAVAILABLE".to_string())?;
        Ok(Self {
            control,
            done,
            worker: Some(worker),
        })
    }

    fn finish(mut self) -> bool {
        let _ = self.done.send(());
        self.worker.take().unwrap().join().unwrap_or(false)
    }
}

impl Drop for StopMonitor {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            self.control.cancel_flag().store(true, Ordering::SeqCst);
            let _ = worker.join();
        }
    }
}

struct Job {
    request: Request,
    body: Value,
    admitted: Instant,
    execution: ExecutionTicket,
}
struct Worker {
    queue: SyncSender<Job>,
    status: Arc<WorkerStatus>,
}

struct Incoming {
    request: Request,
    admitted: Instant,
    execution: ExecutionTicket,
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

fn enqueue(
    sender: &SyncSender<Incoming>,
    request: Request,
    executions: &ExecutionRegistry,
    budget: Duration,
) {
    let admitted = Instant::now();
    let id = request
        .headers()
        .iter()
        .find(|header| header.field.equiv(EXECUTION_ID_HEADER))
        .map(|header| header.value.as_str());
    let execution = match executions.register(id, admitted + budget) {
        Ok(execution) => execution,
        Err(code) => {
            let status = match code {
                "EXECUTION_QUEUE_FULL" => 429,
                "EXECUTION_ID_CONFLICT" => 409,
                "INVALID_EXECUTION_ID" => 400,
                _ => 503,
            };
            error(request, status, code, code != "EXECUTION_ID_CONFLICT");
            return;
        }
    };
    if let Err(TrySendError::Full(incoming) | TrySendError::Disconnected(incoming)) = sender
        .try_send(Incoming {
            request,
            admitted,
            execution,
        })
    {
        execution_error(
            incoming.request,
            incoming.execution,
            429,
            "MODEL_QUEUE_FULL",
            true,
        );
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
    answer_with_id(request, status, body, stopped, None);
}

fn answer_with_id(request: Request, status: u16, body: Value, stopped: bool, id: Option<&str>) {
    let data = serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec());
    let mut response = Response::from_data(data)
        .with_status_code(status)
        .with_header(Header::from_bytes("Content-Type", "application/json").unwrap())
        .with_header(
            Header::from_bytes(
                "X-Harbor-Execution-Stopped",
                if stopped { "true" } else { "false" },
            )
            .unwrap(),
        );
    if let Some(id) = id {
        response.add_header(Header::from_bytes(EXECUTION_ID_HEADER, id).unwrap());
    }
    let _ = request.respond(response);
}

fn execution_answer(
    request: Request,
    ticket: ExecutionTicket,
    status: u16,
    body: Value,
    stopped: bool,
) {
    let id = ticket.id().to_string();
    ticket.finish(stopped);
    answer_with_id(request, status, body, stopped, Some(&id));
}

fn execution_error(
    request: Request,
    ticket: ExecutionTicket,
    status: u16,
    code: &str,
    stopped: bool,
) {
    execution_answer(
        request,
        ticket,
        status,
        json!({"ok": false, "error": {"code": code, "message": code}, "execution_stopped": stopped}),
        stopped,
    );
}

fn stopped_code(control: &ExecutionControl) -> &'static str {
    if control.is_cancelled() {
        "MODEL_EXECUTION_CANCELLED"
    } else {
        "MODEL_QUEUE_TIMEOUT"
    }
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
        execution,
    } = incoming;
    let control = execution.control();
    if control.should_stop() {
        execution_error(request, execution, 504, stopped_code(&control), true);
        return;
    }
    if worker.status.quarantined.load(Ordering::SeqCst) {
        execution_error(request, execution, 503, "MODEL_RUNTIME_QUARANTINED", true);
        return;
    }
    if request
        .body_length()
        .is_some_and(|size| size > MAX_BODY as usize)
    {
        execution_error(request, execution, 413, "REQUEST_TOO_LARGE", true);
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
        execution_error(request, execution, 413, "REQUEST_TOO_LARGE", true);
        return;
    }
    if control.should_stop() {
        execution_error(request, execution, 504, stopped_code(&control), true);
        return;
    }
    let Ok(mut body) = serde_json::from_slice::<Value>(&bytes) else {
        execution_error(request, execution, 400, "INVALID_JSON", true);
        return;
    };
    if !body.is_object()
        || body
            .get("model")
            .and_then(Value::as_str)
            .is_some_and(|name| name != model)
    {
        execution_error(request, execution, 403, "LOCAL_MODELS_FIXED", true);
        return;
    }
    if body.get("stream").and_then(Value::as_bool) == Some(true) {
        execution_error(request, execution, 400, "STREAMING_NOT_SUPPORTED", true);
        return;
    }
    body["model"] = json!(model);
    worker.status.queued.fetch_add(1, Ordering::SeqCst);
    match worker.queue.try_send(Job {
        request,
        body,
        admitted,
        execution,
    }) {
        Ok(()) => {}
        Err(TrySendError::Full(job) | TrySendError::Disconnected(job)) => {
            worker.status.queued.fetch_sub(1, Ordering::SeqCst);
            execution_error(job.request, job.execution, 429, "MODEL_QUEUE_FULL", true);
        }
    }
}

fn next_job(receiver: &Receiver<Job>, status: &WorkerStatus) -> Option<Job> {
    loop {
        let job = receiver.recv().ok()?;
        status.queued.fetch_sub(1, Ordering::SeqCst);
        let control = job.execution.control();
        if control.should_stop() {
            execution_error(
                job.request,
                job.execution,
                504,
                stopped_code(&control),
                true,
            );
        } else if status.quarantined.load(Ordering::SeqCst) {
            execution_error(
                job.request,
                job.execution,
                503,
                "MODEL_RUNTIME_QUARANTINED",
                true,
            );
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
    control: &ExecutionControl,
) -> Result<(), String> {
    let until = control.deadline();
    if control.should_stop() {
        return Err(stopped_code(control).into());
    }
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
    let mut command = Command::new(vendor.join("bin/llama-server"));
    command
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
        .stderr(Stdio::inherit());
    let child = OwnedAiChild::spawn(&mut command).map_err(|error| error.to_string())?;
    *status.process.lock().map_err(|error| error.to_string())? = Some(child);
    while !control.should_stop() {
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
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(DEADLINE)
        .build()
        .expect("chat HTTP client");
    while let Some(job) = next_job(&receiver, &status) {
        execute_chat_job(job, &status, |body, control| {
            start_chat(&root, &status, &client, control)?;
            if control.should_stop() {
                return Err(stopped_code(control).into());
            }
            let remaining = control.deadline().saturating_duration_since(Instant::now());
            let response = client
                .post("http://127.0.0.1:8793/v1/chat/completions")
                .bearer_auth(&model_api_verifier_token()?.current)
                .timeout(remaining)
                .json(body)
                .send()
                .map_err(|error| error.to_string())?;
            let code = response.status().as_u16();
            let body = response
                .json::<Value>()
                .map_err(|error| error.to_string())?;
            Ok((code, body))
        });
    }
    status.stop();
}

fn execute_chat_job(
    job: Job,
    status: &Arc<WorkerStatus>,
    execute: impl FnOnce(&Value, &ExecutionControl) -> Result<(u16, Value), String>,
) {
    let control = job.execution.control();
    let lease = match acquire_ai_resource_lease_until(
        AiWorkload::Llm,
        control.deadline(),
        control.cancel_flag(),
    ) {
        Ok(lease) => ExecutionLease::new(lease),
        Err(error) => {
            status.active.store(false, Ordering::SeqCst);
            execution_error(job.request, job.execution, 503, error.code(), true);
            return;
        }
    };
    if control.should_stop() {
        lease.confirm_stopped();
        status.active.store(false, Ordering::SeqCst);
        execution_error(
            job.request,
            job.execution,
            504,
            stopped_code(&control),
            true,
        );
        return;
    }
    let monitor = match StopMonitor::start(status.clone(), control.clone()) {
        Ok(monitor) => monitor,
        Err(code) => {
            lease.confirm_stopped();
            status.active.store(false, Ordering::SeqCst);
            execution_error(job.request, job.execution, 503, &code, true);
            return;
        }
    };
    job.execution.mark_started();
    let result = execute(&job.body, &control);
    let monitor_stopped = monitor.finish();
    let cancelled = control.should_stop();
    let stopped = if result.is_err() || cancelled || !monitor_stopped {
        status.stop() && monitor_stopped
    } else {
        true
    };
    if stopped {
        lease.confirm_stopped();
    } else {
        lease.quarantine(AiLeaseQuarantineReason::ProcessExitUnconfirmed);
    }
    status.active.store(false, Ordering::SeqCst);
    status.completed.fetch_add(1, Ordering::SeqCst);
    match result {
        Ok((code, body)) if !cancelled && stopped => {
            execution_answer(job.request, job.execution, code, body, true);
        }
        _ => {
            let code = if cancelled {
                stopped_code(&control)
            } else {
                "MODEL_EXECUTION_FAILED"
            };
            execution_error(job.request, job.execution, 503, code, stopped);
        }
    }
}

struct EmbeddingProcess {
    input: std::process::ChildStdin,
    output: Receiver<String>,
}

fn start_embedding(
    root: &Path,
    status: &WorkerStatus,
    until: Instant,
    control: Option<&ExecutionControl>,
) -> Result<EmbeddingProcess, String> {
    if !status.stop() {
        return Err("MODEL_RUNTIME_QUARANTINED".into());
    }
    if Instant::now() >= until || control.is_some_and(ExecutionControl::should_stop) {
        return Err("MODEL_QUEUE_TIMEOUT".into());
    }
    let mut command = Command::new("/usr/bin/python3");
    command
        .arg("/usr/lib/harboros-model-runtime/n2_embedding_worker.py")
        .arg(root.join("embedding.onnx"))
        .env_remove("LD_LIBRARY_PATH")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    let mut child = OwnedAiChild::spawn(&mut command).map_err(|error| error.to_string())?;
    let input = child
        .child_mut()
        .stdin
        .take()
        .ok_or("embedding stdin unavailable")?;
    let output = child
        .child_mut()
        .stdout
        .take()
        .ok_or("embedding stdout unavailable")?;
    *status.process.lock().map_err(|error| error.to_string())? = Some(child);
    if control.is_some_and(ExecutionControl::should_stop) {
        status.stop();
        return Err("MODEL_EXECUTION_CANCELLED".into());
    }
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
    let mut process = start_embedding(&root, &status, Instant::now() + DEADLINE, None).ok();
    if process.is_none() {
        status.stop();
    }
    while let Some(job) = next_job(&receiver, &status) {
        let control = job.execution.control();
        let (batch, count) = match embedding_inputs(&job.body, &tokenizer) {
            Ok(input) => input,
            Err(message) => {
                execution_answer(
                    job.request,
                    job.execution,
                    400,
                    json!({"error": {"code": "INVALID_INPUT", "message": message}}),
                    true,
                );
                status.active.store(false, Ordering::SeqCst);
                continue;
            }
        };
        let monitor = match StopMonitor::start(status.clone(), control.clone()) {
            Ok(monitor) => monitor,
            Err(code) => {
                status.active.store(false, Ordering::SeqCst);
                execution_error(job.request, job.execution, 503, &code, true);
                continue;
            }
        };
        job.execution.mark_started();
        let result = (|| -> Result<Value, String> {
            if process.is_none() {
                process = Some(start_embedding(
                    &root,
                    &status,
                    control.deadline(),
                    Some(&control),
                )?);
            }
            if control.should_stop() {
                return Err(stopped_code(&control).into());
            }
            let worker = process.as_mut().unwrap();
            let line = serde_json::to_string(&json!({"input_ids": batch})).unwrap();
            writeln!(worker.input, "{line}").map_err(|error| error.to_string())?;
            worker.input.flush().map_err(|error| error.to_string())?;
            let remaining = control.deadline().saturating_duration_since(Instant::now());
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
        let monitor_stopped = monitor.finish();
        let cancelled = control.should_stop();
        status.active.store(false, Ordering::SeqCst);
        status.completed.fetch_add(1, Ordering::SeqCst);
        match result {
            Ok(body) if !cancelled && monitor_stopped => {
                execution_answer(job.request, job.execution, 200, body, true);
            }
            _ => {
                let stopped = status.stop() && monitor_stopped;
                process = None;
                let code = if cancelled {
                    stopped_code(&control)
                } else {
                    "MODEL_EXECUTION_FAILED"
                };
                execution_error(job.request, job.execution, 503, code, stopped);
            }
        }
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

fn classifier_admission() -> SyncSender<Incoming> {
    let (sender, receiver) = mpsc::sync_channel::<Incoming>(CAPACITY);
    thread::spawn(move || {
        for incoming in receiver {
            let Incoming {
                mut request,
                execution,
                ..
            } = incoming;
            let control = execution.control();
            if control.should_stop() {
                execution_error(request, execution, 504, stopped_code(&control), true);
                continue;
            }
            let Some(length) = request.body_length().map(|length| length as u64) else {
                execution_error(request, execution, 411, "CONTENT_LENGTH_REQUIRED", true);
                continue;
            };
            if length > CLASSIFIER_MAX_BODY {
                execution_error(request, execution, 413, "REQUEST_TOO_LARGE", true);
                continue;
            }
            execution.mark_started();
            match execute_classifier_rpc(request.as_reader(), length, &control) {
                Ok(body) => execution_answer(request, execution, 200, body, true),
                Err(failure) => {
                    execution_answer(
                        request,
                        execution,
                        503,
                        json!({"ok": false, "error": {"code": failure.code, "message": failure.message},
                            "execution_stopped": failure.exit_confirmed}),
                        failure.exit_confirmed,
                    );
                }
            }
        }
    });
    sender
}

fn execution_control_route(request: Request, path: &str, registry: &ExecutionRegistry) {
    let suffix = path
        .strip_prefix(EXECUTION_CANCEL_PREFIX)
        .unwrap_or_default();
    let result = if request.method() == &Method::Post {
        suffix.strip_suffix("/cancel").map(|id| registry.cancel(id))
    } else if request.method() == &Method::Get && !suffix.contains('/') {
        Some(registry.status(suffix).ok_or("EXECUTION_NOT_FOUND"))
    } else {
        None
    };
    match result {
        Some(Ok(body)) => {
            let stopped = body["execution_stopped"] == true;
            answer(request, 200, body, stopped);
        }
        Some(Err(code)) => {
            let status = match code {
                "EXECUTION_NOT_FOUND" => 404,
                "INVALID_EXECUTION_ID" => 400,
                _ => 503,
            };
            error(request, status, code, false);
        }
        None => error(request, 404, "ROUTE_NOT_FOUND", false),
    }
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
    // Accepted bodies are read off the main loop. Rejected requests can still block
    // on tiny_http's synchronous body drain, so this is not a transport deadline.
    let chat_queue = admission(chat, CHAT_MODEL);
    let embedding_queue = admission(embedding, EMBEDDING_MODEL);
    let classifier_queue = classifier_admission();
    let executions = ExecutionRegistry::new(64);
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
                "queues": {"chat": chat_state, "embedding": embed_state},
                "ai_resources": ai_resource_scheduler_snapshot(), "executions": executions.snapshot()}),
                true,
            );
        } else if !authorized(&request, &verifier) {
            error(request, 401, "UNAUTHORIZED", true);
        } else if path.starts_with(EXECUTION_CANCEL_PREFIX) {
            execution_control_route(request, &path, &executions);
        } else if request.method() == &Method::Post && path == "/v1/chat/completions" {
            enqueue(&chat_queue, request, &executions, DEADLINE);
        } else if request.method() == &Method::Post && path == "/v1/embeddings" {
            enqueue(&embedding_queue, request, &executions, DEADLINE);
        } else if request.method() == &Method::Post && path == CLASSIFIER_RPC_PATH {
            enqueue(&classifier_queue, request, &executions, CLASSIFIER_DEADLINE);
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

    fn incoming(request: Request, admitted: Instant) -> Incoming {
        Incoming {
            request,
            admitted,
            execution: ExecutionRegistry::new(1)
                .register(None, admitted + DEADLINE)
                .unwrap(),
        }
    }

    fn fixture_process() -> OwnedAiChild {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "tests::fixture_child_process", "--nocapture"])
            .env("HARBOR_TEST_CHILD", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        OwnedAiChild::spawn(&mut command).unwrap()
    }

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
        if let Ok(url) = std::env::var("HARBOR_TEST_HTTP_CALLER_URL") {
            Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(30))
                .build()
                .unwrap()
                .post(url)
                .body("{}")
                .send()
                .unwrap();
        } else if std::env::var_os("HARBOR_TEST_HTTP_CHILD").is_some() {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            println!("{}", listener.local_addr().unwrap());
            std::io::stdout().flush().unwrap();
            let until = Instant::now() + Duration::from_secs(60);
            let mut connections = Vec::new();
            while Instant::now() < until {
                match listener.accept() {
                    Ok((connection, _)) => connections.push(connection),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("fixture accept: {error}"),
                }
            }
        } else if std::env::var_os("HARBOR_TEST_CHILD").is_some() {
            thread::sleep(Duration::from_secs(60));
        }
    }

    #[test]
    fn stopped_worker_is_reaped_before_resources_can_be_reused() {
        let state = WorkerStatus::default();
        for _ in 0..2 {
            let child = fixture_process();
            *state.process.lock().unwrap() = Some(child);
            state.ready.store(true, Ordering::SeqCst);
            assert!(state.stop());
            assert!(state.process.lock().unwrap().is_none());
            assert_eq!(state.snapshot()["ready"], false);
            assert!(!state.quarantined.load(Ordering::SeqCst));
        }
    }

    #[test]
    fn worker_snapshot_does_not_wait_for_process_cleanup() {
        let state = Arc::new(WorkerStatus::default());
        let held_process = state.process.lock().unwrap();
        let snapshot_state = state.clone();
        let (sender, receiver) = mpsc::channel();
        let reader = thread::spawn(move || sender.send(snapshot_state.snapshot()).unwrap());
        let response = receiver.recv_timeout(Duration::from_millis(250));
        drop(held_process);
        reader.join().unwrap();
        assert!(response.is_ok(), "health must not wait for process cleanup");
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
                incoming(server.recv().unwrap(), Instant::now()),
                CHAT_MODEL,
            );
        }
        assert_eq!(clients.pop().unwrap().join().unwrap(), 429);
        assert_eq!(worker.status.queued.load(Ordering::SeqCst), CAPACITY);
        for index in 0..CAPACITY {
            let job = next_job(&receiver, &worker.status).unwrap();
            assert_eq!(job.body["id"], index);
            execution_answer(job.request, job.execution, 200, json!({}), true);
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
            incoming(server.recv().unwrap(), Instant::now()),
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
            incoming(server.recv().unwrap(), Instant::now() - DEADLINE),
            CHAT_MODEL,
        );
        let response = client.join().unwrap();
        assert_eq!(response.status().as_u16(), 504);
        assert_eq!(response.headers()["X-Harbor-Execution-Stopped"], "true");
        assert!(receiver.try_recv().is_err());
        assert_eq!(worker.status.snapshot()["active"], false);
    }

    #[test]
    fn registered_queue_cancellation_never_starts_inference() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        let id = uuid::Uuid::new_v4().to_string();
        let sent_id = id.clone();
        let client = thread::spawn(move || {
            Client::new()
                .post(url)
                .header(EXECUTION_ID_HEADER, sent_id)
                .json(&json!({"model": CHAT_MODEL}))
                .send()
                .unwrap()
        });
        let registry = ExecutionRegistry::new(4);
        let (sender, receiver) = mpsc::sync_channel(CAPACITY);
        enqueue(&sender, server.recv().unwrap(), &registry, DEADLINE);
        assert_eq!(registry.status(&id).unwrap()["state"], "queued");
        assert_eq!(registry.cancel(&id).unwrap()["execution_stopped"], false);
        let (queue, jobs) = mpsc::sync_channel(CAPACITY);
        let worker = Worker {
            queue,
            status: Arc::new(WorkerStatus::default()),
        };
        accept_job(&worker, receiver.recv().unwrap(), CHAT_MODEL);
        let response = client.join().unwrap();
        assert_eq!(response.status().as_u16(), 504);
        assert_eq!(response.headers()[EXECUTION_ID_HEADER], id.as_str());
        assert!(jobs.try_recv().is_err());
        assert_eq!(registry.status(&id).unwrap()["execution_stopped"], true);
    }

    #[test]
    fn cancellation_monitor_reaps_current_child_without_affecting_next_execution() {
        let registry = ExecutionRegistry::new(2);
        let state = Arc::new(WorkerStatus::default());
        let ticket = registry.register(None, Instant::now() + DEADLINE).unwrap();
        let id = ticket.id().to_string();
        *state.process.lock().unwrap() = Some(fixture_process());
        let monitor = StopMonitor::start(state.clone(), ticket.control()).unwrap();
        ticket.mark_started();
        assert_eq!(registry.cancel(&id).unwrap()["execution_stopped"], false);
        let until = Instant::now() + Duration::from_secs(10);
        while state.process.lock().unwrap().is_some() {
            assert!(Instant::now() < until, "cancel must reap the owned child");
            thread::sleep(Duration::from_millis(10));
        }
        assert!(monitor.finish());
        ticket.finish(true);
        let next = registry.register(None, Instant::now() + DEADLINE).unwrap();
        *state.process.lock().unwrap() = Some(fixture_process());
        let next_monitor = StopMonitor::start(state.clone(), next.control()).unwrap();
        registry.cancel(&id).unwrap();
        thread::sleep(Duration::from_millis(50));
        assert!(!next.control().is_cancelled());
        assert!(state
            .process
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_none());
        assert!(next_monitor.finish());
        assert!(state.stop());
        next.finish(true);
    }

    #[test]
    fn chat_execution_owns_the_lease_after_the_http_caller_disconnects() {
        use harborbeacon_local_agent::runtime::ai_resource_scheduler::{
            acquire_ai_resource_lease, ai_resource_workload_snapshot,
        };
        let server = Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        let client = thread::spawn(move || {
            Client::new()
                .post(url)
                .body("{}")
                .timeout(Duration::from_millis(200))
                .send()
        });
        let registry = ExecutionRegistry::new(1);
        let ticket = registry.register(None, Instant::now() + DEADLINE).unwrap();
        let id = ticket.id().to_string();
        let job = Job {
            request: server.recv().unwrap(),
            body: json!({}),
            admitted: Instant::now(),
            execution: ticket,
        };
        let state = Arc::new(WorkerStatus::default());
        let (entered, running) = mpsc::channel();
        let (complete, completed) = mpsc::channel();
        let worker = thread::spawn(move || {
            execute_chat_job(job, &state, |_, _| {
                entered.send(()).unwrap();
                completed.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok((200, json!({})))
            })
        });
        running.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(client.join().unwrap().is_err());
        assert_eq!(
            ai_resource_workload_snapshot(AiWorkload::Llm)["holder_workload"],
            "llm"
        );
        assert_eq!(registry.status(&id).unwrap()["execution_stopped"], false);
        complete.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(registry.status(&id).unwrap()["execution_stopped"], true);
        drop(acquire_ai_resource_lease(AiWorkload::CatRecordingVerifier).unwrap());
    }

    #[test]
    fn terminating_the_caller_process_does_not_release_the_runtime_execution_lease() {
        use harborbeacon_local_agent::runtime::ai_resource_scheduler::ai_resource_workload_snapshot;
        let server = Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "tests::fixture_child_process", "--nocapture"])
            .env("HARBOR_TEST_HTTP_CALLER_URL", url)
            .env_remove("HARBOR_TEST_CHILD")
            .env_remove("HARBOR_TEST_HTTP_CHILD")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut caller = OwnedAiChild::spawn(&mut command).unwrap();
        let registry = ExecutionRegistry::new(1);
        let execution = registry.register(None, Instant::now() + DEADLINE).unwrap();
        let id = execution.id().to_string();
        let job = Job {
            request: server
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap(),
            body: json!({}),
            admitted: Instant::now(),
            execution,
        };
        let state = Arc::new(WorkerStatus::default());
        let (entered, running) = mpsc::channel();
        let (complete, completed) = mpsc::channel();
        let execution_worker = thread::spawn(move || {
            execute_chat_job(job, &state, |_, _| {
                entered.send(()).unwrap();
                completed.recv_timeout(Duration::from_secs(10)).unwrap();
                Ok((200, json!({})))
            })
        });
        running.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(caller.try_wait().unwrap().is_none());
        caller.stop(Duration::ZERO).unwrap();
        assert!(
            caller.try_wait().unwrap().is_some(),
            "caller must be reaped"
        );
        assert_eq!(
            ai_resource_workload_snapshot(AiWorkload::Llm)["holder_workload"],
            "llm"
        );
        assert_eq!(registry.status(&id).unwrap()["state"], "running");
        assert_eq!(registry.status(&id).unwrap()["execution_stopped"], false);

        let (granted, grant) = mpsc::channel();
        let classifier = thread::spawn(move || {
            let cancelled = AtomicBool::new(false);
            let lease = acquire_ai_resource_lease_until(
                AiWorkload::CatRecordingVerifier,
                Instant::now() + Duration::from_secs(5),
                &cancelled,
            )
            .unwrap();
            granted.send(()).unwrap();
            drop(lease);
        });
        let until = Instant::now() + Duration::from_secs(2);
        while ai_resource_workload_snapshot(AiWorkload::CatRecordingVerifier)["queue_depth"] != 1 {
            assert!(
                Instant::now() < until,
                "classifier must wait for runtime completion"
            );
            thread::sleep(Duration::from_millis(5));
        }
        assert!(grant.recv_timeout(Duration::from_millis(50)).is_err());
        complete.send(()).unwrap();
        execution_worker.join().unwrap();
        assert_eq!(registry.status(&id).unwrap()["execution_stopped"], true);
        grant.recv_timeout(Duration::from_secs(5)).unwrap();
        classifier.join().unwrap();
    }

    #[test]
    fn active_chat_cancellation_interrupts_blocking_http_and_then_releases_the_lease() {
        use harborbeacon_local_agent::runtime::ai_resource_scheduler::{
            acquire_ai_resource_lease, ai_resource_workload_snapshot,
        };
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", "tests::fixture_child_process", "--nocapture"])
            .env("HARBOR_TEST_HTTP_CHILD", "1")
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = OwnedAiChild::spawn(&mut command).unwrap();
        let output = child.child_mut().stdout.take().unwrap();
        let (address_sender, address_receiver) = mpsc::channel();
        let reader = thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let line = line.unwrap();
                if let Some(address) = line
                    .split_whitespace()
                    .find(|word| word.starts_with("127.0.0.1:"))
                {
                    address_sender.send(address.to_string()).unwrap();
                    return;
                }
            }
        });
        let address = address_receiver
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        reader.join().unwrap();
        let server = Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        let client = thread::spawn(move || {
            Client::new()
                .post(url)
                .body("{}")
                .timeout(Duration::from_secs(10))
                .send()
                .unwrap()
        });
        let registry = ExecutionRegistry::new(1);
        let ticket = registry.register(None, Instant::now() + DEADLINE).unwrap();
        let id = ticket.id().to_string();
        let job = Job {
            request: server.recv().unwrap(),
            body: json!({}),
            admitted: Instant::now(),
            execution: ticket,
        };
        let state = Arc::new(WorkerStatus::default());
        *state.process.lock().unwrap() = Some(child);
        let worker_state = state.clone();
        let (entered, running) = mpsc::channel();
        let worker = thread::spawn(move || {
            execute_chat_job(job, &worker_state, |_, _| {
                entered.send(()).unwrap();
                Client::builder()
                    .no_proxy()
                    .timeout(Duration::from_secs(20))
                    .build()
                    .unwrap()
                    .post(format!("http://{address}/v1/chat/completions"))
                    .body("{}")
                    .send()
                    .map_err(|error| error.to_string())?;
                Err("fixture must never answer".to_string())
            })
        });
        running.recv_timeout(Duration::from_secs(5)).unwrap();
        thread::sleep(Duration::from_millis(100));
        assert_eq!(
            ai_resource_workload_snapshot(AiWorkload::Llm)["holder_workload"],
            "llm"
        );
        let cancelled_at = Instant::now();
        assert_eq!(registry.cancel(&id).unwrap()["execution_stopped"], false);
        let response = client.join().unwrap();
        worker.join().unwrap();
        assert!(cancelled_at.elapsed() < Duration::from_secs(8));
        assert_eq!(response.status().as_u16(), 503);
        assert_eq!(response.headers()["X-Harbor-Execution-Stopped"], "true");
        assert!(state.process.lock().unwrap().is_none());
        assert_eq!(registry.status(&id).unwrap()["execution_stopped"], true);
        drop(acquire_ai_resource_lease(AiWorkload::CatRecordingVerifier).unwrap());
    }

    #[test]
    fn duplicate_execution_id_does_not_acknowledge_the_existing_execution_as_stopped() {
        let server = Server::http("127.0.0.1:0").unwrap();
        let url = format!("http://{}", server.server_addr());
        let registry = ExecutionRegistry::new(2);
        let existing = registry.register(None, Instant::now() + DEADLINE).unwrap();
        existing.mark_started();
        let id = existing.id().to_string();
        let sent_id = id.clone();
        let client = thread::spawn(move || {
            Client::new()
                .post(url)
                .header(EXECUTION_ID_HEADER, sent_id)
                .body("{}")
                .send()
                .unwrap()
        });
        let (sender, receiver) = mpsc::sync_channel(CAPACITY);
        enqueue(&sender, server.recv().unwrap(), &registry, DEADLINE);
        let response = client.join().unwrap();
        assert_eq!(response.status().as_u16(), 409);
        assert_eq!(response.headers()["X-Harbor-Execution-Stopped"], "false");
        assert!(receiver.try_recv().is_err());
        assert_eq!(registry.status(&id).unwrap()["state"], "running");
        existing.finish(true);
    }
}
