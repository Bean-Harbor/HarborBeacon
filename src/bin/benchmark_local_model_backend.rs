use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use harborbeacon_local_agent::scripts::model_benchmark::{
    benchmark_run_role, build_model_benchmark_gate, default_chat_specs, default_embedding_cases,
    evaluate_embedding_benchmark, evaluate_embedding_case, is_output_clean, summarize_chat_probes,
    trim_excerpt, ChatProbeResult, ColdStartProbeResult, EmbeddingProbeResult, HealthProbeResult,
    LocalModelBenchmarkReport, BENCHMARK_RUN_ROLE_BUILDER_COMPATIBILITY,
    BENCHMARK_RUN_ROLE_TARGET_RUNTIME_PROMOTION, MODEL_BENCHMARK_CONTRACT,
    MODEL_BENCHMARK_SCHEMA_VERSION,
};
use reqwest::blocking::Client;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use serde_json::{json, Value};

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:4176/v1";
const DEFAULT_HEALTHZ_URL: &str = "http://127.0.0.1:4176/healthz";
const DEFAULT_BIND: &str = "127.0.0.1:4176";
const DEFAULT_BACKEND: &str = "candle";
const DEFAULT_CHAT_MODEL: &str = "harbor-local-chat";
const DEFAULT_EMBEDDING_MODEL: &str = "harbor-local-embed";
const DEFAULT_OUTPUT: &str = "local-model-benchmark-report.json";
const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 30_000;
const DEFAULT_STARTUP_TIMEOUT_MS: u64 = 45_000;
const DEFAULT_POLL_INTERVAL_MS: u64 = 500;

#[derive(Debug, Clone)]
struct Cli {
    output: PathBuf,
    spawn_binary: Option<PathBuf>,
    base_url: String,
    healthz_url: String,
    bind: String,
    run_role: Option<String>,
    backend: String,
    api_key: Option<String>,
    chat_model: String,
    embedding_model: String,
    upstream_base_url: Option<String>,
    candle_chat_model_id: Option<String>,
    candle_embedding_model_id: Option<String>,
    request_timeout_ms: u64,
    startup_timeout_ms: u64,
    poll_interval_ms: u64,
}

impl Default for Cli {
    fn default() -> Self {
        Self {
            output: PathBuf::from(DEFAULT_OUTPUT),
            spawn_binary: None,
            base_url: DEFAULT_BASE_URL.to_string(),
            healthz_url: DEFAULT_HEALTHZ_URL.to_string(),
            bind: DEFAULT_BIND.to_string(),
            run_role: None,
            backend: DEFAULT_BACKEND.to_string(),
            api_key: std::env::var("HARBOR_MODEL_API_BENCHMARK_API_KEY").ok(),
            chat_model: DEFAULT_CHAT_MODEL.to_string(),
            embedding_model: DEFAULT_EMBEDDING_MODEL.to_string(),
            upstream_base_url: std::env::var("HARBOR_MODEL_API_UPSTREAM_BASE_URL").ok(),
            candle_chat_model_id: std::env::var("HARBOR_MODEL_API_CANDLE_CHAT_MODEL_ID")
                .ok()
                .or_else(|| std::env::var("HARBOR_MODEL_API_CANDLE_MODEL_ID").ok()),
            candle_embedding_model_id: std::env::var("HARBOR_MODEL_API_CANDLE_EMBEDDING_MODEL_ID")
                .ok(),
            request_timeout_ms: DEFAULT_REQUEST_TIMEOUT_MS,
            startup_timeout_ms: DEFAULT_STARTUP_TIMEOUT_MS,
            poll_interval_ms: DEFAULT_POLL_INTERVAL_MS,
        }
    }
}

