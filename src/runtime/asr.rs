//! On-demand local speech-to-text support for knowledge-index audio files.
//!
//! The MVP deliberately shells out to a CPU-only `whisper-cli` process. The
//! process exits after each file, so model memory is returned to the OS and
//! never competes with the long-running GPU VLM runtime.

use std::env;
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::runtime::media_tools::{
    command_exists, ffmpeg_resolution_hint, resolve_ffmpeg_bin, resolve_ffprobe_bin,
};

const CACHE_SCHEMA_VERSION: u32 = 1;
const DEFAULT_TIMEOUT_SECONDS: u64 = 3_600;
const DEFAULT_THREADS: usize = 8;
const ASR_BIN_ENV: &str = "HARBOR_ASR_BIN";
const ASR_MODEL_ENV: &str = "HARBOR_ASR_MODEL";
const ASR_THREADS_ENV: &str = "HARBOR_ASR_THREADS";
const ASR_LANGUAGE_ENV: &str = "HARBOR_ASR_LANGUAGE";
const ASR_TIMEOUT_ENV: &str = "HARBOR_ASR_TIMEOUT_SECONDS";
const ASR_MIN_DURATION_ENV: &str = "HARBOR_ASR_MIN_DURATION_SECONDS";
const ASR_MAX_DURATION_ENV: &str = "HARBOR_ASR_MAX_DURATION_SECONDS";
const ASR_MAX_SOURCE_BYTES_ENV: &str = "HARBOR_ASR_MAX_SOURCE_BYTES";
const ASR_FFPROBE_TIMEOUT_ENV: &str = "HARBOR_ASR_FFPROBE_TIMEOUT_SECONDS";
const DEFAULT_MIN_DURATION_SECONDS: f64 = 1.0;
const DEFAULT_MAX_DURATION_SECONDS: f64 = 900.0;
const DEFAULT_MAX_SOURCE_BYTES: u64 = 256 * 1024 * 1024;
const DEFAULT_FFPROBE_TIMEOUT_SECONDS: u64 = 15;
static ASR_EXECUTION_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AsrSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AsrTranscript {
    pub provider_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub text: String,
    #[serde(default)]
    pub segments: Vec<AsrSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AsrError {
    Unavailable(String),
    Skipped(String),
    Failed(String),
}

impl fmt::Display for AsrError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable(message) => write!(formatter, "ASR unavailable: {message}"),
            Self::Skipped(message) => write!(formatter, "ASR skipped by policy: {message}"),
            Self::Failed(message) => write!(formatter, "ASR failed: {message}"),
        }
    }
}

