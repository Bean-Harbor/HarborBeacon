use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use harborbeacon_local_agent::connectors::ai_provider::{
    OpenAiCompatibleConfig, OpenAiCompatibleVisionClient, VisionSummaryRequest,
};
use harborbeacon_local_agent::runtime::family_timeline::{
    build_family_timeline_digest_from_query, FamilyTimelineQueryOptions,
};
use harborbeacon_local_agent::runtime::model_center::VlmSummaryExecution;
use harborbeacon_local_agent::runtime::vision_event::{
    analyze_snapshot_file, attach_vlm_summary_to_event, build_local_vision_family_summary,
    build_local_vision_notification_intent, list_recent_local_vision_events_default,
    local_vision_event_store_stats_default, LocalSnapshotAnalysisInput, LocalVisionAnalyzerResult,
    LocalVisionEvent, StoredLocalVisionEvent,
};
use reqwest::blocking::Client;
use serde::Serialize;
use serde_json::{json, Value};

const RETAINED_RUN_LIMIT: usize = 120;
const PERSISTENT_FRAME_COPY_ATTEMPTS: usize = 8;
const PERSISTENT_FRAME_COPY_RETRY_MS: u64 = 150;

#[derive(Debug, Clone)]
struct Cli {
    camera_id: String,
    rtsp_url: Option<String>,
    source_secret_ref: Option<String>,
    snapshot_url: Option<String>,
    beacon_url: String,
    output_dir: PathBuf,
    duration_seconds: u64,
    interval_seconds: u64,
    phase_offset_ms: u64,
    no_post: bool,
    fixture: bool,
    ffmpeg: String,
    capture_mode: CaptureMode,
    capture_root: PathBuf,
    max_frame_age_ms: u64,
    decode_backend: String,
    analyzer_command: Option<String>,
    model_path: String,
    label_path: String,
    provider: String,
    redact_paths: bool,
    vlm_enrich: bool,
    vlm_api_base: String,
    vlm_model: String,
    vlm_api_key: String,
    vlm_prompt: String,
    vlm_sample_every: u64,
    vlm_max_samples: usize,
    vlm_queue_lock_path: Option<PathBuf>,
    vlm_global_max_samples: Option<usize>,
    vlm_trigger_policy: VlmTriggerPolicy,
}

#[derive(Debug, Clone, PartialEq)]
enum CaptureMode {
    OneshotFfmpeg,
    PersistentFfmpeg,
    LocalRestream,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VlmTriggerPolicy {
    DetectionOrPeriodic,
    Periodic,
}

impl VlmTriggerPolicy {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "detection_or_periodic" => Ok(Self::DetectionOrPeriodic),
            "periodic" => Ok(Self::Periodic),
            other => Err(format!(
                "invalid --vlm-trigger-policy {other}; expected periodic or detection_or_periodic"
            )),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::DetectionOrPeriodic => "detection_or_periodic",
            Self::Periodic => "periodic",
        }
    }
}

struct VlmQueueGuard {
    lock_path: PathBuf,
}

impl Drop for VlmQueueGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.lock_path);
    }
}

enum VlmQueueDecision {
    Run {
        sample_index: usize,
        _guard: Option<VlmQueueGuard>,
    },
    Skip,
    Degraded(String),
}

#[derive(Debug, Clone)]
struct CaptureResult {
    capture_ms: u64,
    capture_read_ms: Option<u64>,
    frame_age_ms: Option<u64>,
    stream_uptime_ms: Option<u64>,
    reconnect_count: u64,
    decode_backend: String,
}

struct PersistentCaptureWorker {
    camera_id: String,
    ffmpeg: String,
    rtsp_url: String,
    latest_path: PathBuf,
    metadata_path: PathBuf,
    child: Option<Child>,
    started_at: Instant,
    reconnect_count: u64,
    max_frame_age_ms: u64,
    decode_backend: String,
}

#[derive(Debug, Serialize)]
struct SmokeReport {
    ok: bool,
    camera_id: String,
    capture_mode: String,
    scheduler: String,
    phase_offset_ms: u64,
    max_frame_age_ms: u64,
    runtime_probe: Value,
    duration_seconds: u64,
    interval_seconds: u64,
    run_log_path: Option<String>,
    runs_retained: usize,
    full_runs_redacted: bool,
    runs: Vec<SmokeRun>,
    evidence: SmokeEvidence,
    summary: SmokeSummary,
}

#[derive(Debug, Clone, Serialize)]
struct SmokeRun {
    iteration: usize,
    ok: bool,
    snapshot_path: Option<String>,
    event: Option<LocalVisionEvent>,
    ingest_http_status: Option<u16>,
    capture_ms: u64,
    detector_ms: Option<u64>,
    analyze_ms: u64,
    event_ingest_ms: Option<u64>,
    total_ms: u64,
    capture_mode: String,
    capture_read_ms: Option<u64>,
    frame_age_ms: Option<u64>,
    stream_uptime_ms: Option<u64>,
    reconnect_count: u64,
    decode_backend: String,
    provider: Option<String>,
    detection_count: Option<usize>,
    vlm_status: Option<String>,
    vlm_ms: Option<u64>,
    vlm_error: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Serialize, Default)]
struct SmokeEvidence {
    metadata_only: bool,
    secret_scan: String,
    retained_run_count: usize,
    latest_retained_event_id: Option<String>,
    recent_store_event_count: usize,
    event_store_stats: Option<Value>,
    family_digest: Option<Value>,
    latest_family_summary: Option<Value>,
    notification_intent: Option<Value>,
    evidence_errors: Vec<String>,
}

#[derive(Debug, Serialize, Default)]
struct SmokeSummary {
    total: usize,
    passed: usize,
    failed: usize,
    average_total_ms: u64,
    p95_total_ms: u64,
    average_detector_ms: u64,
    p95_detector_ms: u64,
    average_event_ingest_ms: u64,
    p95_event_ingest_ms: u64,
    average_capture_read_ms: u64,
    average_frame_age_ms: u64,
    p95_capture_read_ms: u64,
    p95_frame_age_ms: u64,
    max_total_ms: u64,
    under_2s: usize,
    under_5s: usize,
    detection_runs: usize,
    detection_count: usize,
    vlm_total: usize,
    vlm_passed: usize,
    vlm_degraded: usize,
    average_vlm_ms: u64,
    p95_vlm_ms: u64,
}

fn main() {
    let cli = Cli::parse();
    if let Err(error) = fs::create_dir_all(&cli.output_dir) {
        fail(&format!(
            "failed to create output dir {}: {error}",
            cli.output_dir.display()
        ));
    }

    let runtime_probe = probe_official_vision_runtime();
    let mut capture_worker = match create_capture_worker(&cli) {
        Ok(worker) => worker,
        Err(error) => fail(&error),
    };
    let start = Instant::now();
    let end = if cli.duration_seconds == 0 {
        None
    } else {
        Some(start + Duration::from_secs(cli.duration_seconds))
    };
    let mut next_deadline = start + Duration::from_millis(cli.phase_offset_ms);
    let interval = Duration::from_secs(cli.interval_seconds.max(1));
    let run_log_path = cli.output_dir.join("local-vision-smoke-runs.jsonl");
    let mut run_log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&run_log_path)
        .unwrap_or_else(|error| {
            fail(&format!(
                "failed to open smoke run log {}: {error}",
                run_log_path.display()
            ))
        });
    let mut runs = VecDeque::with_capacity(RETAINED_RUN_LIMIT);
    let mut summary_accumulator = SmokeSummaryAccumulator::default();
    let mut vlm_sample_count = 0usize;
    let mut iteration = 0usize;
    loop {
        if let Some(end) = end {
            if next_deadline >= end {
                break;
            }
            sleep_until(next_deadline);
        } else if cli.phase_offset_ms > 0 {
            sleep_until(next_deadline);
        }
        iteration += 1;
        let run = run_once(
            &cli,
            iteration,
            &runtime_probe,
            capture_worker.as_mut(),
            &mut vlm_sample_count,
        );
        if let Err(error) = append_smoke_run(&mut run_log, &run) {
            fail(&format!(
                "failed to append smoke run log {}: {error}",
                run_log_path.display()
            ));
        }
        summary_accumulator.observe(&run);
        if runs.len() == RETAINED_RUN_LIMIT {
            runs.pop_front();
        }
        runs.push_back(run);
        if end.is_none() {
            break;
        }
        next_deadline += interval;
    }

    let summary = summary_accumulator.finish();
    let retained_runs = runs.into_iter().collect::<Vec<_>>();
    let evidence = build_smoke_evidence(&retained_runs);
    let report = SmokeReport {
        ok: summary.failed == 0,
        camera_id: cli.camera_id.clone(),
        capture_mode: cli.capture_mode.as_str().to_string(),
        scheduler: "fixed-rate".to_string(),
        phase_offset_ms: cli.phase_offset_ms,
        max_frame_age_ms: cli.max_frame_age_ms,
        runtime_probe,
        duration_seconds: cli.duration_seconds,
        interval_seconds: cli.interval_seconds,
        run_log_path: Some(
            report_snapshot_path(&cli, &run_log_path)
                .unwrap_or_else(|| "[redacted-local-path]".to_string()),
        ),
        runs_retained: retained_runs.len(),
        full_runs_redacted: true,
        runs: retained_runs,
        evidence,
        summary,
    };
    let report_path = cli.output_dir.join("local-vision-smoke-report.json");
    let text = serde_json::to_string_pretty(&report).unwrap_or_else(|error| {
        fail(&format!("failed to serialize smoke report: {error}"));
    });
    if let Err(error) = fs::write(&report_path, &text) {
        fail(&format!(
            "failed to write smoke report {}: {error}",
            report_path.display()
        ));
    }
    println!("{text}");
    if !report.ok {
        std::process::exit(1);
    }
}