impl Cli {
    fn parse() -> Self {
        let args = std::env::args().skip(1).collect::<Vec<_>>();
        if matches!(args.first().map(String::as_str), Some("--help" | "-h")) {
            print_usage();
            std::process::exit(0);
        }

        let mut cli = Self::default();
        let mut index = 0;
        while index < args.len() {
            let arg = &args[index];
            match arg.as_str() {
                "--output" => cli.output = PathBuf::from(take_value(&args, &mut index, "--output")),
                value if value.starts_with("--output=") => {
                    cli.output = PathBuf::from(&value["--output=".len()..]);
                }
                "--spawn-binary" => {
                    cli.spawn_binary = Some(PathBuf::from(take_value(
                        &args,
                        &mut index,
                        "--spawn-binary",
                    )))
                }
                value if value.starts_with("--spawn-binary=") => {
                    cli.spawn_binary = Some(PathBuf::from(&value["--spawn-binary=".len()..]));
                }
                "--base-url" => cli.base_url = take_value(&args, &mut index, "--base-url"),
                value if value.starts_with("--base-url=") => {
                    cli.base_url = value["--base-url=".len()..].to_string();
                }
                "--healthz-url" => cli.healthz_url = take_value(&args, &mut index, "--healthz-url"),
                value if value.starts_with("--healthz-url=") => {
                    cli.healthz_url = value["--healthz-url=".len()..].to_string();
                }
                "--bind" => cli.bind = take_value(&args, &mut index, "--bind"),
                value if value.starts_with("--bind=") => {
                    cli.bind = value["--bind=".len()..].to_string();
                }
                "--run-role" => cli.run_role = Some(take_value(&args, &mut index, "--run-role")),
                value if value.starts_with("--run-role=") => {
                    cli.run_role = Some(value["--run-role=".len()..].to_string());
                }
                "--backend" => cli.backend = take_value(&args, &mut index, "--backend"),
                value if value.starts_with("--backend=") => {
                    cli.backend = value["--backend=".len()..].to_string();
                }
                "--api-key" => cli.api_key = Some(take_value(&args, &mut index, "--api-key")),
                value if value.starts_with("--api-key=") => {
                    cli.api_key = Some(value["--api-key=".len()..].to_string());
                }
                "--chat-model" => cli.chat_model = take_value(&args, &mut index, "--chat-model"),
                value if value.starts_with("--chat-model=") => {
                    cli.chat_model = value["--chat-model=".len()..].to_string();
                }
                "--embedding-model" => {
                    cli.embedding_model = take_value(&args, &mut index, "--embedding-model")
                }
                value if value.starts_with("--embedding-model=") => {
                    cli.embedding_model = value["--embedding-model=".len()..].to_string();
                }
                "--upstream-base-url" => {
                    cli.upstream_base_url =
                        Some(take_value(&args, &mut index, "--upstream-base-url"))
                }
                value if value.starts_with("--upstream-base-url=") => {
                    cli.upstream_base_url = Some(value["--upstream-base-url=".len()..].to_string());
                }
                "--candle-chat-model-id" => {
                    cli.candle_chat_model_id =
                        Some(take_value(&args, &mut index, "--candle-chat-model-id"))
                }
                value if value.starts_with("--candle-chat-model-id=") => {
                    cli.candle_chat_model_id =
                        Some(value["--candle-chat-model-id=".len()..].to_string());
                }
                "--candle-embedding-model-id" => {
                    cli.candle_embedding_model_id =
                        Some(take_value(&args, &mut index, "--candle-embedding-model-id"))
                }
                value if value.starts_with("--candle-embedding-model-id=") => {
                    cli.candle_embedding_model_id =
                        Some(value["--candle-embedding-model-id=".len()..].to_string());
                }
                "--request-timeout-ms" => {
                    cli.request_timeout_ms = parse_u64(
                        &take_value(&args, &mut index, "--request-timeout-ms"),
                        "--request-timeout-ms",
                    )
                }
                value if value.starts_with("--request-timeout-ms=") => {
                    cli.request_timeout_ms = parse_u64(
                        &value["--request-timeout-ms=".len()..],
                        "--request-timeout-ms",
                    );
                }
                "--startup-timeout-ms" => {
                    cli.startup_timeout_ms = parse_u64(
                        &take_value(&args, &mut index, "--startup-timeout-ms"),
                        "--startup-timeout-ms",
                    )
                }
                value if value.starts_with("--startup-timeout-ms=") => {
                    cli.startup_timeout_ms = parse_u64(
                        &value["--startup-timeout-ms=".len()..],
                        "--startup-timeout-ms",
                    );
                }
                "--poll-interval-ms" => {
                    cli.poll_interval_ms = parse_u64(
                        &take_value(&args, &mut index, "--poll-interval-ms"),
                        "--poll-interval-ms",
                    )
                }
                value if value.starts_with("--poll-interval-ms=") => {
                    cli.poll_interval_ms =
                        parse_u64(&value["--poll-interval-ms=".len()..], "--poll-interval-ms");
                }
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                value if value.starts_with('-') => fail(&format!("unknown flag: {value}")),
                value => fail(&format!("unexpected positional argument: {value}")),
            }
            index += 1;
        }

        if cli.spawn_binary.is_some() {
            let (base_url, healthz_url) = urls_from_bind(&cli.bind);
            cli.base_url = base_url;
            cli.healthz_url = healthz_url;
        }

        cli
    }
}

