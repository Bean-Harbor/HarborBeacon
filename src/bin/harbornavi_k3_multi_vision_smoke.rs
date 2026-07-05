use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
struct Cli {
    camera_manifest: PathBuf,
    output_dir: Option<PathBuf>,
    duration_seconds: Option<u64>,
    interval_seconds: Option<u64>,
    beacon_url: Option<String>,
    analyzer_command: Option<String>,
    model_path: Option<String>,
    label_path: Option<String>,
    provider: Option<String>,
    local_smoke_bin: String,
    no_post: bool,
    vlm_enrich: bool,
    vlm_api_base: Option<String>,
    vlm_model: Option<String>,
    vlm_api_key: Option<String>,
    vlm_sample_every: Option<u64>,
    vlm_max_samples: Option<u64>,
    vlm_queue_lock_path: Option<PathBuf>,
    vlm_global_max_samples: Option<u64>,
    vlm_trigger_policy: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CameraManifest {
    schema: Option<String>,
    duration_seconds: Option<u64>,
    interval_seconds: Option<u64>,
    beacon_url: Option<String>,
    output_dir: Option<PathBuf>,
    analyzer_command: Option<String>,
    model_path: Option<String>,
    label_path: Option<String>,
    provider: Option<String>,
    no_post: Option<bool>,
    vlm_enrich: Option<bool>,
    #[serde(default, alias = "vlm_api_base")]
    vlm_api_base: Option<String>,
    #[serde(default, alias = "vlm_model")]
    vlm_model: Option<String>,
    #[serde(default, alias = "vlm_sample_every")]
    vlm_sample_every: Option<u64>,
    #[serde(default, alias = "vlm_max_samples")]
    vlm_max_samples: Option<u64>,
    #[serde(default, alias = "vlm_queue")]
    vlm_queue: Option<VlmQueueSettings>,
    capture: Option<CaptureSettings>,
    cameras: Vec<CameraSource>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CameraSource {
    camera_id: String,
    kind: Option<String>,
    rtsp_url: Option<String>,
    snapshot_url: Option<String>,
    fixture: Option<bool>,
    duration_seconds: Option<u64>,
    interval_seconds: Option<u64>,
    #[serde(default, alias = "source_secret_ref")]
    source_secret_ref: Option<String>,
    #[serde(default, alias = "capture_mode")]
    capture_mode: Option<String>,
    #[serde(default, alias = "phase_offset_ms")]
    phase_offset_ms: Option<u64>,
    #[serde(default, alias = "max_frame_age_ms")]
    max_frame_age_ms: Option<u64>,
    #[serde(default, alias = "capture_root")]
    capture_root: Option<PathBuf>,
    #[serde(default, alias = "decode_backend")]
    decode_backend: Option<String>,
    capture: Option<CaptureSettings>,
    #[serde(default, alias = "vlm_enrich")]
    vlm_enrich: Option<bool>,
    #[serde(default, alias = "vlm_sample_every")]
    vlm_sample_every: Option<u64>,
    #[serde(default, alias = "vlm_max_samples")]
    vlm_max_samples: Option<u64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CaptureSettings {
    mode: Option<String>,
    #[serde(default, alias = "phase_offset_ms")]
    phase_offset_ms: Option<u64>,
    #[serde(default, alias = "max_frame_age_ms")]
    max_frame_age_ms: Option<u64>,
    #[serde(default, alias = "source_secret_ref")]
    source_secret_ref: Option<String>,
    #[serde(default, alias = "capture_root")]
    capture_root: Option<PathBuf>,
    #[serde(default, alias = "decode_backend")]
    decode_backend: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VlmQueueSettings {
    mode: Option<String>,
    #[serde(default, alias = "lock_path")]
    lock_path: Option<PathBuf>,
    #[serde(default, alias = "global_max_samples")]
    global_max_samples: Option<u64>,
    #[serde(default, alias = "trigger_policy")]
    trigger_policy: Option<String>,
}

#[derive(Debug, Clone)]
struct ResolvedCapture {
    mode: String,
    phase_offset_ms: u64,
    max_frame_age_ms: u64,
    source_secret_ref: Option<String>,
    capture_root: Option<PathBuf>,
    decode_backend: Option<String>,
}

#[derive(Debug, Clone)]
struct ResolvedConfig {
    schema: String,
    output_dir: PathBuf,
    duration_seconds: u64,
    interval_seconds: u64,
    beacon_url: String,
    analyzer_command: Option<String>,
    model_path: Option<String>,
    label_path: Option<String>,
    provider: String,
    no_post: bool,
    local_smoke_bin: String,
    capture: CaptureSettings,
    vlm_enrich: bool,
    vlm_api_base: Option<String>,
    vlm_model: Option<String>,
    vlm_api_key: Option<String>,
    vlm_sample_every: Option<u64>,
    vlm_max_samples: Option<u64>,
    vlm_queue: Option<VlmQueueSettings>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct MultiVisionReport {
    ok: bool,
    schema: String,
    started_at_ms: u128,
    duration_seconds: u64,
    interval_seconds: u64,
    target_p95_ms: u64,
    scheduler: String,
    output_scope: String,
    source_mix: SourceMix,
    aggregate: AggregateSummary,
    vlm_queue: Option<VlmQueueReport>,
    classification: String,
    cameras: Vec<CameraReport>,
    notes: Vec<String>,
}

#[derive(Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct SourceMix {
    real: usize,
    replay: usize,
    snapshot: usize,
    fixture: usize,
    other: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CameraReport {
    camera_id: String,
    source_kind: String,
    capture_mode: String,
    phase_offset_ms: u64,
    ok: bool,
    child_exit_code: Option<i32>,
    total: usize,
    passed: usize,
    failed: usize,
    success_rate: f64,
    average_total_ms: u64,
    p95_total_ms: u64,
    max_total_ms: u64,
    average_capture_ms: u64,
    p95_capture_ms: u64,
    average_capture_read_ms: u64,
    p95_capture_read_ms: u64,
    average_frame_age_ms: u64,
    p95_frame_age_ms: u64,
    max_stream_uptime_ms: u64,
    reconnect_count: u64,
    decode_backend: Option<String>,
    average_detector_ms: u64,
    p95_detector_ms: u64,
    average_event_ingest_ms: u64,
    p95_event_ingest_ms: u64,
    detection_runs: usize,
    detection_count: usize,
    vlm_total: usize,
    vlm_passed: usize,
    p95_vlm_ms: u64,
    error: Option<String>,
}

#[derive(Debug, Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct AggregateSummary {
    total: usize,
    passed: usize,
    failed: usize,
    success_rate: f64,
    p95_total_ms: u64,
    max_total_ms: u64,
    p95_capture_ms: u64,
    p95_capture_read_ms: u64,
    p95_frame_age_ms: u64,
    p95_detector_ms: u64,
    p95_event_ingest_ms: u64,
    vlm_total: usize,
    vlm_passed: usize,
    p95_vlm_ms: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VlmQueueReport {
    mode: String,
    lock_path_redacted: bool,
    global_max_samples: Option<u64>,
    trigger_policy: Option<String>,
    global_total: usize,
    global_passed: usize,
    global_failed: usize,
}

#[derive(Debug, Deserialize)]
struct SingleSmokeReport {
    ok: bool,
    camera_id: String,
    #[serde(default)]
    capture_mode: Option<String>,
    runs: Vec<SingleSmokeRun>,
    summary: SingleSmokeSummary,
}

#[derive(Debug, Deserialize)]
struct SingleSmokeRun {
    ok: bool,
    capture_ms: u64,
    #[serde(default)]
    capture_mode: Option<String>,
    #[serde(default)]
    capture_read_ms: Option<u64>,
    #[serde(default)]
    frame_age_ms: Option<u64>,
    #[serde(default)]
    stream_uptime_ms: Option<u64>,
    #[serde(default)]
    reconnect_count: Option<u64>,
    #[serde(default)]
    decode_backend: Option<String>,
    detector_ms: Option<u64>,
    event_ingest_ms: Option<u64>,
    #[serde(default)]
    vlm_status: Option<String>,
    #[serde(default)]
    vlm_ms: Option<u64>,
    total_ms: u64,
}

#[derive(Debug, Deserialize)]
struct SingleSmokeSummary {
    total: usize,
    passed: usize,
    failed: usize,
    average_total_ms: u64,
    #[serde(default)]
    p95_total_ms: u64,
    max_total_ms: u64,
    average_detector_ms: u64,
    #[serde(default)]
    p95_detector_ms: u64,
    average_event_ingest_ms: u64,
    #[serde(default)]
    p95_event_ingest_ms: u64,
    #[serde(default)]
    p95_capture_read_ms: u64,
    #[serde(default)]
    p95_frame_age_ms: u64,
    detection_runs: usize,
    detection_count: usize,
    #[serde(default)]
    vlm_total: usize,
    #[serde(default)]
    vlm_passed: usize,
    #[serde(default)]
    p95_vlm_ms: u64,
}

fn main() {
    let cli = Cli::parse();
    let manifest = load_manifest(&cli.camera_manifest);
    let config = resolve_config(&cli, &manifest);
    if manifest.cameras.is_empty() {
        fail("camera manifest must contain at least one camera");
    }
    if let Err(error) = fs::create_dir_all(&config.output_dir) {
        fail(&format!(
            "failed to create output dir {}: {error}",
            config.output_dir.display()
        ));
    }

    let camera_count = manifest.cameras.len();
    let handles = manifest
        .cameras
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, camera)| {
            let config = config.clone();
            thread::spawn(move || run_camera(camera, config, index, camera_count))
        })
        .collect::<Vec<_>>();

    let mut camera_reports = Vec::new();
    for handle in handles {
        match handle.join() {
            Ok(report) => camera_reports.push(report),
            Err(_) => camera_reports.push(CameraReport::system_error(
                "unknown-camera",
                "thread-panic",
                "camera worker panicked",
            )),
        }
    }
    camera_reports.sort_by(|left, right| left.camera_id.cmp(&right.camera_id));

    let source_mix = summarize_sources(&manifest.cameras);
    let aggregate = summarize_aggregate(&camera_reports);
    let vlm_queue = summarize_vlm_queue(config.vlm_queue.as_ref(), &aggregate);
    let classification = classify_result(&aggregate, &camera_reports);
    let ok = classification == "pass";
    let notes = vec![
        "This is a mixed simulation load test: one real camera plus replay cameras does not prove four-vendor camera compatibility.".to_string(),
        "Raw RTSP URLs, credentials, local snapshot paths, and image bytes are intentionally excluded from this aggregate report.".to_string(),
    ];
    let report = MultiVisionReport {
        ok,
        schema: config.schema,
        started_at_ms: epoch_millis(),
        duration_seconds: config.duration_seconds,
        interval_seconds: config.interval_seconds,
        target_p95_ms: 5000,
        scheduler: "fixed-rate-staggered".to_string(),
        output_scope: "[redacted-local-path]".to_string(),
        source_mix,
        aggregate,
        vlm_queue,
        classification,
        cameras: camera_reports,
        notes,
    };
    let text = serde_json::to_string_pretty(&report)
        .unwrap_or_else(|error| fail(&format!("failed to serialize report: {error}")));
    let report_path = config
        .output_dir
        .join("multi-vision-smoke-report.redacted.json");
    if let Err(error) = fs::write(&report_path, &text) {
        fail(&format!(
            "failed to write report {}: {error}",
            report_path.display()
        ));
    }
    println!("{text}");
    if !report.ok {
        std::process::exit(1);
    }
}

fn run_camera(
    camera: CameraSource,
    config: ResolvedConfig,
    camera_index: usize,
    camera_count: usize,
) -> CameraReport {
    let source_kind = camera.source_kind();
    let camera_output_dir = config.output_dir.join(&camera.camera_id);
    if let Err(error) = fs::create_dir_all(&camera_output_dir) {
        return CameraReport::system_error(
            &camera.camera_id,
            &source_kind,
            &format!("failed to create camera output dir: {error}"),
        );
    }
    let args = match build_local_smoke_args(
        &camera,
        &config,
        camera_index,
        camera_count,
        &camera_output_dir,
    ) {
        Ok(args) => args,
        Err(error) => return CameraReport::system_error(&camera.camera_id, &source_kind, &error),
    };
    let output = Command::new(&config.local_smoke_bin)
        .args(&args)
        .stdin(Stdio::null())
        .output();
    let report_path = camera_output_dir.join("local-vision-smoke-report.json");
    match output {
        Ok(output) => {
            let exit_code = output.status.code();
            let stderr = String::from_utf8_lossy(&output.stderr);
            match read_single_report(&report_path) {
                Ok(report) => summarize_camera_report(
                    &camera.camera_id,
                    &source_kind,
                    exit_code,
                    &report,
                    if output.status.success() {
                        None
                    } else {
                        Some(sanitize_sensitive(&truncate(&stderr, 600)))
                    },
                    &camera.resolved_capture(&config, camera_index, camera_count),
                ),
                Err(error) => CameraReport::system_error(
                    &camera.camera_id,
                    &source_kind,
                    &format!("child report missing or invalid: {error}"),
                )
                .with_exit_code(exit_code),
            }
        }
        Err(error) => CameraReport::system_error(
            &camera.camera_id,
            &source_kind,
            &format!("failed to launch local vision smoke binary: {error}"),
        ),
    }
}

fn build_local_smoke_args(
    camera: &CameraSource,
    config: &ResolvedConfig,
    camera_index: usize,
    camera_count: usize,
    camera_output_dir: &Path,
) -> Result<Vec<String>, String> {
    let capture = camera.resolved_capture(config, camera_index, camera_count);
    let mut args = vec![
        "--camera-id".to_string(),
        camera.camera_id.clone(),
        "--duration-seconds".to_string(),
        camera
            .duration_seconds
            .unwrap_or(config.duration_seconds)
            .to_string(),
        "--interval-seconds".to_string(),
        camera
            .interval_seconds
            .unwrap_or(config.interval_seconds)
            .to_string(),
        "--phase-offset-ms".to_string(),
        capture.phase_offset_ms.to_string(),
        "--capture-mode".to_string(),
        capture.mode.clone(),
        "--max-frame-age-ms".to_string(),
        capture.max_frame_age_ms.to_string(),
        "--beacon-url".to_string(),
        config.beacon_url.clone(),
        "--output-dir".to_string(),
        camera_output_dir.to_string_lossy().to_string(),
        "--provider".to_string(),
        config.provider.clone(),
        "--redact-paths".to_string(),
    ];
    if let Some(capture_root) = capture.capture_root.as_ref() {
        args.push("--capture-root".to_string());
        args.push(capture_root.to_string_lossy().to_string());
    }
    if let Some(decode_backend) = capture.decode_backend.as_deref() {
        args.push("--decode-backend".to_string());
        args.push(decode_backend.to_string());
    }
    if config.no_post {
        args.push("--no-post".to_string());
    }
    let vlm_enrich = camera.vlm_enrich.unwrap_or(config.vlm_enrich);
    if vlm_enrich {
        args.push("--vlm-enrich".to_string());
        if let Some(api_base) = config.vlm_api_base.as_deref() {
            args.push("--vlm-api-base".to_string());
            args.push(api_base.to_string());
        }
        if let Some(model) = config.vlm_model.as_deref() {
            args.push("--vlm-model".to_string());
            args.push(model.to_string());
        }
        if let Some(api_key) = config.vlm_api_key.as_deref() {
            args.push("--vlm-api-key".to_string());
            args.push(api_key.to_string());
        }
        let sample_every = camera
            .vlm_sample_every
            .or(config.vlm_sample_every)
            .unwrap_or(30);
        let max_samples = camera
            .vlm_max_samples
            .or(config.vlm_max_samples)
            .unwrap_or(1);
        args.push("--vlm-sample-every".to_string());
        args.push(sample_every.to_string());
        args.push("--vlm-max-samples".to_string());
        args.push(max_samples.to_string());
        if let Some(queue) = config.vlm_queue.as_ref() {
            if queue.mode_name() == "global_serial" {
                if let Some(lock_path) = queue.lock_path.as_ref() {
                    args.push("--vlm-queue-lock-path".to_string());
                    args.push(lock_path.to_string_lossy().to_string());
                }
                if let Some(global_max_samples) = queue.global_max_samples {
                    args.push("--vlm-global-max-samples".to_string());
                    args.push(global_max_samples.to_string());
                }
                if let Some(trigger_policy) = queue.trigger_policy.as_deref() {
                    args.push("--vlm-trigger-policy".to_string());
                    args.push(trigger_policy.to_string());
                }
            }
        }
    }
    if let Some(command) = config.analyzer_command.as_deref() {
        args.push("--analyzer-command".to_string());
        args.push(command.to_string());
    }
    if let Some(path) = config.model_path.as_deref() {
        args.push("--model-path".to_string());
        args.push(path.to_string());
    }
    if let Some(path) = config.label_path.as_deref() {
        args.push("--label-path".to_string());
        args.push(path.to_string());
    }
    if camera.fixture.unwrap_or(false) {
        args.push("--fixture".to_string());
    } else if let Some(source_secret_ref) = capture.source_secret_ref.as_deref() {
        args.push("--source-secret-ref".to_string());
        args.push(source_secret_ref.to_string());
    } else if let Some(rtsp_url) = camera.rtsp_url.as_deref() {
        args.push("--rtsp-url".to_string());
        args.push(rtsp_url.to_string());
    } else if let Some(snapshot_url) = camera.snapshot_url.as_deref() {
        args.push("--snapshot-url".to_string());
        args.push(snapshot_url.to_string());
    } else {
        return Err("camera source must define rtspUrl, snapshotUrl, or fixture=true".to_string());
    }
    Ok(args)
}

fn read_single_report(path: &Path) -> Result<SingleSmokeReport, String> {
    let text = fs::read_to_string(path)
        .map_err(|error| format!("failed to read child report: {error}"))?;
    serde_json::from_str(&text).map_err(|error| format!("failed to parse child report: {error}"))
}

fn summarize_camera_report(
    camera_id: &str,
    source_kind: &str,
    exit_code: Option<i32>,
    report: &SingleSmokeReport,
    error: Option<String>,
    capture: &ResolvedCapture,
) -> CameraReport {
    let total_values = report
        .runs
        .iter()
        .map(|run| run.total_ms)
        .collect::<Vec<_>>();
    let capture_values = report
        .runs
        .iter()
        .map(|run| run.capture_ms)
        .collect::<Vec<_>>();
    let detector_values = report
        .runs
        .iter()
        .filter_map(|run| run.detector_ms)
        .collect::<Vec<_>>();
    let ingest_values = report
        .runs
        .iter()
        .filter_map(|run| run.event_ingest_ms)
        .collect::<Vec<_>>();
    let vlm_values = report
        .runs
        .iter()
        .filter_map(|run| run.vlm_ms)
        .collect::<Vec<_>>();
    let vlm_total = report.summary.vlm_total.max(
        report
            .runs
            .iter()
            .filter(|run| run.vlm_status.is_some())
            .count(),
    );
    let vlm_passed = report.summary.vlm_passed.max(
        report
            .runs
            .iter()
            .filter(|run| run.vlm_status.as_deref() == Some("active"))
            .count(),
    );
    let capture_read_values = report
        .runs
        .iter()
        .filter_map(|run| run.capture_read_ms)
        .collect::<Vec<_>>();
    let frame_age_values = report
        .runs
        .iter()
        .filter_map(|run| run.frame_age_ms)
        .collect::<Vec<_>>();
    let reconnect_count = report
        .runs
        .iter()
        .filter_map(|run| run.reconnect_count)
        .max()
        .unwrap_or(0);
    let max_stream_uptime_ms = report
        .runs
        .iter()
        .filter_map(|run| run.stream_uptime_ms)
        .max()
        .unwrap_or(0);
    let decode_backend = report
        .runs
        .iter()
        .filter_map(|run| run.decode_backend.clone())
        .next()
        .or_else(|| capture.decode_backend.clone());
    let failed_runs = report.runs.iter().filter(|run| !run.ok).count();
    let total = report.summary.total.max(report.runs.len());
    let passed = report
        .summary
        .passed
        .min(total)
        .max(total.saturating_sub(failed_runs));
    let failed = report.summary.failed.max(failed_runs);
    CameraReport {
        camera_id: if report.camera_id.is_empty() {
            camera_id.to_string()
        } else {
            report.camera_id.clone()
        },
        source_kind: source_kind.to_string(),
        capture_mode: report
            .capture_mode
            .clone()
            .or_else(|| {
                report
                    .runs
                    .iter()
                    .filter_map(|run| run.capture_mode.clone())
                    .next()
            })
            .unwrap_or_else(|| capture.mode.clone()),
        phase_offset_ms: capture.phase_offset_ms,
        ok: report.ok && failed == 0 && error.is_none(),
        child_exit_code: exit_code,
        total,
        passed,
        failed,
        success_rate: rate(passed, total),
        average_total_ms: report.summary.average_total_ms,
        p95_total_ms: report.summary.p95_total_ms.max(p95(&total_values)),
        max_total_ms: report.summary.max_total_ms,
        average_capture_ms: average_u64(&capture_values),
        p95_capture_ms: p95(&capture_values),
        average_capture_read_ms: average_u64(&capture_read_values),
        p95_capture_read_ms: report
            .summary
            .p95_capture_read_ms
            .max(p95(&capture_read_values)),
        average_frame_age_ms: average_u64(&frame_age_values),
        p95_frame_age_ms: report.summary.p95_frame_age_ms.max(p95(&frame_age_values)),
        max_stream_uptime_ms,
        reconnect_count,
        decode_backend,
        average_detector_ms: report.summary.average_detector_ms,
        p95_detector_ms: report.summary.p95_detector_ms.max(p95(&detector_values)),
        average_event_ingest_ms: report.summary.average_event_ingest_ms,
        p95_event_ingest_ms: report.summary.p95_event_ingest_ms.max(p95(&ingest_values)),
        detection_runs: report.summary.detection_runs,
        detection_count: report.summary.detection_count,
        vlm_total,
        vlm_passed,
        p95_vlm_ms: report.summary.p95_vlm_ms.max(p95(&vlm_values)),
        error,
    }
}

fn summarize_aggregate(cameras: &[CameraReport]) -> AggregateSummary {
    let total = cameras.iter().map(|camera| camera.total).sum::<usize>();
    let passed = cameras.iter().map(|camera| camera.passed).sum::<usize>();
    let failed = cameras.iter().map(|camera| camera.failed).sum::<usize>();
    AggregateSummary {
        total,
        passed,
        failed,
        success_rate: rate(passed, total),
        p95_total_ms: cameras
            .iter()
            .map(|camera| camera.p95_total_ms)
            .max()
            .unwrap_or(0),
        max_total_ms: cameras
            .iter()
            .map(|camera| camera.max_total_ms)
            .max()
            .unwrap_or(0),
        p95_capture_ms: cameras
            .iter()
            .map(|camera| camera.p95_capture_ms)
            .max()
            .unwrap_or(0),
        p95_capture_read_ms: cameras
            .iter()
            .map(|camera| camera.p95_capture_read_ms)
            .max()
            .unwrap_or(0),
        p95_frame_age_ms: cameras
            .iter()
            .map(|camera| camera.p95_frame_age_ms)
            .max()
            .unwrap_or(0),
        p95_detector_ms: cameras
            .iter()
            .map(|camera| camera.p95_detector_ms)
            .max()
            .unwrap_or(0),
        p95_event_ingest_ms: cameras
            .iter()
            .map(|camera| camera.p95_event_ingest_ms)
            .max()
            .unwrap_or(0),
        vlm_total: cameras.iter().map(|camera| camera.vlm_total).sum(),
        vlm_passed: cameras.iter().map(|camera| camera.vlm_passed).sum(),
        p95_vlm_ms: cameras
            .iter()
            .map(|camera| camera.p95_vlm_ms)
            .max()
            .unwrap_or(0),
    }
}

fn summarize_vlm_queue(
    queue: Option<&VlmQueueSettings>,
    aggregate: &AggregateSummary,
) -> Option<VlmQueueReport> {
    let queue = queue?;
    Some(VlmQueueReport {
        mode: queue.mode_name().to_string(),
        lock_path_redacted: queue.lock_path.is_some(),
        global_max_samples: queue.global_max_samples,
        trigger_policy: queue.trigger_policy.clone(),
        global_total: aggregate.vlm_total,
        global_passed: aggregate.vlm_passed,
        global_failed: aggregate.vlm_total.saturating_sub(aggregate.vlm_passed),
    })
}

fn summarize_sources(cameras: &[CameraSource]) -> SourceMix {
    let mut mix = SourceMix::default();
    for camera in cameras {
        match camera.source_kind().as_str() {
            "real" => mix.real += 1,
            "replay" => mix.replay += 1,
            "snapshot" => mix.snapshot += 1,
            "fixture" => mix.fixture += 1,
            _ => mix.other += 1,
        }
    }
    mix
}

fn classify_result(aggregate: &AggregateSummary, cameras: &[CameraReport]) -> String {
    if cameras.iter().any(|camera| camera.error.is_some()) {
        return "system-risk".to_string();
    }
    if aggregate.success_rate < 0.99 || aggregate.p95_total_ms >= 5000 {
        if (aggregate.p95_capture_ms >= 3000 || aggregate.p95_capture_read_ms >= 3000)
            && aggregate.p95_detector_ms < 1500
        {
            return "capture-bottleneck".to_string();
        }
        if aggregate.p95_detector_ms >= 3000 && aggregate.p95_capture_ms < 1500 {
            return "analyzer-bottleneck".to_string();
        }
        return "capacity-risk".to_string();
    }
    "pass".to_string()
}

fn resolve_config(cli: &Cli, manifest: &CameraManifest) -> ResolvedConfig {
    let provider = cli
        .provider
        .clone()
        .or_else(|| manifest.provider.clone())
        .unwrap_or_else(|| "cpu".to_string());
    if provider != "cpu" && provider != "spacemit" {
        fail("--provider or manifest provider must be cpu or spacemit");
    }
    let vlm_queue = cli_vlm_queue(cli).or_else(|| manifest.vlm_queue.clone());
    if let Some(queue) = vlm_queue.as_ref() {
        queue.validate();
    }
    ResolvedConfig {
        schema: manifest
            .schema
            .clone()
            .unwrap_or_else(|| "harbornavi.k3.multiVisionSmoke.v1".to_string()),
        output_dir: cli
            .output_dir
            .clone()
            .or_else(|| manifest.output_dir.clone())
            .unwrap_or_else(|| PathBuf::from("/tmp/harbornavi-p1/4ch-mixed-simulation")),
        duration_seconds: cli
            .duration_seconds
            .or(manifest.duration_seconds)
            .unwrap_or(1800),
        interval_seconds: cli
            .interval_seconds
            .or(manifest.interval_seconds)
            .unwrap_or(10),
        beacon_url: cli
            .beacon_url
            .clone()
            .or_else(|| manifest.beacon_url.clone())
            .unwrap_or_else(|| "http://127.0.0.1:4174".to_string()),
        analyzer_command: cli
            .analyzer_command
            .clone()
            .or_else(|| manifest.analyzer_command.clone()),
        model_path: cli
            .model_path
            .clone()
            .or_else(|| manifest.model_path.clone()),
        label_path: cli
            .label_path
            .clone()
            .or_else(|| manifest.label_path.clone()),
        provider,
        no_post: cli.no_post || manifest.no_post.unwrap_or(false),
        local_smoke_bin: cli.local_smoke_bin.clone(),
        capture: manifest.capture.clone().unwrap_or_default(),
        vlm_enrich: cli.vlm_enrich || manifest.vlm_enrich.unwrap_or(false),
        vlm_api_base: cli
            .vlm_api_base
            .clone()
            .or_else(|| manifest.vlm_api_base.clone()),
        vlm_model: cli.vlm_model.clone().or_else(|| manifest.vlm_model.clone()),
        vlm_api_key: cli.vlm_api_key.clone(),
        vlm_sample_every: cli.vlm_sample_every.or(manifest.vlm_sample_every),
        vlm_max_samples: cli.vlm_max_samples.or(manifest.vlm_max_samples),
        vlm_queue,
    }
}

fn cli_vlm_queue(cli: &Cli) -> Option<VlmQueueSettings> {
    if cli.vlm_queue_lock_path.is_none()
        && cli.vlm_global_max_samples.is_none()
        && cli.vlm_trigger_policy.is_none()
    {
        return None;
    }
    Some(VlmQueueSettings {
        mode: Some("global_serial".to_string()),
        lock_path: cli.vlm_queue_lock_path.clone(),
        global_max_samples: cli.vlm_global_max_samples,
        trigger_policy: cli.vlm_trigger_policy.clone(),
    })
}

fn load_manifest(path: &Path) -> CameraManifest {
    let text = fs::read_to_string(path).unwrap_or_else(|error| {
        fail(&format!(
            "failed to read camera manifest {}: {error}",
            path.display()
        ))
    });
    serde_json::from_str(&text).unwrap_or_else(|error| {
        fail(&format!(
            "failed to parse camera manifest {}: {error}",
            path.display()
        ))
    })
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

fn rate(passed: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        passed as f64 / total as f64
    }
}

fn truncate(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

fn sanitize_sensitive(value: &str) -> String {
    let mut output = String::new();
    for token in value.split_whitespace() {
        let lower = token.to_ascii_lowercase();
        if lower.contains("rtsp://") {
            output.push_str("rtsp://redacted ");
        } else if lower.contains("api_key")
            || lower.contains("apikey")
            || lower.contains("token")
            || lower.contains("private-key")
            || lower.contains("password")
            || lower.contains("credential")
        {
            output.push_str("[redacted-secret] ");
        } else if lower.starts_with("/tmp/")
            || lower.starts_with("/var/")
            || lower.starts_with("/home/")
            || lower.contains(":\\")
        {
            output.push_str("[redacted-local-path] ");
        } else {
            output.push_str(token);
            output.push(' ');
        }
    }
    output.trim().to_string()
}

fn default_local_smoke_bin() -> String {
    if let Ok(value) = std::env::var("HARBORNAVI_LOCAL_VISION_SMOKE_BIN") {
        return value;
    }
    if let Ok(current) = std::env::current_exe() {
        if let Some(parent) = current.parent() {
            let sibling = parent.join(format!(
                "harbornavi-k3-local-vision-smoke{}",
                std::env::consts::EXE_SUFFIX
            ));
            if sibling.exists() {
                return sibling.to_string_lossy().to_string();
            }
        }
    }
    "harbornavi-k3-local-vision-smoke".to_string()
}

impl CameraSource {
    fn resolved_capture(
        &self,
        config: &ResolvedConfig,
        camera_index: usize,
        camera_count: usize,
    ) -> ResolvedCapture {
        let camera_capture = self.capture.as_ref();
        let mode = self
            .capture_mode
            .clone()
            .or_else(|| camera_capture.and_then(|capture| capture.mode.clone()))
            .or_else(|| config.capture.mode.clone())
            .unwrap_or_else(|| "oneshot_ffmpeg".to_string());
        if !matches!(
            mode.as_str(),
            "oneshot_ffmpeg" | "persistent_ffmpeg" | "local_restream"
        ) {
            fail(&format!(
                "camera {} capture mode must be oneshot_ffmpeg, persistent_ffmpeg, or local_restream",
                self.camera_id
            ));
        }
        let interval_ms = config.interval_seconds.max(1).saturating_mul(1000);
        let auto_phase_ms = if camera_count > 1 {
            interval_ms.saturating_mul(camera_index as u64) / camera_count as u64
        } else {
            0
        };
        let phase_offset_ms = self
            .phase_offset_ms
            .or_else(|| camera_capture.and_then(|capture| capture.phase_offset_ms))
            .or_else(|| config.capture.phase_offset_ms)
            .unwrap_or(auto_phase_ms);
        let max_frame_age_ms = self
            .max_frame_age_ms
            .or_else(|| camera_capture.and_then(|capture| capture.max_frame_age_ms))
            .or_else(|| config.capture.max_frame_age_ms)
            .unwrap_or(2500);
        ResolvedCapture {
            mode,
            phase_offset_ms,
            max_frame_age_ms,
            source_secret_ref: self
                .source_secret_ref
                .clone()
                .or_else(|| camera_capture.and_then(|capture| capture.source_secret_ref.clone()))
                .or_else(|| config.capture.source_secret_ref.clone()),
            capture_root: self
                .capture_root
                .clone()
                .or_else(|| camera_capture.and_then(|capture| capture.capture_root.clone()))
                .or_else(|| config.capture.capture_root.clone()),
            decode_backend: self
                .decode_backend
                .clone()
                .or_else(|| camera_capture.and_then(|capture| capture.decode_backend.clone()))
                .or_else(|| config.capture.decode_backend.clone()),
        }
    }

    fn source_kind(&self) -> String {
        if let Some(kind) = self.kind.as_deref() {
            return kind.to_string();
        }
        if self.fixture.unwrap_or(false) {
            "fixture".to_string()
        } else if self.rtsp_url.is_some() || self.source_secret_ref.is_some() {
            "rtsp".to_string()
        } else if self.snapshot_url.is_some() {
            "snapshot".to_string()
        } else {
            "unknown".to_string()
        }
    }
}

impl VlmQueueSettings {
    fn mode_name(&self) -> &str {
        self.mode.as_deref().unwrap_or_else(|| {
            if self.lock_path.is_some() {
                "global_serial"
            } else {
                "none"
            }
        })
    }

    fn validate(&self) {
        match self.mode_name() {
            "global_serial" => {
                if self.lock_path.is_none() {
                    fail("vlmQueue.mode=global_serial requires vlmQueue.lockPath");
                }
            }
            "none" => {}
            other => fail(&format!(
                "vlmQueue.mode must be global_serial or none, got {other}"
            )),
        }
        if let Some(policy) = self.trigger_policy.as_deref() {
            if policy != "periodic" && policy != "detection_or_periodic" {
                fail("vlmQueue.triggerPolicy must be periodic or detection_or_periodic");
            }
        }
    }
}

impl CameraReport {
    fn system_error(camera_id: &str, source_kind: &str, error: &str) -> Self {
        Self {
            camera_id: camera_id.to_string(),
            source_kind: source_kind.to_string(),
            capture_mode: "unknown".to_string(),
            phase_offset_ms: 0,
            ok: false,
            child_exit_code: None,
            total: 0,
            passed: 0,
            failed: 0,
            success_rate: 0.0,
            average_total_ms: 0,
            p95_total_ms: 0,
            max_total_ms: 0,
            average_capture_ms: 0,
            p95_capture_ms: 0,
            average_capture_read_ms: 0,
            p95_capture_read_ms: 0,
            average_frame_age_ms: 0,
            p95_frame_age_ms: 0,
            max_stream_uptime_ms: 0,
            reconnect_count: 0,
            decode_backend: None,
            average_detector_ms: 0,
            p95_detector_ms: 0,
            average_event_ingest_ms: 0,
            p95_event_ingest_ms: 0,
            detection_runs: 0,
            detection_count: 0,
            vlm_total: 0,
            vlm_passed: 0,
            p95_vlm_ms: 0,
            error: Some(sanitize_sensitive(error)),
        }
    }

    fn with_exit_code(mut self, exit_code: Option<i32>) -> Self {
        self.child_exit_code = exit_code;
        self
    }
}

impl Cli {
    fn parse() -> Self {
        let args = std::env::args().skip(1).collect::<Vec<_>>();
        let mut cli = Self {
            camera_manifest: PathBuf::new(),
            output_dir: None,
            duration_seconds: None,
            interval_seconds: None,
            beacon_url: None,
            analyzer_command: None,
            model_path: None,
            label_path: None,
            provider: None,
            local_smoke_bin: default_local_smoke_bin(),
            no_post: false,
            vlm_enrich: false,
            vlm_api_base: None,
            vlm_model: None,
            vlm_api_key: std::env::var("HARBORNAVI_VLM_API_KEY").ok(),
            vlm_sample_every: None,
            vlm_max_samples: None,
            vlm_queue_lock_path: None,
            vlm_global_max_samples: None,
            vlm_trigger_policy: None,
        };
        let mut index = 0usize;
        while index < args.len() {
            match args[index].as_str() {
                "--camera-manifest" => {
                    cli.camera_manifest =
                        PathBuf::from(take_value(&args, &mut index, "--camera-manifest"))
                }
                "--output-dir" => {
                    cli.output_dir =
                        Some(PathBuf::from(take_value(&args, &mut index, "--output-dir")))
                }
                "--duration-seconds" => {
                    cli.duration_seconds = Some(parse_u64(&take_value(
                        &args,
                        &mut index,
                        "--duration-seconds",
                    )))
                }
                "--interval-seconds" => {
                    cli.interval_seconds = Some(parse_u64(&take_value(
                        &args,
                        &mut index,
                        "--interval-seconds",
                    )))
                }
                "--beacon-url" => {
                    cli.beacon_url = Some(take_value(&args, &mut index, "--beacon-url"))
                }
                "--analyzer-command" => {
                    cli.analyzer_command = Some(take_value(&args, &mut index, "--analyzer-command"))
                }
                "--model-path" => {
                    cli.model_path = Some(take_value(&args, &mut index, "--model-path"))
                }
                "--label-path" => {
                    cli.label_path = Some(take_value(&args, &mut index, "--label-path"))
                }
                "--provider" => cli.provider = Some(take_value(&args, &mut index, "--provider")),
                "--local-smoke-bin" => {
                    cli.local_smoke_bin = take_value(&args, &mut index, "--local-smoke-bin")
                }
                "--vlm-enrich" => cli.vlm_enrich = true,
                "--vlm-api-base" => {
                    cli.vlm_api_base = Some(take_value(&args, &mut index, "--vlm-api-base"))
                }
                "--vlm-model" => cli.vlm_model = Some(take_value(&args, &mut index, "--vlm-model")),
                "--vlm-api-key" => {
                    cli.vlm_api_key = Some(take_value(&args, &mut index, "--vlm-api-key"))
                }
                "--vlm-sample-every" => {
                    cli.vlm_sample_every = Some(parse_u64(&take_value(
                        &args,
                        &mut index,
                        "--vlm-sample-every",
                    )))
                }
                "--vlm-max-samples" => {
                    cli.vlm_max_samples = Some(parse_u64(&take_value(
                        &args,
                        &mut index,
                        "--vlm-max-samples",
                    )))
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
                    )))
                }
                "--vlm-trigger-policy" => {
                    cli.vlm_trigger_policy =
                        Some(take_value(&args, &mut index, "--vlm-trigger-policy"))
                }
                "--no-post" => cli.no_post = true,
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                other => fail(&format!("unknown argument {other}")),
            }
            index += 1;
        }
        if cli.camera_manifest.as_os_str().is_empty() {
            fail("missing required --camera-manifest");
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
        "Usage: harbornavi-k3-multi-vision-smoke --camera-manifest cameras.json [--output-dir PATH] [--duration-seconds N] [--interval-seconds N] [--beacon-url URL] [--analyzer-command PATH] [--model-path PATH] [--label-path PATH] [--provider cpu|spacemit] [--local-smoke-bin PATH] [--vlm-enrich] [--vlm-api-base URL] [--vlm-model MODEL] [--vlm-sample-every N] [--vlm-max-samples N] [--vlm-queue-lock-path PATH] [--vlm-global-max-samples N] [--vlm-trigger-policy periodic|detection_or_periodic] [--no-post]"
    );
}

fn epoch_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_camel_case_manifest() {
        let manifest: CameraManifest = serde_json::from_str(
            r#"{
                "schema": "harbornavi.k3.multiVisionSmoke.v1",
                "durationSeconds": 30,
                "intervalSeconds": 10,
                "provider": "cpu",
                "vlmQueue": {
                    "mode": "global_serial",
                    "lockPath": "/tmp/harbornavi-evt/test/vlm.queue.lock",
                    "globalMaxSamples": 4,
                    "triggerPolicy": "periodic"
                },
                "capture": {"mode": "persistent_ffmpeg", "maxFrameAgeMs": 2500},
                "cameras": [
                    {"cameraId": "cam-real", "kind": "real", "sourceSecretRef": "env:CAM_REAL_RTSP"},
                    {"cameraId": "cam-sim", "kind": "replay", "fixture": true, "capture": {"mode": "oneshot_ffmpeg", "phaseOffsetMs": 2500}}
                ]
            }"#,
        )
        .expect("manifest parses");
        assert_eq!(manifest.cameras.len(), 2);
        assert_eq!(manifest.cameras[0].camera_id, "cam-real");
        assert_eq!(
            manifest.cameras[0].source_secret_ref.as_deref(),
            Some("env:CAM_REAL_RTSP")
        );
        assert_eq!(manifest.cameras[1].source_kind(), "replay");
        assert_eq!(
            manifest.cameras[1]
                .capture
                .as_ref()
                .and_then(|capture| capture.phase_offset_ms),
            Some(2500)
        );
        let queue = manifest.vlm_queue.expect("vlm queue parses");
        assert_eq!(queue.mode_name(), "global_serial");
        assert_eq!(queue.global_max_samples, Some(4));
        assert_eq!(queue.trigger_policy.as_deref(), Some("periodic"));
    }

