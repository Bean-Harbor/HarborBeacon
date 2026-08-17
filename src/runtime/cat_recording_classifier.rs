//! Bounded contract for the K3 MobileNetV2 cat-recording verifier.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

pub const CAT_RECORDING_VALIDATOR_ENV: &str = "HARBOR_K3_CAT_RECORDING_VALIDATOR";
pub const CAT_RECORDING_CLASSIFIER_BIN_ENV: &str = "HARBOR_K3_CAT_RECORDING_CLASSIFIER_BIN";
pub const CAT_RECORDING_CLASSIFIER_MODEL_ENV: &str = "HARBOR_K3_CAT_RECORDING_CLASSIFIER_MODEL";
pub const CAT_RECORDING_CLASSIFIER_MODEL_SHA256_ENV: &str =
    "HARBOR_K3_CAT_RECORDING_CLASSIFIER_MODEL_SHA256";
pub const CAT_RECORDING_CLASSIFIER_THRESHOLD_ENV: &str =
    "HARBOR_K3_CAT_RECORDING_CLASSIFIER_THRESHOLD";
pub const CAT_RECORDING_CLASSIFIER_AI_THREADS_ENV: &str =
    "HARBOR_K3_CAT_RECORDING_CLASSIFIER_AI_THREADS";
pub const CAT_RECORDING_CLASSIFIER_AFFINITY_ENV: &str =
    "HARBOR_K3_CAT_RECORDING_CLASSIFIER_AFFINITY";

pub const CAT_RECORDING_CLASSIFIER_MAX_FRAMES: usize = 9;
pub const CAT_RECORDING_CLASSIFIER_MIN_POSITIVE_FRAMES: usize = 3;
pub const CAT_RECORDING_CLASSIFIER_MODEL_NAME: &str = "mobilenetv2-cat-binary-v2-int8";
pub const CAT_RECORDING_CLASSIFIER_MODEL_SHA256: &str =
    "d0c1bdcf973ca7f6efc6e62af764ff59300e0d27abbc75c20c7f86515769d825";

const DEFAULT_CLASSIFIER_BIN: &str =
    "/usr/lib/harboros-beacon/harbornavi_k3_cat_recording_classifier.py";