fn main() {
    let cli = Cli::parse();
    let client = Client::builder()
        .timeout(Duration::from_millis(cli.request_timeout_ms))
        .build()
        .unwrap_or_else(|error| fail(&format!("failed to build benchmark HTTP client: {error}")));

    let mut spawned = cli
        .spawn_binary
        .as_ref()
        .map(|binary| spawn_service(binary, &cli));

    let cold_start = if spawned.is_some() {
        measure_cold_start(&client, &cli, spawned.as_mut())
    } else {
        ColdStartProbeResult {
            measured: false,
            ready_within_timeout: false,
            timeout_ms: cli.startup_timeout_ms,
            attempts: 0,
            elapsed_ms: None,
            note: Some(
                "attached mode records builder compatibility evidence only; use --spawn-binary for target-runtime promotion evidence"
                    .to_string(),
            ),
        }
    };

    let health = probe_healthz(&client, &cli.healthz_url);
    let chat = summarize_chat_probes(run_chat_benchmarks(&client, &cli));
    let embeddings = run_embedding_benchmark(&client, &cli);
    let run_role = resolve_run_role(cli.run_role.as_deref(), cli.spawn_binary.is_some());
    let gate = build_model_benchmark_gate(
        &run_role,
        &cli.backend,
        &cold_start,
        &health,
        &chat,
        &embeddings,
    );

    let report = LocalModelBenchmarkReport {
        schema_version: MODEL_BENCHMARK_SCHEMA_VERSION,
        contract: MODEL_BENCHMARK_CONTRACT.to_string(),
        mode: if cli.spawn_binary.is_some() {
            "spawned".to_string()
        } else {
            "attached".to_string()
        },
        run_role: run_role.clone(),
        backend: cli.backend.clone(),
        base_url: cli.base_url.clone(),
        healthz_url: cli.healthz_url.clone(),
        generated_at_utc: current_timestamp_utc(),
        cold_start,
        health,
        chat,
        embeddings,
        gate,
    };

    if let Some(parent) = cli.output.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent).unwrap_or_else(|error| {
                fail(&format!("failed to create output directory: {error}"))
            });
        }
    }
    std::fs::write(
        &cli.output,
        serde_json::to_string_pretty(&report).expect("serialize benchmark report"),
    )
    .unwrap_or_else(|error| fail(&format!("failed to write benchmark report: {error}")));

    println!(
        "benchmark report written to {} (promotable={})",
        cli.output.display(),
        report.gate.promotable
    );

    if !report.gate.promotable
        && run_role.eq_ignore_ascii_case(BENCHMARK_RUN_ROLE_TARGET_RUNTIME_PROMOTION)
    {
        std::process::exit(1);
    }
}