#[derive(Debug, Clone)]
struct AsrRuntimeConfig {
    binary: String,
    model: PathBuf,
    threads: usize,
    language: String,
    timeout: Duration,
    min_duration_seconds: f64,
    max_duration_seconds: f64,
    max_source_bytes: u64,
    ffprobe_timeout: Duration,
    provider_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedTranscript {
    schema_version: u32,
    source_path: String,
    source_size: u64,
    source_modified_millis: u128,
    runtime_key: String,
    transcript: AsrTranscript,
}

pub fn runtime_available() -> bool {
    runtime_config().is_ok()
}

pub fn transcribe_cached(audio_path: &Path, index_root: &Path) -> Result<AsrTranscript, AsrError> {
    // Knowledge roots can be refreshed by separate background jobs. Serialize
    // ASR execution so concurrent roots cannot load multiple CPU models and
    // exhaust a small server with no swap.
    let _execution_guard = ASR_EXECUTION_LOCK.lock().map_err(|_| {
        AsrError::Failed("ASR execution lock is unavailable after a worker failure".to_string())
    })?;
    let config = runtime_config()?;
    let source = source_identity(audio_path)?;
    validate_source_size(source.size, config.max_source_bytes)?;
    let duration_seconds = probe_audio_duration_seconds(audio_path, config.ffprobe_timeout)?;
    validate_audio_duration(
        duration_seconds,
        config.min_duration_seconds,
        config.max_duration_seconds,
    )?;
    let cache_path = transcript_cache_path(index_root, audio_path);

    if let Some(cached) = load_cache(&cache_path) {
        if cached.schema_version == CACHE_SCHEMA_VERSION
            && cached.source_path == source.path
            && cached.source_size == source.size
            && cached.source_modified_millis == source.modified_millis
            && cached.runtime_key == config.provider_key
        {
            return Ok(cached.transcript);
        }
    }

    let transcript = transcribe_with_config(audio_path, &config)?;
    let cached = CachedTranscript {
        schema_version: CACHE_SCHEMA_VERSION,
        source_path: source.path,
        source_size: source.size,
        source_modified_millis: source.modified_millis,
        runtime_key: config.provider_key,
        transcript: transcript.clone(),
    };
    persist_cache(&cache_path, &cached)?;
    Ok(transcript)
}

fn runtime_config() -> Result<AsrRuntimeConfig, AsrError> {
    let binary = env::var(ASR_BIN_ENV).unwrap_or_else(|_| "whisper-cli".to_string());
    if !command_exists(&binary) {
        return Err(AsrError::Unavailable(format!(
            "set {ASR_BIN_ENV} or place whisper-cli on PATH"
        )));
    }

    let model = env::var_os(ASR_MODEL_ENV)
        .map(PathBuf::from)
        .filter(|path| path.is_file())
        .ok_or_else(|| {
            AsrError::Unavailable(format!(
                "set {ASR_MODEL_ENV} to an existing multilingual whisper.cpp model"
            ))
        })?;
    let threads = env::var(ASR_THREADS_ENV)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_THREADS)
        .min(32);
    let timeout_seconds = env::var(ASR_TIMEOUT_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_TIMEOUT_SECONDS);
    let language = env::var(ASR_LANGUAGE_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "auto".to_string());
    let min_duration_seconds = env::var(ASR_MIN_DURATION_ENV)
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
        .unwrap_or(DEFAULT_MIN_DURATION_SECONDS);
    let max_duration_seconds = env::var(ASR_MAX_DURATION_ENV)
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(DEFAULT_MAX_DURATION_SECONDS);
    let max_source_bytes = env::var(ASR_MAX_SOURCE_BYTES_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_MAX_SOURCE_BYTES);
    let ffprobe_timeout_seconds = env::var(ASR_FFPROBE_TIMEOUT_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(DEFAULT_FFPROBE_TIMEOUT_SECONDS);
    let (min_duration_seconds, max_duration_seconds, max_source_bytes, ffprobe_timeout_seconds) =
        enforce_asr_policy_limits(
            min_duration_seconds,
            max_duration_seconds,
            max_source_bytes,
            ffprobe_timeout_seconds,
        );
    let model_metadata = fs::metadata(&model).map_err(|error| {
        AsrError::Unavailable(format!("cannot inspect model {}: {error}", model.display()))
    })?;
    let model_modified = model_metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_millis())
        .unwrap_or_default();
    let provider_key = format!(
        "whisper.cpp:cpu:{}:{}:{}",
        model
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("model"),
        model_metadata.len(),
        model_modified
    );

    Ok(AsrRuntimeConfig {
        binary,
        model,
        threads,
        language,
        timeout: Duration::from_secs(timeout_seconds),
        min_duration_seconds,
        max_duration_seconds,
        max_source_bytes,
        ffprobe_timeout: Duration::from_secs(ffprobe_timeout_seconds),
        provider_key,
    })
}

fn enforce_asr_policy_limits(
    min_duration_seconds: f64,
    max_duration_seconds: f64,
    max_source_bytes: u64,
    ffprobe_timeout_seconds: u64,
) -> (f64, f64, u64, u64) {
    let max_duration_seconds = max_duration_seconds.min(DEFAULT_MAX_DURATION_SECONDS);
    (
        min_duration_seconds.min(max_duration_seconds),
        max_duration_seconds,
        max_source_bytes.min(DEFAULT_MAX_SOURCE_BYTES),
        ffprobe_timeout_seconds.min(DEFAULT_FFPROBE_TIMEOUT_SECONDS),
    )
}