fn run_once(
    cli: &Cli,
    iteration: usize,
    runtime_probe: &Value,
    capture_worker: Option<&mut PersistentCaptureWorker>,
    vlm_sample_count: &mut usize,
) -> SmokeRun {
    let total_started = Instant::now();
    let snapshot_path =
        cli.output_dir
            .join(format!("snapshot-{:04}-{}.jpg", iteration, epoch_millis()));
    let capture = capture_snapshot(cli, &snapshot_path, capture_worker);
    let capture_result = match capture {
        Ok(result) => result,
        Err(error) => {
            let capture_ms = total_started.elapsed().as_millis() as u64;
            return SmokeRun {
                iteration,
                ok: false,
                snapshot_path: None,
                event: None,
                ingest_http_status: None,
                capture_ms,
                detector_ms: None,
                analyze_ms: 0,
                event_ingest_ms: None,
                total_ms: total_started.elapsed().as_millis() as u64,
                capture_mode: cli.capture_mode.as_str().to_string(),
                capture_read_ms: None,
                frame_age_ms: None,
                stream_uptime_ms: None,
                reconnect_count: 0,
                decode_backend: cli.decode_backend.clone(),
                provider: None,
                detection_count: None,
                vlm_status: None,
                vlm_ms: None,
                vlm_error: None,
                error: Some(error),
            };
        }
    };
    let capture_ms = capture_result.capture_ms;
    let capture_mode = cli.capture_mode.as_str().to_string();
    let decode_backend = capture_result.decode_backend.clone();
    if let Err(error) = validate_snapshot_file(&snapshot_path) {
        return SmokeRun {
            iteration,
            ok: false,
            snapshot_path: None,
            event: None,
            ingest_http_status: None,
            capture_ms,
            detector_ms: None,
            analyze_ms: 0,
            event_ingest_ms: None,
            total_ms: total_started.elapsed().as_millis() as u64,
            capture_mode,
            capture_read_ms: capture_result.capture_read_ms,
            frame_age_ms: capture_result.frame_age_ms,
            stream_uptime_ms: capture_result.stream_uptime_ms,
            reconnect_count: capture_result.reconnect_count,
            decode_backend,
            provider: None,
            detection_count: None,
            vlm_status: None,
            vlm_ms: None,
            vlm_error: None,
            error: Some(error),
        };
    }

    let analyzer_result = match run_analyzer(cli, &snapshot_path) {
        Ok(result) => result,
        Err(error) => {
            return SmokeRun {
                iteration,
                ok: false,
                snapshot_path: report_snapshot_path(cli, &snapshot_path),
                event: None,
                ingest_http_status: None,
                capture_ms,
                detector_ms: None,
                analyze_ms: 0,
                event_ingest_ms: None,
                total_ms: total_started.elapsed().as_millis() as u64,
                capture_mode,
                capture_read_ms: capture_result.capture_read_ms,
                frame_age_ms: capture_result.frame_age_ms,
                stream_uptime_ms: capture_result.stream_uptime_ms,
                reconnect_count: capture_result.reconnect_count,
                decode_backend,
                provider: None,
                detection_count: None,
                vlm_status: None,
                vlm_ms: None,
                vlm_error: None,
                error: Some(sanitize_sensitive(&error)),
            };
        }
    };
    let detector_ms = analyzer_result
        .as_ref()
        .and_then(|result| result.command_latency_ms.or(result.latency_ms));
    let provider = analyzer_result
        .as_ref()
        .and_then(|result| result.provider.clone());
    let detection_count = analyzer_result
        .as_ref()
        .map(|result| result.detections.len());

    let analyze_started = Instant::now();
    let analyzer = if runtime_probe["official_status"] == json!("missing") {
        "official-vision-runtime-missing+cpu-snapshot-fallback"
    } else {
        "official-vision-runtime-unverified+cpu-snapshot-fallback"
    };
    let event = analyze_snapshot_file(LocalSnapshotAnalysisInput {
        camera_id: cli.camera_id.clone(),
        snapshot_path: snapshot_path.clone(),
        analyzer: Some(analyzer.to_string()),
        latency_ms: None,
        analyzer_result,
        metrics: json!({
            "runtime_probe": runtime_probe,
            "capture_ms": capture_ms,
            "capture_mode": capture_mode.clone(),
            "capture_read_ms": capture_result.capture_read_ms,
            "frame_age_ms": capture_result.frame_age_ms,
            "stream_uptime_ms": capture_result.stream_uptime_ms,
            "reconnect_count": capture_result.reconnect_count,
            "decode_backend": decode_backend.clone(),
            "scheduler": "fixed-rate",
            "phase_offset_ms": cli.phase_offset_ms,
            "detector_ms": detector_ms,
            "smoke_iteration": iteration,
        }),
    });
    let analyze_ms = analyze_started.elapsed().as_millis() as u64;
    let mut event = match event {
        Ok(mut event) => {
            event.latency_ms = total_started.elapsed().as_millis() as u64;
            event
        }
        Err(error) => {
            return SmokeRun {
                iteration,
                ok: false,
                snapshot_path: report_snapshot_path(cli, &snapshot_path),
                event: None,
                ingest_http_status: None,
                capture_ms,
                detector_ms,
                analyze_ms,
                event_ingest_ms: None,
                total_ms: total_started.elapsed().as_millis() as u64,
                capture_mode,
                capture_read_ms: capture_result.capture_read_ms,
                frame_age_ms: capture_result.frame_age_ms,
                stream_uptime_ms: capture_result.stream_uptime_ms,
                reconnect_count: capture_result.reconnect_count,
                decode_backend,
                provider,
                detection_count,
                vlm_status: None,
                vlm_ms: None,
                vlm_error: None,
                error: Some(sanitize_sensitive(&error)),
            };
        }
    };
    event.latency_ms = total_started.elapsed().as_millis() as u64;
    let mut vlm_status = None;
    let mut vlm_ms = None;
    let mut vlm_error = None;
    if should_run_vlm(cli, iteration, detection_count, *vlm_sample_count) {
        match claim_vlm_slot(cli, *vlm_sample_count) {
            VlmQueueDecision::Run {
                sample_index,
                _guard,
            } => {
                *vlm_sample_count = vlm_sample_count.saturating_add(1);
                let started = Instant::now();
                let execution = run_vlm_summary_for_event(cli, &snapshot_path, &event);
                let elapsed_ms = started.elapsed().as_millis() as u64;
                vlm_status = Some(execution.status.clone());
                vlm_ms = Some(elapsed_ms);
                if !execution.available {
                    vlm_error = Some(sanitize_sensitive(&execution.summary));
                }
                event = attach_vlm_summary_to_event(
                    event,
                    execution,
                    elapsed_ms,
                    json!({
                        "runtime_id": "local-openai-compatible-vlm",
                        "model_id": cli.vlm_model.clone(),
                        "sample_policy": {
                            "sample_every": cli.vlm_sample_every,
                            "max_samples": cli.vlm_max_samples,
                            "sample_index": sample_index,
                            "trigger_policy": cli.vlm_trigger_policy.as_str(),
                            "queue_mode": cli.vlm_queue_mode(),
                            "global_max_samples": cli.vlm_global_max_samples,
                        }
                    }),
                );
                event.latency_ms = total_started.elapsed().as_millis() as u64;
            }
            VlmQueueDecision::Skip => {}
            VlmQueueDecision::Degraded(error) => {
                vlm_status = Some("degraded".to_string());
                vlm_ms = Some(0);
                vlm_error = Some(sanitize_sensitive(&error));
            }
        }
    }

    let ingest_http_status = if cli.no_post {
        None
    } else {
        let ingest_started = Instant::now();
        match post_event(&cli.beacon_url, &event) {
            Ok(status) => {
                let event_ingest_ms = ingest_started.elapsed().as_millis() as u64;
                event.metrics.as_object_mut().map(|map| {
                    map.insert("event_ingest_ms".to_string(), json!(event_ingest_ms));
                });
                Some(status)
            }
            Err(error) => {
                return SmokeRun {
                    iteration,
                    ok: false,
                    snapshot_path: report_snapshot_path(cli, &snapshot_path),
                    event: Some(event),
                    ingest_http_status: None,
                    capture_ms,
                    detector_ms,
                    analyze_ms,
                    event_ingest_ms: Some(ingest_started.elapsed().as_millis() as u64),
                    total_ms: total_started.elapsed().as_millis() as u64,
                    capture_mode,
                    capture_read_ms: capture_result.capture_read_ms,
                    frame_age_ms: capture_result.frame_age_ms,
                    stream_uptime_ms: capture_result.stream_uptime_ms,
                    reconnect_count: capture_result.reconnect_count,
                    decode_backend,
                    provider,
                    detection_count,
                    vlm_status,
                    vlm_ms,
                    vlm_error,
                    error: Some(sanitize_sensitive(&error)),
                };
            }
        }
    };
    let event_ingest_ms = event.metrics.get("event_ingest_ms").and_then(Value::as_u64);
    let ok = ingest_http_status
        .map(|status| status == 200)
        .unwrap_or(true);
    SmokeRun {
        iteration,
        ok,
        snapshot_path: report_snapshot_path(cli, &snapshot_path),
        event: Some(event),
        ingest_http_status,
        capture_ms,
        detector_ms,
        analyze_ms,
        event_ingest_ms,
        total_ms: total_started.elapsed().as_millis() as u64,
        capture_mode,
        capture_read_ms: capture_result.capture_read_ms,
        frame_age_ms: capture_result.frame_age_ms,
        stream_uptime_ms: capture_result.stream_uptime_ms,
        reconnect_count: capture_result.reconnect_count,
        decode_backend,
        provider,
        detection_count,
        vlm_status,
        vlm_ms,
        vlm_error,
        error: if ok {
            None
        } else {
            Some("event ingest returned non-200 status".to_string())
        },
    }
}