fn run_chat_benchmarks(client: &Client, cli: &Cli) -> Vec<ChatProbeResult> {
    default_chat_specs()
        .into_iter()
        .map(|spec| {
            let started = Instant::now();
            match post_openai_chat(
                client,
                &cli.base_url,
                cli.api_key.as_deref(),
                &cli.chat_model,
                spec.prompt,
            ) {
                Ok(text) => {
                    let semantic_ok = spec
                        .expected_any
                        .iter()
                        .find(|needle| text.contains(**needle))
                        .map(|value| (*value).to_string());
                    let output_clean = is_output_clean(&text);
                    ChatProbeResult {
                        name: spec.name.to_string(),
                        prompt: spec.prompt.to_string(),
                        ok: semantic_ok.is_some() && output_clean,
                        semantic_ok: semantic_ok.is_some(),
                        output_clean,
                        latency_ms: Some(started.elapsed().as_millis() as u64),
                        matched_expectation: semantic_ok,
                        response_excerpt: Some(trim_excerpt(&text, 80)),
                        error: None,
                    }
                }
                Err(error) => ChatProbeResult {
                    name: spec.name.to_string(),
                    prompt: spec.prompt.to_string(),
                    ok: false,
                    semantic_ok: false,
                    output_clean: false,
                    latency_ms: Some(started.elapsed().as_millis() as u64),
                    matched_expectation: None,
                    response_excerpt: None,
                    error: Some(error),
                },
            }
        })
        .collect()
}

fn run_embedding_benchmark(
    client: &Client,
    cli: &Cli,
) -> harborbeacon_local_agent::scripts::model_benchmark::EmbeddingBenchmarkSummary {
    let mut vectors = HashMap::<String, Vec<f32>>::new();
    let mut probes = Vec::new();

    let probe_texts = [
        ("probe-flower", "樱花观测记录与花期照片归档"),
        ("probe-snapshot", "摄像头抓拍使用说明与图片归档策略"),
        ("probe-record", "录像保存时长与导出步骤"),
    ];

    for (name, input) in probe_texts {
        let started = Instant::now();
        match post_openai_embedding(
            client,
            &cli.base_url,
            cli.api_key.as_deref(),
            &cli.embedding_model,
            input,
        ) {
            Ok(vector) => {
                let non_zero_dimensions = vector
                    .iter()
                    .filter(|value| value.abs() > f32::EPSILON)
                    .count();
                vectors.insert(input.to_string(), vector.clone());
                probes.push(EmbeddingProbeResult {
                    name: name.to_string(),
                    input: input.to_string(),
                    ok: !vector.is_empty() && non_zero_dimensions > 0,
                    latency_ms: Some(started.elapsed().as_millis() as u64),
                    dimensions: Some(vector.len()),
                    non_zero_dimensions,
                    error: None,
                });
            }
            Err(error) => probes.push(EmbeddingProbeResult {
                name: name.to_string(),
                input: input.to_string(),
                ok: false,
                latency_ms: Some(started.elapsed().as_millis() as u64),
                dimensions: None,
                non_zero_dimensions: 0,
                error: Some(error),
            }),
        }
    }

    let cases = default_embedding_cases();
    for case in &cases {
        ensure_embedding_vector(client, cli, &case.query, &mut vectors);
        for candidate in &case.candidates {
            ensure_embedding_vector(client, cli, &candidate.text, &mut vectors);
        }
    }

    let retrieval_cases = cases
        .iter()
        .map(|case| evaluate_embedding_case(case, &vectors))
        .collect::<Vec<_>>();

    evaluate_embedding_benchmark(probes, retrieval_cases)
}

fn ensure_embedding_vector(
    client: &Client,
    cli: &Cli,
    text: &str,
    vectors: &mut HashMap<String, Vec<f32>>,
) {
    if vectors.contains_key(text) {
        return;
    }
    if let Ok(vector) = post_openai_embedding(
        client,
        &cli.base_url,
        cli.api_key.as_deref(),
        &cli.embedding_model,
        text,
    ) {
        vectors.insert(text.to_string(), vector);
    }
}