fn probe_audio_duration_seconds(audio_path: &Path, timeout: Duration) -> Result<f64, AsrError> {
    let ffprobe = resolve_ffprobe_bin().ok_or_else(|| {
        AsrError::Unavailable(
            "ffprobe is required to enforce the minimum supported audio duration".to_string(),
        )
    })?;
    let mut command = Command::new(ffprobe);
    command
        .arg("-v")
        .arg("error")
        .arg("-show_entries")
        .arg("format=duration")
        .arg("-of")
        .arg("default=noprint_wrappers=1:nokey=1")
        .arg(audio_path);
    let (status, stdout, stderr) =
        run_command_capture(&mut command, timeout, "audio duration probe", true)?;
    if !status.success() {
        return Err(AsrError::Failed(format!(
            "ffprobe could not read audio duration for {}: {}",
            audio_path.display(),
            compact_log(&stderr)
        )));
    }
    let duration_seconds = stdout.trim().parse::<f64>().map_err(|error| {
        AsrError::Failed(format!(
            "ffprobe returned an invalid audio duration for {}: {error}",
            audio_path.display()
        ))
    })?;
    validate_audio_duration(duration_seconds, 0.0, f64::MAX)?;
    Ok(duration_seconds)
}

fn validate_source_size(source_size: u64, max_source_bytes: u64) -> Result<(), AsrError> {
    if source_size > max_source_bytes {
        return Err(AsrError::Skipped(format!(
            "audio source is {source_size} bytes, above the supported maximum {max_source_bytes} bytes"
        )));
    }
    Ok(())
}

fn validate_audio_duration(
    duration_seconds: f64,
    min_duration_seconds: f64,
    max_duration_seconds: f64,
) -> Result<(), AsrError> {
    if !duration_seconds.is_finite() || duration_seconds <= 0.0 {
        return Err(AsrError::Failed(format!(
            "audio duration must be positive, got {duration_seconds}"
        )));
    }
    if duration_seconds < min_duration_seconds {
        return Err(AsrError::Skipped(format!(
            "audio duration {duration_seconds:.2}s is below the supported minimum \
             {min_duration_seconds:.2}s; provide a longer recording"
        )));
    }
    if duration_seconds > max_duration_seconds {
        return Err(AsrError::Skipped(format!(
            "audio duration {duration_seconds:.2}s exceeds the supported maximum \
             {max_duration_seconds:.2}s"
        )));
    }
    Ok(())
}

fn transcribe_with_config(
    audio_path: &Path,
    config: &AsrRuntimeConfig,
) -> Result<AsrTranscript, AsrError> {
    let ffmpeg = resolve_ffmpeg_bin().ok_or_else(|| {
        AsrError::Unavailable(format!(
            "ffmpeg is required to normalize audio; {}",
            ffmpeg_resolution_hint()
        ))
    })?;
    let work_dir = env::temp_dir().join(format!("harborbeacon-asr-{}", Uuid::new_v4().as_simple()));
    fs::create_dir_all(&work_dir).map_err(|error| {
        AsrError::Failed(format!(
            "cannot create temporary ASR directory {}: {error}",
            work_dir.display()
        ))
    })?;
    let normalized_audio = work_dir.join("audio.wav");
    let output_prefix = work_dir.join("transcript");
    let ffmpeg_log = work_dir.join("ffmpeg.log");
    let whisper_log = work_dir.join("whisper.log");

    let result = (|| {
        let mut ffmpeg_command = Command::new(&ffmpeg);
        ffmpeg_command
            .arg("-nostdin")
            .arg("-v")
            .arg("error")
            .arg("-y")
            .arg("-i")
            .arg(audio_path)
            .arg("-vn")
            .arg("-ar")
            .arg("16000")
            .arg("-ac")
            .arg("1")
            .arg("-c:a")
            .arg("pcm_s16le")
            .arg(&normalized_audio);
        run_command(
            &mut ffmpeg_command,
            &ffmpeg_log,
            Duration::from_secs(300),
            "audio normalization",
        )?;

        let mut whisper_command = Command::new(&config.binary);
        whisper_command
            .arg("-m")
            .arg(&config.model)
            .arg("-f")
            .arg(&normalized_audio)
            .arg("-l")
            .arg(&config.language)
            .arg("-t")
            .arg(config.threads.to_string())
            .arg("-ng")
            .arg("-oj")
            .arg("-of")
            .arg(&output_prefix)
            .arg("-np");
        run_command(
            &mut whisper_command,
            &whisper_log,
            config.timeout,
            "speech transcription",
        )?;

        let json_path = output_prefix.with_extension("json");
        let bytes = fs::read(&json_path).map_err(|error| {
            AsrError::Failed(format!(
                "whisper.cpp did not produce {}: {error}",
                json_path.display()
            ))
        })?;
        let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
            AsrError::Failed(format!("invalid whisper.cpp JSON output: {error}"))
        })?;
        parse_whisper_json(&value, config.provider_key.clone())
    })();

    let _ = fs::remove_dir_all(&work_dir);
    result
}