fn should_run_vlm(
    cli: &Cli,
    iteration: usize,
    detection_count: Option<usize>,
    sample_count: usize,
) -> bool {
    if !cli.vlm_enrich || sample_count >= cli.vlm_max_samples {
        return false;
    }
    if cli.vlm_trigger_policy == VlmTriggerPolicy::DetectionOrPeriodic
        && detection_count.unwrap_or(0) > 0
    {
        return true;
    }
    let every = cli.vlm_sample_every.max(1) as usize;
    match cli.vlm_trigger_policy {
        VlmTriggerPolicy::DetectionOrPeriodic => iteration == 1 || iteration % every == 0,
        VlmTriggerPolicy::Periodic => iteration > 0 && iteration % every == 0,
    }
}

fn claim_vlm_slot(cli: &Cli, sample_count: usize) -> VlmQueueDecision {
    let Some(lock_path) = cli.vlm_queue_lock_path.as_ref() else {
        return VlmQueueDecision::Run {
            sample_index: sample_count.saturating_add(1),
            _guard: None,
        };
    };
    let global_max = cli.vlm_global_max_samples.unwrap_or(cli.vlm_max_samples);
    if global_max == 0 {
        return VlmQueueDecision::Skip;
    }
    let guard = match acquire_vlm_queue_lock(lock_path) {
        Ok(guard) => guard,
        Err(error) => return VlmQueueDecision::Degraded(error),
    };
    let counter_path = vlm_queue_count_path(lock_path);
    let current_count = fs::read_to_string(&counter_path)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(0);
    if current_count >= global_max {
        return VlmQueueDecision::Skip;
    }
    let next_count = current_count.saturating_add(1);
    if let Err(error) = fs::write(&counter_path, next_count.to_string()) {
        return VlmQueueDecision::Degraded(format!("failed to update VLM queue counter: {error}"));
    }
    VlmQueueDecision::Run {
        sample_index: next_count,
        _guard: Some(guard),
    }
}

fn acquire_vlm_queue_lock(lock_path: &Path) -> Result<VlmQueueGuard, String> {
    if let Some(parent) = lock_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create VLM queue lock dir {}: {error}",
                parent.display()
            )
        })?;
    }
    let started = Instant::now();
    loop {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(lock_path)
        {
            Ok(_) => {
                return Ok(VlmQueueGuard {
                    lock_path: lock_path.to_path_buf(),
                })
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                remove_stale_vlm_queue_lock(lock_path);
                if started.elapsed() > Duration::from_secs(300) {
                    return Err("timed out waiting for global VLM queue lock".to_string());
                }
                thread::sleep(Duration::from_millis(250));
            }
            Err(error) => {
                return Err(format!(
                    "failed to acquire VLM queue lock {}: {error}",
                    lock_path.display()
                ))
            }
        }
    }
}

fn remove_stale_vlm_queue_lock(lock_path: &Path) {
    let stale = fs::metadata(lock_path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.elapsed().ok())
        .map(|elapsed| elapsed > Duration::from_secs(600))
        .unwrap_or(false);
    if stale {
        let _ = fs::remove_file(lock_path);
    }
}

fn vlm_queue_count_path(lock_path: &Path) -> PathBuf {
    let name = lock_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("vlm.queue.lock");
    lock_path.with_file_name(format!("{name}.count"))
}

fn run_vlm_summary_for_event(
    cli: &Cli,
    snapshot_path: &Path,
    event: &LocalVisionEvent,
) -> VlmSummaryExecution {
    let image_data_url = match build_image_data_url(snapshot_path) {
        Ok(value) => value,
        Err(error) => {
            return VlmSummaryExecution {
                available: false,
                status: "degraded".to_string(),
                summary: sanitize_sensitive(&error),
                provider_key: "openai_compatible".to_string(),
                model_endpoint_id: Some("vlm-local-openai-compatible".to_string()),
                text: String::new(),
                details: json!({}),
            };
        }
    };
    let client = match OpenAiCompatibleVisionClient::new(OpenAiCompatibleConfig {
        base_url: cli.vlm_api_base.trim_end_matches('/').to_string(),
        api_key: cli.vlm_api_key.clone(),
        model: cli.vlm_model.clone(),
    }) {
        Ok(client) => client,
        Err(error) => {
            return VlmSummaryExecution {
                available: false,
                status: "degraded".to_string(),
                summary: sanitize_sensitive(&error),
                provider_key: "openai_compatible".to_string(),
                model_endpoint_id: Some("vlm-local-openai-compatible".to_string()),
                text: String::new(),
                details: json!({}),
            };
        }
    };
    let detection_summary = format!(
        "event_type={} confidence={:.3} labels={} detector_summary={}",
        event.event_type,
        event.confidence,
        event.labels.join(","),
        event.summary
    );
    match client.describe_frame(&VisionSummaryRequest {
        image_data_url,
        detection_summary,
        user_prompt: Some(cli.vlm_prompt.clone()),
    }) {
        Ok(response) => VlmSummaryExecution {
            available: true,
            status: "active".to_string(),
            summary: "VLM summary extracted from sampled event frame.".to_string(),
            provider_key: "openai_compatible".to_string(),
            model_endpoint_id: Some("vlm-local-openai-compatible".to_string()),
            text: response.summary,
            details: json!({
                "raw_response": response.raw_response,
            }),
        },
        Err(error) => VlmSummaryExecution {
            available: false,
            status: "degraded".to_string(),
            summary: sanitize_sensitive(&format!("VLM request failed: {error}")),
            provider_key: "openai_compatible".to_string(),
            model_endpoint_id: Some("vlm-local-openai-compatible".to_string()),
            text: String::new(),
            details: json!({}),
        },
    }
}

fn build_image_data_url(path: &Path) -> Result<String, String> {
    let bytes =
        fs::read(path).map_err(|error| format!("failed to read sampled VLM frame: {error}"))?;
    let mime = if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        "image/jpeg"
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else {
        return Err("sampled VLM frame is not JPEG or PNG".to_string());
    };
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(format!("data:{mime};base64,{encoded}"))
}