fn post_openai_chat(
    client: &Client,
    base_url: &str,
    api_key: Option<&str>,
    model: &str,
    prompt: &str,
) -> Result<String, String> {
    let payload = json!({
        "model": model,
        "temperature": 0.0,
        "max_tokens": 24,
        "messages": [
            {
                "role": "user",
                "content": prompt,
            }
        ]
    });
    let response = json_request(
        client,
        "POST",
        &format!("{}/chat/completions", base_url.trim_end_matches('/')),
        api_key,
        Some(payload),
    )?;
    extract_message_text(&response).ok_or_else(|| {
        format!(
            "chat response did not include assistant text: {}",
            trim_excerpt(&response.to_string(), 120)
        )
    })
}

fn resolve_run_role(explicit: Option<&str>, spawned: bool) -> String {
    let fallback = benchmark_run_role(spawned);
    match explicit.unwrap_or(fallback).trim() {
        value if value.eq_ignore_ascii_case(BENCHMARK_RUN_ROLE_BUILDER_COMPATIBILITY) => {
            BENCHMARK_RUN_ROLE_BUILDER_COMPATIBILITY.to_string()
        }
        value if value.eq_ignore_ascii_case(BENCHMARK_RUN_ROLE_TARGET_RUNTIME_PROMOTION) => {
            BENCHMARK_RUN_ROLE_TARGET_RUNTIME_PROMOTION.to_string()
        }
        other => fail(&format!(
            "unsupported run role '{other}'; expected {BENCHMARK_RUN_ROLE_BUILDER_COMPATIBILITY} or {BENCHMARK_RUN_ROLE_TARGET_RUNTIME_PROMOTION}"
        )),
    }
}

fn post_openai_embedding(
    client: &Client,
    base_url: &str,
    api_key: Option<&str>,
    model: &str,
    input: &str,
) -> Result<Vec<f32>, String> {
    let payload = json!({
        "model": model,
        "input": input,
    });
    let response = json_request(
        client,
        "POST",
        &format!("{}/embeddings", base_url.trim_end_matches('/')),
        api_key,
        Some(payload),
    )?;
    response
        .get("data")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("embedding"))
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_f64)
                .map(|value| value as f32)
                .collect::<Vec<_>>()
        })
        .filter(|embedding| !embedding.is_empty())
        .ok_or_else(|| {
            format!(
                "embedding response did not include data[0].embedding: {}",
                trim_excerpt(&response.to_string(), 120)
            )
        })
}

fn measure_cold_start(
    client: &Client,
    cli: &Cli,
    mut spawned: Option<&mut SpawnedService>,
) -> ColdStartProbeResult {
    let started = Instant::now();
    let mut attempts = 0usize;
    let mut last_note = None;
    let timeout = Duration::from_millis(cli.startup_timeout_ms);

    while started.elapsed() <= timeout {
        attempts += 1;
        if let Some(service) = spawned.as_mut() {
            if let Some(status) = service.child.try_wait().unwrap_or(None) {
                return ColdStartProbeResult {
                    measured: true,
                    ready_within_timeout: false,
                    timeout_ms: cli.startup_timeout_ms,
                    attempts,
                    elapsed_ms: Some(started.elapsed().as_millis() as u64),
                    note: Some(format!("spawned service exited early with status {status}")),
                };
            }
        }

        let probe = probe_healthz(client, &cli.healthz_url);
        if probe.ok && probe.ready {
            return ColdStartProbeResult {
                measured: true,
                ready_within_timeout: true,
                timeout_ms: cli.startup_timeout_ms,
                attempts,
                elapsed_ms: Some(started.elapsed().as_millis() as u64),
                note: probe.note,
            };
        }
        last_note = probe.error.or(probe.note);
        thread::sleep(Duration::from_millis(cli.poll_interval_ms));
    }

    ColdStartProbeResult {
        measured: true,
        ready_within_timeout: false,
        timeout_ms: cli.startup_timeout_ms,
        attempts,
        elapsed_ms: Some(started.elapsed().as_millis() as u64),
        note: last_note,
    }
}