const DEFAULT_CLASSIFIER_MODEL: &str = "/usr/share/harboros-beacon/vision-models/\
mobilenetv2-cat-binary-v2-20260806/mobilenetv2_cat_binary_int8.onnx";
const DEFAULT_THRESHOLD_PPM: u32 = 620_000;
const DEFAULT_AI_THREADS: usize = 4;
const DEFAULT_AFFINITY: &str = "12;13;14;15";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatRecordingValidatorBackend {
    MobileNetV2Int8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatRecordingClassifierConfig {
    pub python_bin: String,
    pub classifier_bin: PathBuf,
    pub model_path: PathBuf,
    pub expected_model_sha256: String,
    pub threshold_ppm: u32,
    pub ai_threads: usize,
    pub affinity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatRecordingFramePrediction {
    pub frame_index: u8,
    pub cat_probability_ppm: u32,
    #[serde(default)]
    pub inference_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatRecordingAggregation {
    pub cat_present: bool,
    pub cat_frame_indices: Vec<u8>,
    pub reason_code: String,
    pub frame_predictions: Vec<CatRecordingFramePrediction>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CatRecordingClassifierOutput {
    pub schema_version: String,
    pub status: String,
    pub provider: String,
    pub model_name: String,
    pub model_sha256: String,
    pub threshold_ppm: u32,
    pub sampled_frame_count: u8,
    pub predictions: Vec<CatRecordingFramePrediction>,
    #[serde(default)]
    pub session_creation_ms: u64,
    #[serde(default)]
    pub total_inference_ms: u64,
    #[serde(default)]
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CatRecordingClassifierProbeOutput {
    pub schema_version: String,
    pub status: String,
    pub provider: String,
    pub model_name: String,
    pub model_sha256: String,
}

pub fn validator_backend_from_env() -> Result<CatRecordingValidatorBackend, String> {
    let configured = env::var(CAT_RECORDING_VALIDATOR_ENV).ok();
    parse_validator_backend(configured.as_deref())
}

pub fn parse_validator_backend(
    configured: Option<&str>,
) -> Result<CatRecordingValidatorBackend, String> {
    match configured
        .unwrap_or("mobilenet_v2_int8")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "mobilenet_v2_int8" => Ok(CatRecordingValidatorBackend::MobileNetV2Int8),
        _ => Err(format!(
            "{CAT_RECORDING_VALIDATOR_ENV} must be mobilenet_v2_int8"
        )),
    }
}

pub fn build_classifier_probe_command(config: &CatRecordingClassifierConfig) -> Command {
    let mut command = Command::new(&config.python_bin);
    command
        .arg(&config.classifier_bin)
        .arg("--model")
        .arg(&config.model_path)
        .arg("--expected-sha256")
        .arg(&config.expected_model_sha256)
        .arg("--ai-threads")
        .arg(config.ai_threads.to_string())
        .arg("--affinity")
        .arg(&config.affinity)
        .arg("--probe");
    command
}

pub fn parse_classifier_probe_output(
    stdout: &[u8],
    config: &CatRecordingClassifierConfig,
) -> Result<CatRecordingClassifierProbeOutput, String> {
    let output = serde_json::from_slice::<CatRecordingClassifierProbeOutput>(stdout)
        .map_err(|error| format!("cat_classifier_probe_invalid_json: {error}"))?;
    if output.schema_version != "1.0" || output.status != "ok" {
        return Err("cat_classifier_probe_invalid_status".to_string());
    }
    if output.provider != "SpaceMITExecutionProvider" {
        return Err("cat_classifier_spacemit_provider_not_active".to_string());
    }
    if output.model_name != CAT_RECORDING_CLASSIFIER_MODEL_NAME
        || output.model_sha256 != config.expected_model_sha256
    {
        return Err("cat_classifier_model_contract_mismatch".to_string());
    }
    Ok(output)
}

pub fn classifier_config_from_env() -> Result<CatRecordingClassifierConfig, String> {
    let threshold_ppm = env::var(CAT_RECORDING_CLASSIFIER_THRESHOLD_ENV)
        .ok()
        .map(|value| parse_threshold_ppm(&value))
        .transpose()?
        .unwrap_or(DEFAULT_THRESHOLD_PPM);
    let ai_threads = env::var(CAT_RECORDING_CLASSIFIER_AI_THREADS_ENV)
        .ok()
        .map(|value| {
            value
                .trim()
                .parse::<usize>()
                .map_err(|_| format!("{CAT_RECORDING_CLASSIFIER_AI_THREADS_ENV} must be 1..=4"))
        })
        .transpose()?
        .unwrap_or(DEFAULT_AI_THREADS);
    if !(1..=4).contains(&ai_threads) {
        return Err(format!(
            "{CAT_RECORDING_CLASSIFIER_AI_THREADS_ENV} must be 1..=4"
        ));
    }
    let affinity = env::var(CAT_RECORDING_CLASSIFIER_AFFINITY_ENV)
        .unwrap_or_else(|_| DEFAULT_AFFINITY.to_string());
    validate_affinity(&affinity, ai_threads)?;

    Ok(CatRecordingClassifierConfig {
        python_bin: env::var("HARBOR_K3_CAT_RECORDING_CLASSIFIER_PYTHON")
            .unwrap_or_else(|_| "python3".to_string()),
        classifier_bin: env_path(CAT_RECORDING_CLASSIFIER_BIN_ENV, DEFAULT_CLASSIFIER_BIN),
        model_path: env_path(CAT_RECORDING_CLASSIFIER_MODEL_ENV, DEFAULT_CLASSIFIER_MODEL),
        expected_model_sha256: env::var(CAT_RECORDING_CLASSIFIER_MODEL_SHA256_ENV)
            .unwrap_or_else(|_| CAT_RECORDING_CLASSIFIER_MODEL_SHA256.to_string())
            .trim()
            .to_ascii_lowercase(),
        threshold_ppm,
        ai_threads,
        affinity,
    })
}

pub fn build_classifier_command(
    config: &CatRecordingClassifierConfig,
    sample_frames: &[(u8, PathBuf)],
) -> Result<Command, String> {
    validate_sample_frames(sample_frames)?;
    let mut command = Command::new(&config.python_bin);
    command
        .arg(&config.classifier_bin)
        .arg("--model")
        .arg(&config.model_path)
        .arg("--expected-sha256")
        .arg(&config.expected_model_sha256)
        .arg("--threshold")
        .arg(format!(
            "{:.6}",
            f64::from(config.threshold_ppm) / 1_000_000.0
        ))
        .arg("--ai-threads")
        .arg(config.ai_threads.to_string())
        .arg("--affinity")
        .arg(&config.affinity);
    for (frame_index, frame_path) in sample_frames {
        command
            .arg("--frame")
            .arg(format!("{frame_index}={}", frame_path.display()));
    }
    Ok(command)
}

pub fn parse_classifier_output(
    stdout: &[u8],
    config: &CatRecordingClassifierConfig,
    expected_frame_indices: &[u8],
) -> Result<CatRecordingClassifierOutput, String> {
    let output = serde_json::from_slice::<CatRecordingClassifierOutput>(stdout)
        .map_err(|error| format!("cat_classifier_invalid_json: {error}"))?;
    if output.schema_version != "1.0" || output.status != "ok" {
        return Err("cat_classifier_invalid_status".to_string());
    }
    if output.provider != "SpaceMITExecutionProvider" {
        return Err("cat_classifier_spacemit_provider_not_active".to_string());
    }
    if output.model_name != CAT_RECORDING_CLASSIFIER_MODEL_NAME
        || output.model_sha256 != config.expected_model_sha256
    {
        return Err("cat_classifier_model_contract_mismatch".to_string());
    }
    if output.threshold_ppm != config.threshold_ppm {
        return Err("cat_classifier_threshold_mismatch".to_string());
    }
    let actual_indices = output
        .predictions
        .iter()
        .map(|prediction| prediction.frame_index)
        .collect::<Vec<_>>();
    if actual_indices != expected_frame_indices
        || usize::from(output.sampled_frame_count) != expected_frame_indices.len()
    {
        return Err("cat_classifier_frame_contract_mismatch".to_string());
    }
    aggregate_cat_recording_predictions(
        &output.predictions,
        config.threshold_ppm,
        CAT_RECORDING_CLASSIFIER_MIN_POSITIVE_FRAMES,
    )?;
    Ok(output)
}

pub fn aggregate_cat_recording_predictions(
    predictions: &[CatRecordingFramePrediction],
    threshold_ppm: u32,
    minimum_positive_frames: usize,
) -> Result<CatRecordingAggregation, String> {
    if predictions.is_empty() || predictions.len() > CAT_RECORDING_CLASSIFIER_MAX_FRAMES {
        return Err(format!(
            "classifier requires between 1 and {} frames",
            CAT_RECORDING_CLASSIFIER_MAX_FRAMES
        ));
    }
    if threshold_ppm > 1_000_000 || minimum_positive_frames == 0 {
        return Err("classifier aggregation policy is invalid".to_string());
    }
    let mut sorted = predictions.to_vec();
    sorted.sort_by_key(|prediction| prediction.frame_index);
    for window in sorted.windows(2) {
        if window[0].frame_index == window[1].frame_index {
            return Err("classifier returned a duplicate frame index".to_string());
        }
    }
    if sorted.iter().any(|prediction| {
        !(1..=CAT_RECORDING_CLASSIFIER_MAX_FRAMES as u8).contains(&prediction.frame_index)
            || prediction.cat_probability_ppm > 1_000_000
    }) {
        return Err(format!(
            "classifier frame indices must be between 1 and {} and probabilities must be valid",
            CAT_RECORDING_CLASSIFIER_MAX_FRAMES
        ));
    }
    let cat_frame_indices = sorted
        .iter()
        .filter(|prediction| prediction.cat_probability_ppm >= threshold_ppm)
        .map(|prediction| prediction.frame_index)
        .collect::<Vec<_>>();
    let cat_present = cat_frame_indices.len() >= minimum_positive_frames;
    let reason_code = if cat_present {
        "cat_visible"
    } else if cat_frame_indices.is_empty() {
        "no_cat_visible"
    } else {
        "uncertain"
    };
    Ok(CatRecordingAggregation {
        cat_present,
        cat_frame_indices,
        reason_code: reason_code.to_string(),
        frame_predictions: sorted,
    })
}

fn validate_sample_frames(sample_frames: &[(u8, PathBuf)]) -> Result<(), String> {
    let predictions = sample_frames
        .iter()
        .map(|(frame_index, _)| CatRecordingFramePrediction {
            frame_index: *frame_index,
            cat_probability_ppm: 0,
            inference_ms: 0,
        })
        .collect::<Vec<_>>();
    aggregate_cat_recording_predictions(&predictions, DEFAULT_THRESHOLD_PPM, 1)?;
    for (_, path) in sample_frames {
        if !path.is_file() {
            return Err(format!(
                "cat classifier frame is missing: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn parse_threshold_ppm(value: &str) -> Result<u32, String> {
    let threshold = value
        .trim()
        .parse::<f64>()
        .map_err(|_| format!("{CAT_RECORDING_CLASSIFIER_THRESHOLD_ENV} must be 0.0..=1.0"))?;
    if !threshold.is_finite() || !(0.0..=1.0).contains(&threshold) {
        return Err(format!(
            "{CAT_RECORDING_CLASSIFIER_THRESHOLD_ENV} must be 0.0..=1.0"
        ));
    }
    Ok((threshold * 1_000_000.0).round() as u32)
}

fn validate_affinity(value: &str, ai_threads: usize) -> Result<(), String> {
    let cores = value
        .split(';')
        .map(str::parse::<u8>)
        .collect::<Result<Vec<_>, _>>();
    let valid = cores.as_ref().is_ok_and(|cores| {
        cores.len() == ai_threads
            && cores.iter().all(|core| [12, 13, 14, 15].contains(core))
            && !cores
                .iter()
                .enumerate()
                .any(|(index, core)| cores[..index].contains(core))
    });
    if !valid {
        return Err(format!(
            "{CAT_RECORDING_CLASSIFIER_AFFINITY_ENV} must contain {ai_threads} unique core IDs from 12..=15"
        ));
    }
    Ok(())
}

fn env_path(name: &str, default: &str) -> PathBuf {
    env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(default).to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::validate_affinity;

    #[test]
    fn affinity_requires_unique_cluster_cores() {
        assert!(validate_affinity("12;13;14;15", 4).is_ok());
        assert!(validate_affinity("12;13;14;14", 4)
            .unwrap_err()
            .contains("unique"));
    }
}