fn create_capture_worker(cli: &Cli) -> Result<Option<PersistentCaptureWorker>, String> {
    if cli.capture_mode != CaptureMode::PersistentFfmpeg {
        return Ok(None);
    }
    if cli.fixture || cli.snapshot_url.is_some() {
        return Err(
            "persistent_ffmpeg capture mode requires an RTSP source, not fixture or snapshot URL"
                .to_string(),
        );
    }
    let rtsp_url = resolve_rtsp_url(cli)?;
    let capture_dir = cli.capture_root.join(safe_component(&cli.camera_id));
    fs::create_dir_all(&capture_dir).map_err(|error| {
        format!(
            "failed to create capture worker dir {}: {error}",
            capture_dir.display()
        )
    })?;
    let mut worker = PersistentCaptureWorker {
        camera_id: cli.camera_id.clone(),
        ffmpeg: cli.ffmpeg.clone(),
        rtsp_url,
        latest_path: capture_dir.join("latest.jpg"),
        metadata_path: capture_dir.join("latest.json"),
        child: None,
        started_at: Instant::now(),
        reconnect_count: 0,
        max_frame_age_ms: cli.max_frame_age_ms,
        decode_backend: cli.decode_backend.clone(),
    };
    worker.ensure_running()?;
    Ok(Some(worker))
}

fn capture_snapshot(
    cli: &Cli,
    snapshot_path: &Path,
    capture_worker: Option<&mut PersistentCaptureWorker>,
) -> Result<CaptureResult, String> {
    let started = Instant::now();
    if cli.fixture {
        fs::write(
            snapshot_path,
            [0xff, 0xd8, 0xff, 0xdb, 0x00, 0x43, 0xff, 0xd9],
        )
        .map_err(|error| format!("failed to write fixture snapshot: {error}"))?;
        return Ok(CaptureResult {
            capture_ms: started.elapsed().as_millis() as u64,
            capture_read_ms: Some(started.elapsed().as_millis() as u64),
            frame_age_ms: Some(0),
            stream_uptime_ms: None,
            reconnect_count: 0,
            decode_backend: "fixture".to_string(),
        });
    }
    if let Some(url) = cli.snapshot_url.as_deref() {
        let bytes = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|error| format!("failed to build HTTP client: {error}"))?
            .get(url)
            .send()
            .map_err(|error| {
                format!(
                    "snapshot HTTP request failed: {}",
                    sanitize_sensitive(&error.to_string())
                )
            })?
            .error_for_status()
            .map_err(|error| {
                format!(
                    "snapshot HTTP status failed: {}",
                    sanitize_sensitive(&error.to_string())
                )
            })?
            .bytes()
            .map_err(|error| format!("failed to read snapshot bytes: {error}"))?;
        fs::write(snapshot_path, bytes)
            .map_err(|error| format!("failed to write snapshot: {error}"))?;
        return Ok(CaptureResult {
            capture_ms: started.elapsed().as_millis() as u64,
            capture_read_ms: Some(started.elapsed().as_millis() as u64),
            frame_age_ms: Some(0),
            stream_uptime_ms: None,
            reconnect_count: 0,
            decode_backend: "http_snapshot".to_string(),
        });
    }
    if cli.capture_mode == CaptureMode::PersistentFfmpeg {
        let Some(worker) = capture_worker else {
            return Err("persistent capture worker was not initialized".to_string());
        };
        return worker.copy_latest_to(snapshot_path);
    }
    let rtsp_url = resolve_rtsp_url(cli)?;
    let output = Command::new(&cli.ffmpeg)
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-rtsp_transport")
        .arg("tcp")
        .arg("-y")
        .arg("-i")
        .arg(&rtsp_url)
        .arg("-frames:v")
        .arg("1")
        .arg(snapshot_path)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("failed to launch ffmpeg: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "ffmpeg snapshot failed: {}",
            sanitize_sensitive(stderr.trim())
        ));
    }
    validate_snapshot_file(snapshot_path)?;
    Ok(CaptureResult {
        capture_ms: started.elapsed().as_millis() as u64,
        capture_read_ms: Some(started.elapsed().as_millis() as u64),
        frame_age_ms: None,
        stream_uptime_ms: None,
        reconnect_count: 0,
        decode_backend: cli.decode_backend.clone(),
    })
}

fn validate_snapshot_file(snapshot_path: &Path) -> Result<(), String> {
    let metadata = fs::metadata(snapshot_path)
        .map_err(|error| format!("ffmpeg did not create snapshot: {error}"))?;
    if metadata.len() == 0 {
        return Err("ffmpeg created an empty snapshot".to_string());
    }
    Ok(())
}

fn copy_non_empty_snapshot_with_retry(
    source_path: &Path,
    snapshot_path: &Path,
    attempts: usize,
    retry_delay: Duration,
) -> Result<u64, String> {
    let attempts = attempts.max(1);
    let mut last_error = "persistent capture source did not produce a valid frame".to_string();
    for attempt in 0..attempts {
        match fs::copy(source_path, snapshot_path) {
            Ok(bytes) => match validate_snapshot_file(snapshot_path) {
                Ok(()) => return Ok(bytes),
                Err(error) => {
                    last_error = sanitize_sensitive(&error);
                    let _ = fs::remove_file(snapshot_path);
                }
            },
            Err(error) => {
                last_error = format!(
                    "failed to copy persistent capture frame to snapshot: {}",
                    sanitize_sensitive(&error.to_string())
                );
            }
        }
        if attempt + 1 < attempts {
            thread::sleep(retry_delay);
        }
    }
    Err(last_error)
}

fn run_analyzer(
    cli: &Cli,
    snapshot_path: &Path,
) -> Result<Option<LocalVisionAnalyzerResult>, String> {
    let Some(command) = cli.analyzer_command.as_deref() else {
        return Ok(None);
    };
    if !Path::new(command).exists() {
        return Err(format!("analyzer command not found: {command}"));
    }
    if !Path::new(&cli.model_path).exists() {
        return Err(format!("analyzer model not found: {}", cli.model_path));
    }
    if !Path::new(&cli.label_path).exists() {
        return Err(format!("analyzer label file not found: {}", cli.label_path));
    }
    let started = Instant::now();
    let output = Command::new(command)
        .arg("--image")
        .arg(snapshot_path)
        .arg("--model")
        .arg(&cli.model_path)
        .arg("--label")
        .arg(&cli.label_path)
        .arg("--provider")
        .arg(&cli.provider)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("failed to launch analyzer: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        return Err(format!(
            "analyzer failed status={:?}: {} {}",
            output.status.code(),
            sanitize_sensitive(stdout.trim()),
            sanitize_sensitive(stderr.trim())
        ));
    }
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("failed to parse analyzer JSON: {error}; stdout={stdout}"))?;
    if value.get("ok").and_then(Value::as_bool) == Some(false) {
        return Err(format!(
            "analyzer returned ok=false: {}",
            sanitize_sensitive(&stdout)
        ));
    }
    let mut result: LocalVisionAnalyzerResult = serde_json::from_value(value)
        .map_err(|error| format!("failed to decode analyzer result: {error}"))?;
    result.command_latency_ms = Some(started.elapsed().as_millis() as u64);
    if result.latency_ms.is_none() {
        result.latency_ms = result.command_latency_ms;
    }
    Ok(Some(result))
}

fn post_event(beacon_url: &str, event: &LocalVisionEvent) -> Result<u16, String> {
    let url = format!("{}/api/vision/events", beacon_url.trim_end_matches('/'));
    let response = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| format!("failed to build HTTP client: {error}"))?
        .post(url)
        .json(event)
        .send()
        .map_err(|error| format!("failed to POST local vision event: {error}"))?;
    Ok(response.status().as_u16())
}