fn probe_healthz(client: &Client, url: &str) -> HealthProbeResult {
    let response = match client.get(url).send() {
        Ok(response) => response,
        Err(error) => {
            return HealthProbeResult {
                ok: false,
                ready: false,
                http_status: None,
                service: None,
                status: None,
                backend_kind: None,
                backend_ready: None,
                note: None,
                error: Some(format!("healthz request failed: {error}")),
            }
        }
    };

    let http_status = response.status().as_u16();
    let body = match response.text() {
        Ok(body) => body,
        Err(error) => {
            return HealthProbeResult {
                ok: false,
                ready: false,
                http_status: Some(http_status),
                service: None,
                status: None,
                backend_kind: None,
                backend_ready: None,
                note: None,
                error: Some(format!("failed to read healthz response: {error}")),
            }
        }
    };

    let payload = match serde_json::from_str::<Value>(&body) {
        Ok(payload) => payload,
        Err(error) => {
            return HealthProbeResult {
                ok: false,
                ready: false,
                http_status: Some(http_status),
                service: None,
                status: None,
                backend_kind: None,
                backend_ready: None,
                note: None,
                error: Some(format!("healthz response was not valid JSON: {error}")),
            }
        }
    };

    HealthProbeResult {
        ok: http_status < 400,
        ready: payload
            .get("ready")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        http_status: Some(http_status),
        service: payload
            .get("service")
            .and_then(Value::as_str)
            .map(str::to_string),
        status: payload
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_string),
        backend_kind: payload
            .get("backend")
            .and_then(|value| value.get("kind"))
            .and_then(Value::as_str)
            .map(str::to_string),
        backend_ready: payload
            .get("backend")
            .and_then(|value| value.get("ready"))
            .and_then(Value::as_bool),
        note: payload
            .get("note")
            .and_then(Value::as_str)
            .map(str::to_string),
        error: None,
    }
}

fn json_request(
    client: &Client,
    method: &str,
    url: &str,
    api_key: Option<&str>,
    body: Option<Value>,
) -> Result<Value, String> {
    let mut request = match method {
        "GET" => client.get(url),
        "POST" => client.post(url),
        other => return Err(format!("unsupported HTTP method: {other}")),
    };
    if let Some(api_key) = api_key.filter(|value| !value.trim().is_empty()) {
        request = request.header(AUTHORIZATION, format!("Bearer {api_key}"));
    }
    request = request.header(CONTENT_TYPE, "application/json");
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request
        .send()
        .map_err(|error| format!("request to {url} failed: {error}"))?;
    let status = response.status();
    let text = response
        .text()
        .map_err(|error| format!("failed to read response from {url}: {error}"))?;
    if !status.is_success() {
        return Err(format!(
            "request to {url} returned {status}: {}",
            trim_excerpt(&text, 160)
        ));
    }
    serde_json::from_str(&text)
        .map_err(|error| format!("response from {url} was not valid JSON: {error}"))
}

fn extract_message_text(payload: &Value) -> Option<String> {
    let content = payload
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(|item| item.get("message"))
        .and_then(|message| message.get("content"))?;

    if let Some(text) = content.as_str() {
        return Some(text.trim().to_string());
    }

    let parts = content.as_array()?;
    let joined = parts
        .iter()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("");
    if joined.trim().is_empty() {
        None
    } else {
        Some(joined.trim().to_string())
    }
}

fn spawn_service(binary: &Path, cli: &Cli) -> SpawnedService {
    if !binary.exists() {
        fail(&format!("spawn binary not found: {}", binary.display()));
    }

    let mut command = build_spawn_command(binary, cli);
    let child = command
        .spawn()
        .unwrap_or_else(|error| fail(&format!("failed to spawn harbor-model-api: {error}")));
    SpawnedService { child }
}