    #[test]
    fn default_phase_offsets_stagger_four_cameras() {
        let config = ResolvedConfig {
            schema: "test".to_string(),
            output_dir: PathBuf::from("/tmp/test"),
            duration_seconds: 30,
            interval_seconds: 10,
            beacon_url: "http://127.0.0.1:4174".to_string(),
            analyzer_command: None,
            model_path: None,
            label_path: None,
            provider: "cpu".to_string(),
            no_post: true,
            local_smoke_bin: "harbornavi-k3-local-vision-smoke".to_string(),
            capture: CaptureSettings::default(),
            vlm_enrich: false,
            vlm_api_base: None,
            vlm_model: None,
            vlm_api_key: None,
            vlm_sample_every: None,
            vlm_max_samples: None,
            vlm_queue: None,
        };
        let camera = CameraSource {
            camera_id: "cam-3".to_string(),
            kind: None,
            rtsp_url: Some("rtsp://example/redacted".to_string()),
            snapshot_url: None,
            fixture: None,
            duration_seconds: None,
            interval_seconds: None,
            source_secret_ref: None,
            capture_mode: None,
            phase_offset_ms: None,
            max_frame_age_ms: None,
            capture_root: None,
            decode_backend: None,
            capture: None,
            vlm_enrich: None,
            vlm_sample_every: None,
            vlm_max_samples: None,
        };

        assert_eq!(camera.resolved_capture(&config, 0, 4).phase_offset_ms, 0);
        assert_eq!(camera.resolved_capture(&config, 1, 4).phase_offset_ms, 2500);
        assert_eq!(camera.resolved_capture(&config, 2, 4).phase_offset_ms, 5000);
        assert_eq!(camera.resolved_capture(&config, 3, 4).phase_offset_ms, 7500);
    }