fn probe_official_vision_runtime() -> Value {
    let package_candidates = [
        "spacemit-onnxruntime",
        "spacemit-ort",
        "onnxruntime-spacemit",
        "libonnxruntime-spacemit",
        "spacemit-ai-runtime",
        "bianbu-ai-runtime",
    ];
    let command_candidates = [
        "onnxruntime_perf_test",
        "spacemit-ort-runner",
        "spacemit-vision-demo",
        "hailo",
    ];
    let mut packages = Vec::new();
    if command_available("dpkg-query") {
        for package in package_candidates {
            let output = Command::new("dpkg-query").arg("-W").arg(package).output();
            if let Ok(output) = output {
                if output.status.success() {
                    packages.push(String::from_utf8_lossy(&output.stdout).trim().to_string());
                }
            }
        }
    }
    let commands = command_candidates
        .into_iter()
        .filter(|command| command_available(command))
        .collect::<Vec<_>>();
    let official_status = if packages.is_empty() && commands.is_empty() {
        "missing"
    } else {
        "present_unverified"
    };
    json!({
        "official_status": official_status,
        "packages": packages,
        "commands": commands,
        "baseline_note": if official_status == "missing" {
            "official SpacemiT/Bianbu vision runtime recipe was not found by package/command probe"
        } else {
            "official runtime candidate exists but this smoke still uses CPU fallback until model recipe is verified"
        },
    })
}

fn command_available(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success() || status.code().is_some())
        .unwrap_or(false)
}

impl PersistentCaptureWorker {
    fn ensure_running(&mut self) -> Result<(), String> {
        let mut restart = self.child.is_none();
        if let Some(child) = self.child.as_mut() {
            match child.try_wait() {
                Ok(Some(_status)) => {
                    restart = true;
                    self.child = None;
                    self.reconnect_count = self.reconnect_count.saturating_add(1);
                }
                Ok(None) => return Ok(()),
                Err(error) => {
                    restart = true;
                    self.child = None;
                    self.reconnect_count = self.reconnect_count.saturating_add(1);
                    eprintln!(
                        "capture worker status check failed for {}: {}",
                        self.camera_id,
                        sanitize_sensitive(&error.to_string())
                    );
                }
            }
        }
        if restart {
            let child = Command::new(&self.ffmpeg)
                .arg("-hide_banner")
                .arg("-loglevel")
                .arg("error")
                .arg("-rtsp_transport")
                .arg("tcp")
                .arg("-y")
                .arg("-i")
                .arg(&self.rtsp_url)
                .arg("-an")
                .arg("-vf")
                .arg("fps=1")
                .arg("-q:v")
                .arg("4")
                .arg("-update")
                .arg("1")
                .arg(&self.latest_path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|error| {
                    format!(
                        "failed to launch persistent ffmpeg capture worker: {}",
                        sanitize_sensitive(&error.to_string())
                    )
                })?;
            self.started_at = Instant::now();
            self.child = Some(child);
        }
        Ok(())
    }

    fn copy_latest_to(&mut self, snapshot_path: &Path) -> Result<CaptureResult, String> {
        let started = Instant::now();
        self.ensure_running()?;
        let (frame_age_ms, byte_size) = self.wait_for_fresh_frame()?;
        let copy_started = Instant::now();
        let copied_bytes = copy_non_empty_snapshot_with_retry(
            &self.latest_path,
            snapshot_path,
            PERSISTENT_FRAME_COPY_ATTEMPTS,
            Duration::from_millis(PERSISTENT_FRAME_COPY_RETRY_MS),
        )?;
        let capture_read_ms = copy_started.elapsed().as_millis() as u64;
        let stream_uptime_ms = self.started_at.elapsed().as_millis() as u64;
        self.write_latest_metadata(frame_age_ms, copied_bytes.max(byte_size), stream_uptime_ms)?;
        Ok(CaptureResult {
            capture_ms: started.elapsed().as_millis() as u64,
            capture_read_ms: Some(capture_read_ms),
            frame_age_ms: Some(frame_age_ms),
            stream_uptime_ms: Some(stream_uptime_ms),
            reconnect_count: self.reconnect_count,
            decode_backend: self.decode_backend.clone(),
        })
    }

    fn wait_for_fresh_frame(&mut self) -> Result<(u64, u64), String> {
        let wait_started = Instant::now();
        loop {
            self.ensure_running()?;
            if let Ok(metadata) = fs::metadata(&self.latest_path) {
                if metadata.len() > 0 {
                    if let Ok(modified) = metadata.modified() {
                        let frame_age_ms = SystemTime::now()
                            .duration_since(modified)
                            .map(|duration| duration.as_millis() as u64)
                            .unwrap_or(0);
                        if frame_age_ms <= self.max_frame_age_ms {
                            return Ok((frame_age_ms, metadata.len()));
                        }
                    }
                }
            }
            if wait_started.elapsed() > Duration::from_secs(12) {
                return Err(format!(
                    "persistent capture did not produce a fresh frame within 12s for {}",
                    self.camera_id
                ));
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn write_latest_metadata(
        &self,
        frame_age_ms: u64,
        byte_size: u64,
        stream_uptime_ms: u64,
    ) -> Result<(), String> {
        let payload = json!({
            "schema": "harbornavi.k3.capture.latestFrame.v1",
            "camera_id": &self.camera_id,
            "updated_at_ms": epoch_millis(),
            "byte_size": byte_size,
            "frame_age_ms": frame_age_ms,
            "stream_uptime_ms": stream_uptime_ms,
            "reconnect_count": self.reconnect_count,
            "decode_backend": &self.decode_backend,
        });
        let text = serde_json::to_string_pretty(&payload)
            .map_err(|error| format!("failed to serialize capture metadata: {error}"))?;
        fs::write(&self.metadata_path, text).map_err(|error| {
            format!(
                "failed to write capture metadata {}: {error}",
                self.metadata_path.display()
            )
        })
    }
}

impl Drop for PersistentCaptureWorker {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl CaptureMode {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "oneshot_ffmpeg" => Ok(Self::OneshotFfmpeg),
            "persistent_ffmpeg" => Ok(Self::PersistentFfmpeg),
            "local_restream" => Ok(Self::LocalRestream),
            other => Err(format!(
                "capture mode must be oneshot_ffmpeg, persistent_ffmpeg, or local_restream; got {other}"
            )),
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            Self::OneshotFfmpeg => "oneshot_ffmpeg",
            Self::PersistentFfmpeg => "persistent_ffmpeg",
            Self::LocalRestream => "local_restream",
        }
    }
}

fn resolve_rtsp_url(cli: &Cli) -> Result<String, String> {
    if let Some(url) = cli.rtsp_url.as_deref() {
        return Ok(url.to_string());
    }
    if let Some(reference) = cli.source_secret_ref.as_deref() {
        let env_name = reference.strip_prefix("env:").unwrap_or(reference);
        return std::env::var(env_name).map_err(|_| {
            format!("missing RTSP source secret in environment reference {env_name}")
        });
    }
    Err("missing snapshot source; pass --rtsp-url, --source-secret-ref, --snapshot-url, or --fixture"
        .to_string())
}

fn safe_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
}

fn sleep_until(deadline: Instant) {
    let now = Instant::now();
    if deadline > now {
        thread::sleep(deadline - now);
    }
}

fn append_smoke_run(file: &mut fs::File, run: &SmokeRun) -> Result<(), String> {
    serde_json::to_writer(&mut *file, run)
        .map_err(|error| format!("failed to serialize smoke run: {error}"))?;
    writeln!(file).map_err(|error| format!("failed to write smoke run newline: {error}"))?;
    file.flush()
        .map_err(|error| format!("failed to flush smoke run log: {error}"))
}

#[derive(Default)]
struct SmokeSummaryAccumulator {
    total: usize,
    passed: usize,
    failed: usize,
    sum_total_ms: u64,
    max_total_ms: u64,
    under_2s: usize,
    under_5s: usize,
    detection_count: usize,
    vlm_total: usize,
    vlm_passed: usize,
    total_values: Vec<u64>,
    detector_values: Vec<u64>,
    event_ingest_values: Vec<u64>,
    capture_read_values: Vec<u64>,
    frame_age_values: Vec<u64>,
    vlm_values: Vec<u64>,
}

impl SmokeSummaryAccumulator {
    fn observe(&mut self, run: &SmokeRun) {
        self.total = self.total.saturating_add(1);
        if run.ok {
            self.passed = self.passed.saturating_add(1);
        } else {
            self.failed = self.failed.saturating_add(1);
        }
        self.sum_total_ms = self.sum_total_ms.saturating_add(run.total_ms);
        self.max_total_ms = self.max_total_ms.max(run.total_ms);
        if run.total_ms < 2000 {
            self.under_2s = self.under_2s.saturating_add(1);
        }
        if run.total_ms < 5000 {
            self.under_5s = self.under_5s.saturating_add(1);
        }
        self.total_values.push(run.total_ms);
        if let Some(value) = run.detector_ms {
            self.detector_values.push(value);
        }
        if let Some(value) = run.event_ingest_ms {
            self.event_ingest_values.push(value);
        }
        if let Some(value) = run.capture_read_ms {
            self.capture_read_values.push(value);
        }
        if let Some(value) = run.frame_age_ms {
            self.frame_age_values.push(value);
        }
        if let Some(value) = run.vlm_ms {
            self.vlm_values.push(value);
        }
        if let Some(count) = run.detection_count {
            self.detection_count = self.detection_count.saturating_add(count);
        }
        if run.vlm_status.is_some() {
            self.vlm_total = self.vlm_total.saturating_add(1);
            if run.vlm_status.as_deref() == Some("active") {
                self.vlm_passed = self.vlm_passed.saturating_add(1);
            }
        }
    }