fn run_command(
    command: &mut Command,
    log_path: &Path,
    timeout: Duration,
    label: &str,
) -> Result<ExitStatus, AsrError> {
    let stdout = fs::File::create(log_path).map_err(|error| {
        AsrError::Failed(format!("cannot create {} log: {error}", log_path.display()))
    })?;
    let stderr = stdout.try_clone().map_err(|error| {
        AsrError::Failed(format!("cannot clone {} log: {error}", log_path.display()))
    })?;
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(|error| AsrError::Failed(format!("cannot start {label}: {error}")))?;
    let status = wait_for_child(&mut child, timeout, label, false)?;
    if status.success() {
        Ok(status)
    } else {
        let detail = fs::read_to_string(log_path).unwrap_or_default();
        Err(AsrError::Failed(format!(
            "{label} exited with {status}: {}",
            compact_log(&detail)
        )))
    }
}

fn run_command_capture(
    command: &mut Command,
    timeout: Duration,
    label: &str,
    timeout_is_skipped: bool,
) -> Result<(ExitStatus, String, String), AsrError> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| AsrError::Failed(format!("cannot start {label}: {error}")))?;
    let status = wait_for_child(&mut child, timeout, label, timeout_is_skipped)?;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_end(&mut stdout);
    }
    if let Some(mut pipe) = child.stderr.take() {
        let _ = pipe.read_to_end(&mut stderr);
    }
    Ok((
        status,
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
    ))
}

fn wait_for_child(
    child: &mut Child,
    timeout: Duration,
    label: &str,
    timeout_is_skipped: bool,
) -> Result<ExitStatus, AsrError> {
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if started.elapsed() < timeout => {
                thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let message = format!("{label} exceeded {} seconds", timeout.as_secs());
                return Err(if timeout_is_skipped {
                    AsrError::Skipped(message)
                } else {
                    AsrError::Failed(message)
                });
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(AsrError::Failed(format!(
                    "cannot wait for {label}: {error}"
                )));
            }
        }
    }
}