    #[test]
    fn vlm_manifest_options_are_passed_to_child_runner() {
        let config = ResolvedConfig {
            schema: "test".to_string(),
            output_dir: PathBuf::from("/tmp/test"),
            duration_seconds: 30,
            interval_seconds: 10,
            beacon_url: "http://127.0.0.1:4174".to_string(),
            analyzer_command: None,
            model_path: None,
            label_path: None,
            provider: "cpu".to_string(),
            no_post: false,
            local_smoke_bin: "harbornavi-k3-local-vision-smoke".to_string(),
            capture: CaptureSettings::default(),
            vlm_enrich: true,
            vlm_api_base: Some("http://127.0.0.1:8080/v1".to_string()),
            vlm_model: Some("local-vlm".to_string()),
            vlm_api_key: None,
            vlm_sample_every: Some(360),
            vlm_max_samples: Some(2),
            vlm_queue: Some(VlmQueueSettings {
                mode: Some("global_serial".to_string()),
                lock_path: Some(PathBuf::from("/tmp/harbornavi-vlm/vlm.queue.lock")),
                global_max_samples: Some(4),
                trigger_policy: Some("periodic".to_string()),
            }),
        };
        let camera = CameraSource {
            camera_id: "cam-vlm".to_string(),
            kind: None,
            rtsp_url: None,
            snapshot_url: None,
            fixture: Some(true),
            duration_seconds: None,
            interval_seconds: None,
            source_secret_ref: None,
            capture_mode: None,
            phase_offset_ms: None,
            max_frame_age_ms: None,
            capture_root: None,
            decode_backend: None,
            capture: None,
            vlm_enrich: None,
            vlm_sample_every: Some(120),
            vlm_max_samples: None,
        };

        let args = build_local_smoke_args(&camera, &config, 0, 4, Path::new("/tmp/test/cam-vlm"))
            .expect("args");

        assert!(args.contains(&"--vlm-enrich".to_string()));
        assert_arg_value(&args, "--vlm-api-base", "http://127.0.0.1:8080/v1");
        assert_arg_value(&args, "--vlm-model", "local-vlm");
        assert_arg_value(&args, "--vlm-sample-every", "120");
        assert_arg_value(&args, "--vlm-max-samples", "2");
        assert_arg_value(
            &args,
            "--vlm-queue-lock-path",
            "/tmp/harbornavi-vlm/vlm.queue.lock",
        );
        assert_arg_value(&args, "--vlm-global-max-samples", "4");
        assert_arg_value(&args, "--vlm-trigger-policy", "periodic");
        assert!(args.contains(&"--fixture".to_string()));
    }