fn build_spawn_command(binary: &Path, cli: &Cli) -> Command {
    let mut command = Command::new(binary);
    command
        .arg("--bind")
        .arg(&cli.bind)
        .arg("--backend")
        .arg(&cli.backend)
        .arg("--chat-model")
        .arg(&cli.chat_model)
        .arg("--embedding-model")
        .arg(&cli.embedding_model)
        .arg("--request-timeout-ms")
        .arg(cli.request_timeout_ms.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    if let Some(upstream) = cli.upstream_base_url.as_deref() {
        command.arg("--upstream-base-url").arg(upstream);
    }
    if let Some(model_id) = cli.candle_chat_model_id.as_deref() {
        command.arg("--candle-chat-model-id").arg(model_id);
    }
    if let Some(model_id) = cli.candle_embedding_model_id.as_deref() {
        command.arg("--candle-embedding-model-id").arg(model_id);
    }
    if let Some(api_key) = cli
        .api_key
        .as_deref()
        .filter(|value| !value.trim().is_empty())
    {
        command.env("HARBOR_MODEL_API_TOKEN", api_key);
    }

    command
}

fn urls_from_bind(bind: &str) -> (String, String) {
    let bind = bind
        .trim()
        .trim_start_matches("http://")
        .trim_end_matches('/');
    (
        format!("http://{bind}/v1"),
        format!("http://{bind}/healthz"),
    )
}

fn current_timestamp_utc() -> String {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => format!("unix-epoch-ms:{}", duration.as_millis()),
        Err(_) => "unix-epoch-ms:0".to_string(),
    }
}

fn parse_u64(value: &str, flag: &str) -> u64 {
    value
        .parse::<u64>()
        .unwrap_or_else(|error| fail(&format!("invalid value for {flag}: {error}")))
}

fn take_value(args: &[String], index: &mut usize, flag: &str) -> String {
    *index += 1;
    if *index >= args.len() {
        fail(&format!("missing value for {flag}"));
    }
    args[*index].clone()
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(2);
}

fn print_usage() {
    eprintln!(
        "Usage: benchmark-local-model-backend [--spawn-binary PATH] [--backend candle|openai_proxy] [--bind ADDR] [--run-role builder-compatibility|target-runtime-promotion] [--base-url URL] [--healthz-url URL] [--chat-model NAME] [--embedding-model NAME] [--upstream-base-url URL] [--candle-chat-model-id ID] [--candle-embedding-model-id ID] [--api-key TOKEN] [--request-timeout-ms N] [--startup-timeout-ms N] [--poll-interval-ms N] [--output PATH]"
    );
}

struct SpawnedService {
    child: Child,
}

impl Drop for SpawnedService {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::{build_spawn_command, Cli};
    use std::ffi::OsStr;
    use std::path::Path;
    use std::process::Command;

    fn command_env<'a>(command: &'a Command, name: &str) -> Option<Option<&'a OsStr>> {
        command
            .get_envs()
            .find(|(key, _)| *key == OsStr::new(name))
            .map(|(_, value)| value)
    }

    #[test]
    fn spawned_model_api_receives_only_explicit_benchmark_api_key() {
        let mut cli = Cli::default();
        cli.api_key = None;
        let without_key = build_spawn_command(Path::new("harbor-model-api"), &cli);
        assert_eq!(command_env(&without_key, "HARBOR_MODEL_API_TOKEN"), None);

        let explicit = "benchmark_token_0123456789abcdef0123456789abcdef";
        cli.api_key = Some(explicit.to_string());
        let with_key = build_spawn_command(Path::new("harbor-model-api"), &cli);
        assert_eq!(
            command_env(&with_key, "HARBOR_MODEL_API_TOKEN"),
            Some(Some(OsStr::new(explicit)))
        );
    }
}