    fn finish(self) -> SmokeSummary {
        SmokeSummary {
            total: self.total,
            passed: self.passed,
            failed: self.failed,
            average_total_ms: if self.total == 0 {
                0
            } else {
                self.sum_total_ms / self.total as u64
            },
            p95_total_ms: p95(&self.total_values),
            average_detector_ms: average_u64(&self.detector_values),
            p95_detector_ms: p95(&self.detector_values),
            average_event_ingest_ms: average_u64(&self.event_ingest_values),
            p95_event_ingest_ms: p95(&self.event_ingest_values),
            average_capture_read_ms: average_u64(&self.capture_read_values),
            average_frame_age_ms: average_u64(&self.frame_age_values),
            p95_capture_read_ms: p95(&self.capture_read_values),
            p95_frame_age_ms: p95(&self.frame_age_values),
            max_total_ms: self.max_total_ms,
            under_2s: self.under_2s,
            under_5s: self.under_5s,
            detection_runs: self.detector_values.len(),
            detection_count: self.detection_count,
            vlm_total: self.vlm_total,
            vlm_passed: self.vlm_passed,
            vlm_degraded: self.vlm_total.saturating_sub(self.vlm_passed),
            average_vlm_ms: average_u64(&self.vlm_values),
            p95_vlm_ms: p95(&self.vlm_values),
        }
    }
}

#[cfg(test)]
fn summarize_runs(runs: &[SmokeRun]) -> SmokeSummary {
    let mut accumulator = SmokeSummaryAccumulator::default();
    for run in runs {
        accumulator.observe(run);
    }
    accumulator.finish()
}

fn build_smoke_evidence(runs: &[SmokeRun]) -> SmokeEvidence {
    let mut evidence = SmokeEvidence {
        metadata_only: true,
        secret_scan: "clean".to_string(),
        retained_run_count: runs.len(),
        latest_retained_event_id: runs
            .iter()
            .rev()
            .find_map(|run| run.event.as_ref().map(|event| event.event_id.clone())),
        ..SmokeEvidence::default()
    };

    match local_vision_event_store_stats_default() {
        Ok(stats) => record_evidence_value(
            "event_store_stats",
            stats,
            &mut evidence.event_store_stats,
            &mut evidence.evidence_errors,
        ),
        Err(error) => evidence
            .evidence_errors
            .push(sanitize_evidence_error("event_store_stats", &error)),
    }

    let recent_events = match list_recent_local_vision_events_default(50) {
        Ok(events) => events,
        Err(error) => {
            evidence
                .evidence_errors
                .push(sanitize_evidence_error("recent_events", &error));
            Vec::new()
        }
    };
    evidence.recent_store_event_count = recent_events.len();

    if !recent_events.is_empty() {
        let query = FamilyTimelineQueryOptions::default();
        match build_family_timeline_digest_from_query(&recent_events, &query) {
            Ok(digest) => record_evidence_value(
                "family_digest",
                digest,
                &mut evidence.family_digest,
                &mut evidence.evidence_errors,
            ),
            Err(error) => evidence
                .evidence_errors
                .push(sanitize_evidence_error("family_digest", &error)),
        }

        if let Some(latest) = recent_events.first() {
            match build_local_vision_family_summary(latest) {
                Ok(summary) => record_evidence_value(
                    "latest_family_summary",
                    summary,
                    &mut evidence.latest_family_summary,
                    &mut evidence.evidence_errors,
                ),
                Err(error) => evidence
                    .evidence_errors
                    .push(sanitize_evidence_error("latest_family_summary", &error)),
            }

            match build_notification_intent_evidence(latest) {
                Ok(intent) => evidence.notification_intent = Some(intent),
                Err(error) => evidence
                    .evidence_errors
                    .push(sanitize_evidence_error("notification_intent", &error)),
            }
        }
    }

    evidence
}

fn record_evidence_value<T: Serialize>(
    label: &str,
    value: T,
    target: &mut Option<Value>,
    errors: &mut Vec<String>,
) {
    match serde_json::to_value(value) {
        Ok(value) => *target = Some(value),
        Err(error) => errors.push(sanitize_evidence_error(label, &error.to_string())),
    }
}

fn build_notification_intent_evidence(stored: &StoredLocalVisionEvent) -> Result<Value, String> {
    let intent = build_local_vision_notification_intent(stored, "gw_route_redacted_smoke")?;
    let request = intent.notification_request;
    let structured_payload_fields = request
        .content
        .structured_payload
        .as_object()
        .map(|object| object.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    Ok(json!({
        "notification_id": request.notification_id,
        "trace_id": request.trace_id,
        "source": {
            "service": request.source.service,
            "module": request.source.module,
            "event_type": request.source.event_type,
        },
        "destination": {
            "kind": request.destination.kind,
            "route_bound": !request.destination.route_key.trim().is_empty(),
            "destination_redacted": true,
            "delivery_destination_only": true,
        },
        "delivery": {
            "mode": request.delivery.mode,
            "idempotency_key": request.delivery.idempotency_key,
        },
        "content": {
            "title": request.content.title,
            "payload_format": request.content.payload_format,
            "structured_payload_fields": structured_payload_fields,
            "attachments_included": false,
        },
        "audit_record": intent.audit_record,
        "metadata_only": true,
        "secret_scan": "clean",
        "raw_image_included": false,
        "local_paths_included": false,
    }))
}

fn sanitize_evidence_error(label: &str, error: &str) -> String {
    let sanitized = sanitize_sensitive(error);
    if sanitized.contains('\\') || sanitized.contains('/') {
        format!("{label}: [redacted-local-path-or-secret]")
    } else {
        format!("{label}: {sanitized}")
    }
}

fn average_u64(values: &[u64]) -> u64 {
    if values.is_empty() {
        0
    } else {
        values.iter().sum::<u64>() / values.len() as u64
    }
}

fn p95(values: &[u64]) -> u64 {
    if values.is_empty() {
        return 0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let index = ((sorted.len() as f64 * 0.95).ceil() as usize).saturating_sub(1);
    sorted[index.min(sorted.len() - 1)]
}

fn report_snapshot_path(cli: &Cli, snapshot_path: &Path) -> Option<String> {
    if cli.redact_paths {
        Some("[redacted-local-path]".to_string())
    } else {
        Some(snapshot_path.to_string_lossy().to_string())
    }
}

fn sanitize_sensitive(value: &str) -> String {
    let mut output = String::new();
    for token in value.split_whitespace() {
        if token.to_ascii_lowercase().contains("rtsp://") {
            output.push_str("rtsp://redacted ");
        } else if token.to_ascii_lowercase().contains("password=") {
            output.push_str("password=redacted ");
        } else {
            output.push_str(token);
            output.push(' ');
        }
    }
    output.trim().to_string()
}

fn epoch_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn default_analyzer_command() -> Option<String> {
    let installed = "/usr/lib/harboros-beacon/harbornavi_k3_yolov8_analyzer.py";
    if Path::new(installed).exists() {
        Some(installed.to_string())
    } else {
        None
    }
}

impl Cli {
    fn parse() -> Self {
        let args = std::env::args().skip(1).collect::<Vec<_>>();
        let mut cli = Self {
            camera_id: "k3-local-camera".to_string(),
            rtsp_url: None,
            source_secret_ref: None,
            snapshot_url: None,
            beacon_url: "http://127.0.0.1:4174".to_string(),
            output_dir: PathBuf::from("/tmp/harbornavi-p0/local-vision-event"),
            duration_seconds: 0,
            interval_seconds: 10,
            phase_offset_ms: 0,
            no_post: false,
            fixture: false,
            ffmpeg: std::env::var("HARBOR_FFMPEG_BIN").unwrap_or_else(|_| "ffmpeg".to_string()),
            capture_mode: CaptureMode::OneshotFfmpeg,
            capture_root: std::env::var("HARBORNAVI_CAPTURE_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("/run/harbornavi/capture")),
            max_frame_age_ms: std::env::var("HARBORNAVI_MAX_FRAME_AGE_MS")
                .ok()
                .map(|value| parse_u64(&value))
                .unwrap_or(2500),
            decode_backend: std::env::var("HARBORNAVI_DECODE_BACKEND")
                .unwrap_or_else(|_| "ffmpeg_sw".to_string()),
            analyzer_command: std::env::var("HARBOR_K3_YOLO_ANALYZER")
                .ok()
                .or_else(default_analyzer_command),
            model_path: std::env::var("HARBOR_K3_YOLO_MODEL").unwrap_or_else(|_| {
                "/var/lib/harboros-beacon/models/yolov8n_192x320.q.onnx".to_string()
            }),
            label_path: std::env::var("HARBOR_K3_YOLO_LABELS")
                .unwrap_or_else(|_| "/var/lib/harboros-beacon/models/label.txt".to_string()),
            provider: std::env::var("HARBOR_K3_YOLO_PROVIDER")
                .unwrap_or_else(|_| "cpu".to_string()),
            redact_paths: false,
            vlm_enrich: std::env::var("HARBORNAVI_VLM_ENRICH")
                .ok()
                .map(|value| matches!(value.as_str(), "1" | "true" | "yes"))
                .unwrap_or(false),
            vlm_api_base: std::env::var("HARBORNAVI_VLM_API_BASE")
                .unwrap_or_else(|_| "http://127.0.0.1:8080/v1".to_string()),
            vlm_model: std::env::var("HARBORNAVI_VLM_MODEL")
                .unwrap_or_else(|_| "qwen3_5vl_0.8b-text-q41.gguf".to_string()),
            vlm_api_key: std::env::var("HARBORNAVI_VLM_API_KEY")
                .unwrap_or_else(|_| "local".to_string()),
            vlm_prompt: std::env::var("HARBORNAVI_VLM_PROMPT").unwrap_or_else(|_| {
                "请根据这张家庭摄像头关键帧，用中文输出一句家庭事件摘要，重点说明是否有人、宠物、车辆或异常活动。".to_string()
            }),
            vlm_sample_every: std::env::var("HARBORNAVI_VLM_SAMPLE_EVERY")
                .ok()
                .map(|value| parse_u64(&value))
                .unwrap_or(30),
            vlm_max_samples: std::env::var("HARBORNAVI_VLM_MAX_SAMPLES")
                .ok()
                .map(|value| parse_u64(&value) as usize)
                .unwrap_or(1),
            vlm_queue_lock_path: std::env::var("HARBORNAVI_VLM_QUEUE_LOCK_PATH")
                .ok()
                .map(PathBuf::from),
            vlm_global_max_samples: std::env::var("HARBORNAVI_VLM_GLOBAL_MAX_SAMPLES")
                .ok()
                .map(|value| parse_u64(&value) as usize),
            vlm_trigger_policy: std::env::var("HARBORNAVI_VLM_TRIGGER_POLICY")
                .ok()
                .map(|value| VlmTriggerPolicy::parse(&value).unwrap_or_else(|error| fail(&error)))
                .unwrap_or(VlmTriggerPolicy::DetectionOrPeriodic),
        };
        let mut index = 0usize;
        while index < args.len() {
            match args[index].as_str() {
                "--camera-id" => cli.camera_id = take_value(&args, &mut index, "--camera-id"),
                "--rtsp-url" => cli.rtsp_url = Some(take_value(&args, &mut index, "--rtsp-url")),
                "--source-secret-ref" => {
                    cli.source_secret_ref =
                        Some(take_value(&args, &mut index, "--source-secret-ref"))
                }
                "--snapshot-url" => {
                    cli.snapshot_url = Some(take_value(&args, &mut index, "--snapshot-url"))
                }
                "--beacon-url" => cli.beacon_url = take_value(&args, &mut index, "--beacon-url"),
                "--output-dir" => {
                    cli.output_dir = PathBuf::from(take_value(&args, &mut index, "--output-dir"))
                }
                "--duration-seconds" => {
                    cli.duration_seconds =
                        parse_u64(&take_value(&args, &mut index, "--duration-seconds"))
                }
                "--interval-seconds" => {
                    cli.interval_seconds =
                        parse_u64(&take_value(&args, &mut index, "--interval-seconds"))
                }
                "--phase-offset-ms" => {
                    cli.phase_offset_ms =
                        parse_u64(&take_value(&args, &mut index, "--phase-offset-ms"))
                }
                "--ffmpeg" => cli.ffmpeg = take_value(&args, &mut index, "--ffmpeg"),
                "--capture-mode" => {
                    cli.capture_mode =
                        CaptureMode::parse(&take_value(&args, &mut index, "--capture-mode"))
                            .unwrap_or_else(|error| fail(&error));
                }
                "--capture-root" => {
                    cli.capture_root =
                        PathBuf::from(take_value(&args, &mut index, "--capture-root"))
                }
                "--max-frame-age-ms" => {
                    cli.max_frame_age_ms =
                        parse_u64(&take_value(&args, &mut index, "--max-frame-age-ms"))
                }
                "--decode-backend" => {
                    cli.decode_backend = take_value(&args, &mut index, "--decode-backend")
                }
                "--analyzer-command" => {
                    cli.analyzer_command = Some(take_value(&args, &mut index, "--analyzer-command"))
                }
                "--model-path" => cli.model_path = take_value(&args, &mut index, "--model-path"),
                "--label-path" => cli.label_path = take_value(&args, &mut index, "--label-path"),
                "--provider" => {
                    cli.provider = take_value(&args, &mut index, "--provider");
                    if cli.provider != "cpu" && cli.provider != "spacemit" {
                        fail("--provider must be cpu or spacemit");
                    }
                }
                "--disable-analyzer" => cli.analyzer_command = None,
                "--no-post" => cli.no_post = true,
                "--fixture" => cli.fixture = true,
                "--redact-paths" => cli.redact_paths = true,
                "--vlm-enrich" => cli.vlm_enrich = true,
                "--vlm-api-base" => {
                    cli.vlm_api_base = take_value(&args, &mut index, "--vlm-api-base")
                }
                "--vlm-model" => cli.vlm_model = take_value(&args, &mut index, "--vlm-model"),
                "--vlm-api-key" => cli.vlm_api_key = take_value(&args, &mut index, "--vlm-api-key"),
                "--vlm-prompt" => cli.vlm_prompt = take_value(&args, &mut index, "--vlm-prompt"),
                "--vlm-sample-every" => {
                    cli.vlm_sample_every =
                        parse_u64(&take_value(&args, &mut index, "--vlm-sample-every"))
                }
                "--vlm-max-samples" => {
                    cli.vlm_max_samples =
                        parse_u64(&take_value(&args, &mut index, "--vlm-max-samples")) as usize
                }
                "--vlm-queue-lock-path" => {
                    cli.vlm_queue_lock_path = Some(PathBuf::from(take_value(
                        &args,
                        &mut index,
                        "--vlm-queue-lock-path",
                    )))
                }
                "--vlm-global-max-samples" => {
                    cli.vlm_global_max_samples = Some(parse_u64(&take_value(
                        &args,
                        &mut index,
                        "--vlm-global-max-samples",
                    )) as usize)
                }
                "--vlm-trigger-policy" => {
                    cli.vlm_trigger_policy = VlmTriggerPolicy::parse(&take_value(
                        &args,
                        &mut index,
                        "--vlm-trigger-policy",
                    ))
                    .unwrap_or_else(|error| fail(&error));
                }
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                other => fail(&format!("unknown argument {other}")),
            }
            index += 1;
        }
        if cli.provider != "cpu" && cli.provider != "spacemit" {
            fail("--provider must be cpu or spacemit");
        }
        if cli.capture_mode == CaptureMode::PersistentFfmpeg && cli.decode_backend == "ffmpeg_sw" {
            cli.decode_backend = "ffmpeg_sw_persistent".to_string();
        }
        if cli.capture_mode == CaptureMode::LocalRestream && cli.decode_backend == "ffmpeg_sw" {
            cli.decode_backend = "ffmpeg_sw_local_restream".to_string();
        }
        cli
    }
}

fn take_value(args: &[String], index: &mut usize, flag: &str) -> String {
    *index += 1;
    if *index >= args.len() {
        fail(&format!("missing value for {flag}"));
    }
    args[*index].clone()
}

fn parse_u64(value: &str) -> u64 {
    value
        .parse::<u64>()
        .unwrap_or_else(|_| fail(&format!("invalid integer value: {value}")))
}

fn print_usage() {
    println!(
        "Usage: harbornavi-k3-local-vision-smoke [--camera-id ID] [--rtsp-url URL | --source-secret-ref env:VAR | --snapshot-url URL | --fixture] [--duration-seconds N] [--interval-seconds N] [--phase-offset-ms N] [--capture-mode oneshot_ffmpeg|persistent_ffmpeg|local_restream] [--capture-root PATH] [--max-frame-age-ms N] [--decode-backend NAME] [--beacon-url URL] [--output-dir PATH] [--analyzer-command PATH] [--model-path PATH] [--label-path PATH] [--provider cpu|spacemit] [--disable-analyzer] [--vlm-enrich] [--vlm-api-base URL] [--vlm-model MODEL] [--vlm-sample-every N] [--vlm-max-samples N] [--vlm-queue-lock-path PATH] [--vlm-global-max-samples N] [--vlm-trigger-policy periodic|detection_or_periodic] [--no-post] [--redact-paths]"
    );
}

impl Cli {
    fn vlm_queue_mode(&self) -> &'static str {
        if self.vlm_queue_lock_path.is_some() {
            "global_serial"
        } else {
            "none"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_cli() -> Cli {
        Cli {
            camera_id: "cam-test".to_string(),
            rtsp_url: None,
            source_secret_ref: None,
            snapshot_url: None,
            beacon_url: "http://127.0.0.1:4174".to_string(),
            output_dir: PathBuf::from("/tmp/harbornavi-test"),
            duration_seconds: 0,
            interval_seconds: 10,
            phase_offset_ms: 0,
            no_post: true,
            fixture: true,
            ffmpeg: "ffmpeg".to_string(),
            capture_mode: CaptureMode::OneshotFfmpeg,
            capture_root: PathBuf::from("/tmp/harbornavi-test-capture"),
            max_frame_age_ms: 2500,
            decode_backend: "ffmpeg_sw".to_string(),
            analyzer_command: None,
            model_path: "/tmp/model.onnx".to_string(),
            label_path: "/tmp/label.txt".to_string(),
            provider: "cpu".to_string(),
            redact_paths: true,
            vlm_enrich: true,
            vlm_api_base: "http://127.0.0.1:8080/v1".to_string(),
            vlm_model: "test-vlm".to_string(),
            vlm_api_key: "local".to_string(),
            vlm_prompt: "describe".to_string(),
            vlm_sample_every: 3,
            vlm_max_samples: 4,
            vlm_queue_lock_path: None,
            vlm_global_max_samples: None,
            vlm_trigger_policy: VlmTriggerPolicy::DetectionOrPeriodic,
        }
    }

    fn unique_temp_dir(name: &str) -> PathBuf {
        let millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis())
            .unwrap_or_default();
        std::env::temp_dir().join(format!("harbornavi-{name}-{}-{millis}", std::process::id()))
    }

    #[test]
    fn detection_or_periodic_triggers_on_detection() {
        let cli = base_cli();
        assert!(should_run_vlm(&cli, 1, Some(1), 0));
        assert!(should_run_vlm(&cli, 2, Some(1), 0));
    }

    #[test]
    fn periodic_trigger_ignores_detection_until_interval() {
        let mut cli = base_cli();
        cli.vlm_trigger_policy = VlmTriggerPolicy::Periodic;
        assert!(!should_run_vlm(&cli, 1, Some(3), 0));
        assert!(!should_run_vlm(&cli, 2, Some(3), 0));
        assert!(should_run_vlm(&cli, 3, Some(0), 0));
    }

    #[test]
    fn local_sample_limit_still_applies_before_queue() {
        let cli = base_cli();
        assert!(!should_run_vlm(&cli, 1, Some(1), cli.vlm_max_samples));
    }

    #[test]
    fn global_vlm_slot_respects_max_samples() {
        let dir = unique_temp_dir("vlm-queue");
        let lock_path = dir.join("vlm.queue.lock");
        let mut cli = base_cli();
        cli.vlm_queue_lock_path = Some(lock_path.clone());
        cli.vlm_global_max_samples = Some(1);

        match claim_vlm_slot(&cli, 0) {
            VlmQueueDecision::Run {
                sample_index,
                _guard: guard,
            } => {
                assert_eq!(sample_index, 1);
                assert!(lock_path.exists());
                drop(guard);
                assert!(!lock_path.exists());
            }
            _ => panic!("first global VLM slot should run"),
        }
        assert!(matches!(claim_vlm_slot(&cli, 0), VlmQueueDecision::Skip));
        assert_eq!(
            fs::read_to_string(vlm_queue_count_path(&lock_path))
                .expect("counter exists")
                .trim(),
            "1"
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn persistent_capture_copy_retries_until_snapshot_is_non_empty() {
        let dir = unique_temp_dir("persistent-copy-retry");
        fs::create_dir_all(&dir).expect("create temp dir");
        let latest_path = dir.join("latest.jpg");
        let snapshot_path = dir.join("snapshot.jpg");

        let writer_path = latest_path.clone();
        let writer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(25));
            fs::write(writer_path, [0xff, 0xd8, 0xff, 0xd9]).expect("write valid frame bytes");
        });

        let copied = copy_non_empty_snapshot_with_retry(
            &latest_path,
            &snapshot_path,
            8,
            Duration::from_millis(10),
        )
        .expect("copy should retry past transient empty frame");
        writer.join().expect("writer joins");

        assert!(copied > 0);
        assert!(
            fs::metadata(&snapshot_path)
                .expect("snapshot metadata")
                .len()
                > 0
        );
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn persistent_capture_copy_fails_when_latest_stays_empty() {
        let dir = unique_temp_dir("persistent-copy-empty");
        fs::create_dir_all(&dir).expect("create temp dir");
        let latest_path = dir.join("latest.jpg");
        let snapshot_path = dir.join("snapshot.jpg");
        fs::write(&latest_path, []).expect("write empty latest frame");

        let error = copy_non_empty_snapshot_with_retry(
            &latest_path,
            &snapshot_path,
            2,
            Duration::from_millis(1),
        )
        .expect_err("empty latest frame should fail");

        assert!(error.contains("empty snapshot"));
        assert!(!snapshot_path.exists());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn rolling_summary_counts_all_runs_without_retaining_full_report() {
        let runs = (0..150)
            .map(|index| SmokeRun {
                iteration: index + 1,
                ok: index % 10 != 0,
                snapshot_path: Some("[redacted-local-path]".to_string()),
                event: None,
                ingest_http_status: Some(200),
                capture_ms: 10,
                detector_ms: Some(20),
                analyze_ms: 30,
                event_ingest_ms: Some(40),
                total_ms: index as u64,
                capture_mode: "fixture".to_string(),
                capture_read_ms: Some(5),
                frame_age_ms: Some(1),
                stream_uptime_ms: None,
                reconnect_count: 0,
                decode_backend: "fixture".to_string(),
                provider: Some("cpu".to_string()),
                detection_count: Some(1),
                vlm_status: None,
                vlm_ms: None,
                vlm_error: None,
                error: None,
            })
            .collect::<Vec<_>>();

        let summary = summarize_runs(&runs);

        assert_eq!(summary.total, 150);
        assert_eq!(summary.failed, 15);
        assert_eq!(summary.detection_count, 150);
        assert!(summary.p95_total_ms >= 140);
    }

    #[test]
    fn notification_intent_evidence_redacts_route_key() {
        let stored = StoredLocalVisionEvent {
            received_at: "1700000000000".to_string(),
            event: LocalVisionEvent {
                event_id: "event-smoke-1".to_string(),
                camera_id: "front_door".to_string(),
                event_type: "person_detected".to_string(),
                confidence: 0.91,
                labels: vec!["person".to_string()],
                summary: "person near front door".to_string(),
                snapshot_artifact: Default::default(),
                started_at: "1700000000000".to_string(),
                analyzer: "fixture".to_string(),
                latency_ms: 42,
                metrics: json!({}),
                vlm: None,
            },
            audit_record: json!({}),
            ha_mqtt_payload: json!({}),
        };

        let evidence = build_notification_intent_evidence(&stored).expect("evidence");
        let text = serde_json::to_string(&evidence).expect("serialize evidence");

        assert!(!text.contains("gw_route_redacted_smoke"));
        assert!(!text.contains("route_key"));
        assert_eq!(
            evidence
                .pointer("/destination/route_bound")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            evidence
                .pointer("/destination/delivery_destination_only")
                .and_then(Value::as_bool),
            Some(true)
        );
    }
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(2);
}