    #[test]
    fn p95_uses_nearest_upper_rank() {
        assert_eq!(p95(&[1, 2, 3, 4, 5]), 5);
        assert_eq!(p95(&[50, 10, 30, 20]), 50);
        assert_eq!(p95(&[]), 0);
    }

    fn assert_arg_value(args: &[String], flag: &str, expected: &str) {
        let index = args
            .iter()
            .position(|arg| arg == flag)
            .unwrap_or_else(|| panic!("{flag} missing from args: {args:?}"));
        assert_eq!(args.get(index + 1).map(String::as_str), Some(expected));
    }

    #[test]
    fn vlm_queue_report_redacts_lock_path() {
        let queue = VlmQueueSettings {
            mode: Some("global_serial".to_string()),
            lock_path: Some(PathBuf::from("/tmp/harbornavi-secret/vlm.queue.lock")),
            global_max_samples: Some(4),
            trigger_policy: Some("periodic".to_string()),
        };
        let aggregate = AggregateSummary {
            total: 10,
            passed: 10,
            failed: 0,
            success_rate: 1.0,
            p95_total_ms: 1000,
            max_total_ms: 1200,
            p95_capture_ms: 100,
            p95_capture_read_ms: 20,
            p95_frame_age_ms: 200,
            p95_detector_ms: 300,
            p95_event_ingest_ms: 40,
            vlm_total: 4,
            vlm_passed: 3,
            p95_vlm_ms: 4500,
        };
        let report = summarize_vlm_queue(Some(&queue), &aggregate).expect("queue report");
        assert!(report.lock_path_redacted);
        assert_eq!(report.global_failed, 1);
        let serialized = serde_json::to_string(&report).expect("serialize queue report");
        assert!(!serialized.contains("harbornavi-secret"));
    }