fn parse_whisper_json(value: &Value, provider_key: String) -> Result<AsrTranscript, AsrError> {
    let language = value
        .pointer("/result/language")
        .or_else(|| value.get("language"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let mut segments = Vec::new();

    if let Some(items) = value.get("transcription").and_then(Value::as_array) {
        for item in items {
            let text = item
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if text.is_empty() {
                continue;
            }
            let start_ms = item
                .pointer("/offsets/from")
                .and_then(Value::as_u64)
                .or_else(|| {
                    item.pointer("/timestamps/from")
                        .and_then(Value::as_str)
                        .and_then(parse_timestamp_ms)
                })
                .unwrap_or_default();
            let end_ms = item
                .pointer("/offsets/to")
                .and_then(Value::as_u64)
                .or_else(|| {
                    item.pointer("/timestamps/to")
                        .and_then(Value::as_str)
                        .and_then(parse_timestamp_ms)
                })
                .unwrap_or(start_ms);
            segments.push(AsrSegment {
                start_ms,
                end_ms: end_ms.max(start_ms),
                text,
            });
        }
    } else if let Some(items) = value.get("segments").and_then(Value::as_array) {
        for item in items {
            let text = item
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_string();
            if text.is_empty() {
                continue;
            }
            let start_ms = seconds_value_to_ms(item.get("start")).unwrap_or_default();
            let end_ms = seconds_value_to_ms(item.get("end")).unwrap_or(start_ms);
            segments.push(AsrSegment {
                start_ms,
                end_ms: end_ms.max(start_ms),
                text,
            });
        }
    }

    let text = if segments.is_empty() {
        value
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string()
    } else {
        segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<Vec<_>>()
            .join(" ")
    };
    if text.is_empty() {
        return Err(AsrError::Failed(
            "whisper.cpp returned no speech text".to_string(),
        ));
    }

    Ok(AsrTranscript {
        provider_key,
        language,
        text,
        segments,
    })
}

fn seconds_value_to_ms(value: Option<&Value>) -> Option<u64> {
    value.and_then(|value| {
        value
            .as_f64()
            .map(|seconds| (seconds.max(0.0) * 1_000.0).round() as u64)
            .or_else(|| {
                value
                    .as_str()
                    .and_then(|text| text.parse::<f64>().ok())
                    .map(|seconds| (seconds.max(0.0) * 1_000.0).round() as u64)
            })
    })
}

fn parse_timestamp_ms(value: &str) -> Option<u64> {
    let normalized = value.trim().replace(',', ".");
    let mut parts = normalized.split(':').collect::<Vec<_>>();
    if parts.len() == 2 {
        parts.insert(0, "0");
    }
    if parts.len() != 3 {
        return None;
    }
    let hours = parts[0].parse::<u64>().ok()?;
    let minutes = parts[1].parse::<u64>().ok()?;
    let seconds = parts[2].parse::<f64>().ok()?;
    Some(
        hours
            .saturating_mul(3_600_000)
            .saturating_add(minutes.saturating_mul(60_000))
            .saturating_add((seconds.max(0.0) * 1_000.0).round() as u64),
    )
}

fn transcript_cache_path(index_root: &Path, audio_path: &Path) -> PathBuf {
    let mut hasher = Sha256::new();
    hasher.update(audio_path.to_string_lossy().as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    index_root
        .join("asr-transcripts")
        .join(format!("{digest}.json"))
}

fn load_cache(path: &Path) -> Option<CachedTranscript> {
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn persist_cache(path: &Path, cached: &CachedTranscript) -> Result<(), AsrError> {
    let parent = path
        .parent()
        .ok_or_else(|| AsrError::Failed(format!("invalid ASR cache path {}", path.display())))?;
    fs::create_dir_all(parent).map_err(|error| {
        AsrError::Failed(format!(
            "cannot create ASR cache directory {}: {error}",
            parent.display()
        ))
    })?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("transcript"),
        Uuid::new_v4().as_simple()
    ));
    let bytes = serde_json::to_vec_pretty(cached)
        .map_err(|error| AsrError::Failed(format!("cannot encode ASR cache: {error}")))?;
    fs::write(&temporary, bytes).map_err(|error| {
        AsrError::Failed(format!(
            "cannot write ASR cache {}: {error}",
            temporary.display()
        ))
    })?;
    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        AsrError::Failed(format!(
            "cannot publish ASR cache {}: {error}",
            path.display()
        ))
    })
}

struct SourceIdentity {
    path: String,
    size: u64,
    modified_millis: u128,
}

fn source_identity(path: &Path) -> Result<SourceIdentity, AsrError> {
    let metadata = fs::metadata(path).map_err(|error| {
        AsrError::Failed(format!("cannot inspect audio {}: {error}", path.display()))
    })?;
    let modified_millis = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_millis())
        .unwrap_or_default();
    Ok(SourceIdentity {
        path: path.to_string_lossy().into_owned(),
        size: metadata.len(),
        modified_millis,
    })
}