    #[test]
    fn parses_snake_case_child_report() {
        let report: SingleSmokeReport = serde_json::from_str(
            r#"{
                "ok": true,
                "camera_id": "cam-a",
                "runtime_probe": {},
                "duration_seconds": 10,
                "interval_seconds": 10,
                "runs": [
                    {
                        "iteration": 1,
                        "ok": true,
                        "snapshot_path": "[redacted-local-path]",
                        "event": null,
                        "ingest_http_status": 200,
                        "capture_ms": 100,
                        "detector_ms": 200,
                        "analyze_ms": 10,
                        "event_ingest_ms": 30,
                        "total_ms": 350,
                        "capture_mode": "persistent_ffmpeg",
                        "capture_read_ms": 12,
                        "frame_age_ms": 400,
                        "stream_uptime_ms": 2000,
                        "reconnect_count": 0,
                        "decode_backend": "ffmpeg_sw_persistent",
                        "provider": "cpu",
                        "detection_count": 1,
                        "error": null
                    }
                ],
                "summary": {
                    "total": 1,
                    "passed": 1,
                    "failed": 0,
                    "average_total_ms": 350,
                    "average_detector_ms": 200,
                    "average_event_ingest_ms": 30,
                    "max_total_ms": 350,
                    "under_2s": 1,
                    "under_5s": 1,
                    "detection_runs": 1,
                    "detection_count": 1
                }
            }"#,
        )
        .expect("child report parses");
        assert_eq!(report.camera_id, "cam-a");
        assert_eq!(report.runs[0].capture_ms, 100);
        assert_eq!(report.runs[0].capture_read_ms, Some(12));
        assert_eq!(report.runs[0].frame_age_ms, Some(400));
    }

    #[test]
    fn classifies_pass_and_bottlenecks() {
        let pass = AggregateSummary {
            total: 100,
            passed: 100,
            failed: 0,
            success_rate: 1.0,
            p95_total_ms: 1200,
            max_total_ms: 2000,
            p95_capture_ms: 700,
            p95_capture_read_ms: 120,
            p95_frame_age_ms: 500,
            p95_detector_ms: 300,
            p95_event_ingest_ms: 50,
            ..AggregateSummary::default()
        };
        assert_eq!(classify_result(&pass, &[]), "pass");
        let capture = AggregateSummary {
            success_rate: 1.0,
            p95_total_ms: 6000,
            p95_capture_ms: 3500,
            p95_detector_ms: 700,
            ..AggregateSummary::default()
        };
        assert_eq!(classify_result(&capture, &[]), "capture-bottleneck");
        let analyzer = AggregateSummary {
            success_rate: 1.0,
            p95_total_ms: 6000,
            p95_capture_ms: 700,
            p95_detector_ms: 3500,
            ..AggregateSummary::default()
        };
        assert_eq!(classify_result(&analyzer, &[]), "analyzer-bottleneck");
    }
}