fn compact_log(value: &str) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= 400 {
        compact
    } else {
        format!("{}...", compact.chars().take(400).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::env;
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use std::sync::Mutex;
    #[cfg(unix)]
    use std::time::Duration;

    use serde_json::json;

    use super::{
        compact_log, enforce_asr_policy_limits, parse_timestamp_ms, parse_whisper_json,
        validate_audio_duration, validate_source_size, AsrError, AsrSegment,
    };
    #[cfg(unix)]
    use super::{
        probe_audio_duration_seconds, transcribe_cached, ASR_BIN_ENV, ASR_FFPROBE_TIMEOUT_ENV,
        ASR_LANGUAGE_ENV, ASR_MAX_DURATION_ENV, ASR_MAX_SOURCE_BYTES_ENV, ASR_MIN_DURATION_ENV,
        ASR_MODEL_ENV, ASR_THREADS_ENV,
    };

    #[cfg(unix)]
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn parses_whisper_cpp_transcription_with_offsets() {
        let value = json!({
            "result": {"language": "zh"},
            "transcription": [
                {
                    "timestamps": {"from": "00:00:00,000", "to": "00:00:02,500"},
                    "offsets": {"from": 0, "to": 2500},
                    "text": "你好，世界"
                },
                {
                    "timestamps": {"from": "00:00:02,500", "to": "00:00:04,000"},
                    "offsets": {"from": 2500, "to": 4000},
                    "text": "这是测试"
                }
            ]
        });
        let transcript =
            parse_whisper_json(&value, "whisper.cpp:test".to_string()).expect("transcript");

        assert_eq!(transcript.language.as_deref(), Some("zh"));
        assert_eq!(transcript.text, "你好，世界 这是测试");
        assert_eq!(
            transcript.segments,
            vec![
                AsrSegment {
                    start_ms: 0,
                    end_ms: 2500,
                    text: "你好，世界".to_string(),
                },
                AsrSegment {
                    start_ms: 2500,
                    end_ms: 4000,
                    text: "这是测试".to_string(),
                }
            ]
        );
    }

    #[test]
    fn parses_openai_style_segments() {
        let value = json!({
            "language": "en",
            "segments": [
                {"start": 1.25, "end": 2.75, "text": "hello world"}
            ]
        });
        let transcript =
            parse_whisper_json(&value, "whisper.cpp:test".to_string()).expect("transcript");

        assert_eq!(transcript.segments[0].start_ms, 1250);
        assert_eq!(transcript.segments[0].end_ms, 2750);
        assert_eq!(transcript.text, "hello world");
    }

    #[test]
    fn parses_timestamp_variants() {
        assert_eq!(parse_timestamp_ms("00:01:02.345"), Some(62_345));
        assert_eq!(parse_timestamp_ms("01:02,500"), Some(62_500));
        assert_eq!(parse_timestamp_ms("invalid"), None);
    }

    #[test]
    fn short_audio_is_skipped_with_an_explicit_policy_reason() {
        let error =
            validate_audio_duration(0.3, 1.0, 900.0).expect_err("short audio must be skipped");
        assert!(matches!(error, AsrError::Skipped(_)));
        assert_eq!(
            error.to_string(),
            "ASR skipped by policy: audio duration 0.30s is below the supported minimum 1.00s; \
             provide a longer recording"
        );
        assert!(validate_audio_duration(1.0, 1.0, 900.0).is_ok());
    }

    #[test]
    fn oversized_or_overlong_audio_is_skipped_before_transcription() {
        assert!(matches!(
            validate_source_size(268_435_457, 268_435_456),
            Err(AsrError::Skipped(_))
        ));
        assert!(matches!(
            validate_audio_duration(900.1, 1.0, 900.0),
            Err(AsrError::Skipped(_))
        ));
    }

    #[test]
    fn asr_environment_limits_can_only_tighten_hard_policy_caps() {
        let (minimum, maximum, bytes, probe_seconds) =
            enforce_asr_policy_limits(1_000.0, 10_000.0, u64::MAX, u64::MAX);

        assert_eq!(minimum, 900.0);
        assert_eq!(maximum, 900.0);
        assert_eq!(bytes, 256 * 1024 * 1024);
        assert_eq!(probe_seconds, 15);

        let (minimum, maximum, bytes, probe_seconds) =
            enforce_asr_policy_limits(1.0, 120.0, 1024, 2);
        assert_eq!(
            (minimum, maximum, bytes, probe_seconds),
            (1.0, 120.0, 1024, 2)
        );
    }

    #[test]
    fn compact_log_truncates_utf8_on_character_boundaries() {
        let compact = compact_log(&"界".repeat(500));
        assert_eq!(compact.chars().count(), 403);
        assert!(compact.ends_with("..."));
    }

    #[cfg(unix)]
    #[test]
    fn ffprobe_timeout_is_a_skipped_policy_result() {
        let _guard = ENV_LOCK.lock().expect("environment lock");
        let root = env::temp_dir().join(format!(
            "harborbeacon-asr-ffprobe-timeout-{}",
            uuid::Uuid::new_v4().as_simple()
        ));
        fs::create_dir_all(&root).expect("create root");
        let audio = root.join("meeting.mp3");
        let ffprobe = root.join("fake-ffprobe");
        fs::write(&audio, b"audio").expect("write audio");
        fs::write(&ffprobe, "#!/bin/sh\nsleep 2\nprintf '2.5\\n'\n").expect("write ffprobe");
        let mut permissions = fs::metadata(&ffprobe).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&ffprobe, permissions).expect("make executable");
        env::set_var("HARBOR_FFPROBE_BIN", &ffprobe);

        let error = probe_audio_duration_seconds(&audio, Duration::from_millis(100))
            .expect_err("probe must time out");

        assert!(matches!(error, AsrError::Skipped(_)));
        env::remove_var("HARBOR_FFPROBE_BIN");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn on_demand_runtime_transcribes_once_then_reuses_cache() {
        let _guard = ENV_LOCK.lock().expect("environment lock");
        let root = env::temp_dir().join(format!(
            "harborbeacon-asr-runtime-test-{}",
            uuid::Uuid::new_v4().as_simple()
        ));
        let index_root = root.join("index");
        fs::create_dir_all(&index_root).expect("create test root");
        let audio = root.join("meeting.mp3");
        let model = root.join("ggml-small-q5_1.bin");
        let ffmpeg = root.join("fake-ffmpeg");
        let ffprobe = root.join("fake-ffprobe");
        let whisper = root.join("fake-whisper");
        let invocation_log = root.join("whisper-invocations");
        fs::write(&audio, b"fake audio").expect("write audio");
        fs::write(&model, b"fake model").expect("write model");
        fs::write(
            &ffmpeg,
            "#!/bin/sh\ninput=''\nprevious=''\nfor arg in \"$@\"; do\n  if [ \"$previous\" = '-i' ]; then input=\"$arg\"; fi\n  previous=\"$arg\"\ndone\nlast=''\nfor arg in \"$@\"; do last=\"$arg\"; done\ncp \"$input\" \"$last\"\n",
        )
        .expect("write fake ffmpeg");
        fs::write(&ffprobe, "#!/bin/sh\nprintf '2.5\\n'\n").expect("write fake ffprobe");
        fs::write(
            &whisper,
            format!(
                "#!/bin/sh\nout=''\nprevious=''\nfor arg in \"$@\"; do\n  if [ \"$previous\" = '-of' ]; then out=\"$arg\"; fi\n  previous=\"$arg\"\ndone\necho run >> '{}'\nprintf '%s' '{{\"result\":{{\"language\":\"zh\"}},\"transcription\":[{{\"offsets\":{{\"from\":1000,\"to\":2500}},\"text\":\"会议将在九点开始\"}}]}}' > \"${{out}}.json\"\n",
                invocation_log.display()
            ),
        )
        .expect("write fake whisper");
        for script in [&ffmpeg, &ffprobe, &whisper] {
            let mut permissions = fs::metadata(script).expect("script metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(script, permissions).expect("make script executable");
        }

        env::set_var("HARBOR_FFMPEG_BIN", &ffmpeg);
        env::set_var("HARBOR_FFPROBE_BIN", &ffprobe);
        env::set_var(ASR_BIN_ENV, &whisper);
        env::set_var(ASR_MODEL_ENV, &model);
        env::set_var(ASR_THREADS_ENV, "2");
        env::set_var(ASR_LANGUAGE_ENV, "auto");
        env::set_var(ASR_MIN_DURATION_ENV, "1.0");
        env::set_var(ASR_MAX_DURATION_ENV, "900");
        env::set_var(ASR_MAX_SOURCE_BYTES_ENV, "268435456");
        env::set_var(ASR_FFPROBE_TIMEOUT_ENV, "15");

        let first = transcribe_cached(&audio, &index_root).expect("first transcript");
        let second = transcribe_cached(&audio, &index_root).expect("cached transcript");

        assert_eq!(first, second);
        assert_eq!(first.text, "会议将在九点开始");
        assert_eq!(first.segments[0].start_ms, 1000);
        assert_eq!(
            fs::read_to_string(&invocation_log)
                .expect("invocation log")
                .lines()
                .count(),
            1
        );

        for key in [
            "HARBOR_FFMPEG_BIN",
            "HARBOR_FFPROBE_BIN",
            ASR_BIN_ENV,
            ASR_MODEL_ENV,
            ASR_THREADS_ENV,
            ASR_LANGUAGE_ENV,
            ASR_MIN_DURATION_ENV,
            ASR_MAX_DURATION_ENV,
            ASR_MAX_SOURCE_BYTES_ENV,
            ASR_FFPROBE_TIMEOUT_ENV,
        ] {
            env::remove_var(key);
        }
        let _ = fs::remove_dir_all(root);
    }
}
