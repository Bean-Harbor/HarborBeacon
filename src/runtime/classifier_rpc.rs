//! Private, bounded classifier transport between Beacon and the N2 model runtime.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Cursor, Read, Take, Write};
use std::path::PathBuf;
use std::time::Duration;

use reqwest::blocking::{Body, Client};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::ai_execution::{
    request_execution_cancel, ExecutionControl, ExecutionLease, EXECUTION_ID_HEADER,
};
use super::ai_resource_scheduler::{
    acquire_ai_resource_lease_until, AiLeaseQuarantineReason, AiWorkload,
};
use super::cat_recording_classifier::{
    build_classifier_command, build_classifier_probe_command, classifier_config_from_env,
    parse_classifier_output, parse_classifier_probe_output, validator_backend_from_env,
    CatRecordingClassifierConfig, CatRecordingClassifierOutput, CatRecordingClassifierProbeOutput,
    CAT_RECORDING_CLASSIFIER_MAX_FRAMES,
};
use super::owned_ai_process::run_owned_ai_command;
use crate::service_auth::model_api_verifier_token;

pub const CLASSIFIER_RPC_PATH: &str = "/internal/ai/classifier";
const MAX_MANIFEST_BYTES: usize = 16 * 1024;
const MAX_FRAME_BYTES: u64 = 32 * 1024 * 1024;
pub const CLASSIFIER_MAX_BODY: u64 =
    4 + MAX_MANIFEST_BYTES as u64 + MAX_FRAME_BYTES * CAT_RECORDING_CLASSIFIER_MAX_FRAMES as u64;
const CLASSIFIER_UPSTREAM: &str = "http://127.0.0.1:8792";
const MAX_RESPONSE_BYTES: u64 = 128 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ClassifierManifest {
    schema_version: u8,
    probe: bool,
    model_sha256: String,
    threshold_ppm: u32,
    frames: Vec<FrameManifest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FrameManifest {
    frame_index: u8,
    length: u64,
}

#[derive(Debug)]
pub struct ClassifierRpcError {
    pub code: String,
    pub message: String,
    pub exit_confirmed: bool,
}

impl ClassifierRpcError {
    fn before_execution(code: &str) -> Self {
        Self {
            code: code.to_string(),
            message: code.to_string(),
            exit_confirmed: true,
        }
    }
}

fn validate_manifest(
    manifest: &ClassifierManifest,
    expected_hash: &str,
) -> Result<u64, ClassifierRpcError> {
    if manifest.schema_version != 1 || manifest.threshold_ppm > 1_000_000 {
        return Err(ClassifierRpcError::before_execution(
            "cat_classifier_invalid_manifest",
        ));
    }
    if manifest.model_sha256 != expected_hash
        || expected_hash.len() != 64
        || !expected_hash.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ClassifierRpcError::before_execution(
            "cat_classifier_model_contract_mismatch",
        ));
    }
    if (manifest.probe && !manifest.frames.is_empty())
        || (!manifest.probe && manifest.frames.is_empty())
        || manifest.frames.len() > CAT_RECORDING_CLASSIFIER_MAX_FRAMES
    {
        return Err(ClassifierRpcError::before_execution(
            "cat_classifier_invalid_frame_count",
        ));
    }
    let mut seen = [false; CAT_RECORDING_CLASSIFIER_MAX_FRAMES + 1];
    let mut total = 0;
    for frame in &manifest.frames {
        let index = usize::from(frame.frame_index);
        if index == 0
            || index > CAT_RECORDING_CLASSIFIER_MAX_FRAMES
            || seen[index]
            || frame.length == 0
            || frame.length > MAX_FRAME_BYTES
        {
            return Err(ClassifierRpcError::before_execution(
                "cat_classifier_invalid_frame_manifest",
            ));
        }
        seen[index] = true;
        total += frame.length;
    }
    Ok(total)
}

fn read_manifest(
    reader: &mut dyn Read,
    expected_hash: &str,
    body_length: Option<u64>,
    should_stop: &dyn Fn() -> bool,
) -> Result<ClassifierManifest, ClassifierRpcError> {
    if body_length.is_some_and(|length| !(5..=CLASSIFIER_MAX_BODY).contains(&length)) {
        return Err(ClassifierRpcError::before_execution(
            "cat_classifier_invalid_body_length",
        ));
    }
    let mut prefix = [0; 4];
    read_exact_stoppable(reader, &mut prefix, should_stop)?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length == 0 || length > MAX_MANIFEST_BYTES {
        return Err(ClassifierRpcError::before_execution(
            "cat_classifier_invalid_manifest_length",
        ));
    }
    let mut bytes = vec![0; length];
    read_exact_stoppable(reader, &mut bytes, should_stop)?;
    let manifest: ClassifierManifest = serde_json::from_slice(&bytes)
        .map_err(|_| ClassifierRpcError::before_execution("cat_classifier_invalid_manifest"))?;
    let payload_length = validate_manifest(&manifest, expected_hash)?;
    if body_length.is_some_and(|total| total != 4 + length as u64 + payload_length) {
        return Err(ClassifierRpcError::before_execution(
            "cat_classifier_invalid_body_length",
        ));
    }
    Ok(manifest)
}

fn copy_frame(
    reader: &mut dyn Read,
    writer: &mut dyn Write,
    length: u64,
    should_stop: &dyn Fn() -> bool,
) -> Result<(), ClassifierRpcError> {
    if length == 0 || length > MAX_FRAME_BYTES {
        return Err(ClassifierRpcError::before_execution(
            "cat_classifier_invalid_frame_manifest",
        ));
    }
    let mut buffer = [0; 64 * 1024];
    let prefix_length = length.min(8) as usize;
    read_exact_stoppable(reader, &mut buffer[..prefix_length], should_stop)?;
    let prefix = &buffer[..prefix_length];
    if !prefix.starts_with(b"\xff\xd8\xff") && !prefix.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err(ClassifierRpcError::before_execution(
            "cat_classifier_invalid_frame_format",
        ));
    }
    writer
        .write_all(prefix)
        .map_err(|_| ClassifierRpcError::before_execution("cat_classifier_frame_write_failed"))?;
    let mut remaining = length - prefix_length as u64;
    while remaining > 0 {
        let length = remaining.min(buffer.len() as u64) as usize;
        read_exact_stoppable(reader, &mut buffer[..length], should_stop)?;
        writer.write_all(&buffer[..length]).map_err(|_| {
            ClassifierRpcError::before_execution("cat_classifier_frame_write_failed")
        })?;
        remaining -= length as u64;
    }
    Ok(())
}

fn read_exact_stoppable(
    reader: &mut dyn Read,
    mut bytes: &mut [u8],
    should_stop: &dyn Fn() -> bool,
) -> Result<(), ClassifierRpcError> {
    while !bytes.is_empty() {
        if should_stop() {
            return Err(ClassifierRpcError::before_execution(
                "cat_classifier_cancelled",
            ));
        }
        match reader.read(bytes) {
            Ok(0) => {
                return Err(ClassifierRpcError::before_execution(
                    "cat_classifier_truncated_body",
                ))
            }
            Ok(read) => bytes = &mut bytes[read..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => {
                return Err(ClassifierRpcError::before_execution(
                    "cat_classifier_body_read_failed",
                ))
            }
        }
    }
    Ok(())
}

struct StagedFrames {
    directory: PathBuf,
    frames: Vec<(u8, PathBuf)>,
    exit_unconfirmed: bool,
}

impl StagedFrames {
    fn new() -> Result<Self, ClassifierRpcError> {
        let directory = std::env::temp_dir().join(format!("harbor-classifier-{}", Uuid::new_v4()));
        let builder = fs::DirBuilder::new();
        #[cfg(unix)]
        let builder = {
            use std::os::unix::fs::DirBuilderExt;
            let mut builder = builder;
            builder.mode(0o700);
            builder
        };
        builder.create(&directory).map_err(|_| {
            ClassifierRpcError::before_execution("cat_classifier_temp_directory_failed")
        })?;
        Ok(Self {
            directory,
            frames: Vec::new(),
            exit_unconfirmed: false,
        })
    }

    fn receive(
        &mut self,
        reader: &mut dyn Read,
        manifest: &ClassifierManifest,
        should_stop: &dyn Fn() -> bool,
    ) -> Result<(), ClassifierRpcError> {
        for frame in &manifest.frames {
            let path = self
                .directory
                .join(format!("frame-{}.image", frame.frame_index));
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|_| {
                    ClassifierRpcError::before_execution("cat_classifier_frame_write_failed")
                })?;
            copy_frame(reader, &mut file, frame.length, should_stop)?;
            self.frames.push((frame.frame_index, path));
        }
        Ok(())
    }
}

impl Drop for StagedFrames {
    fn drop(&mut self) {
        if !self.exit_unconfirmed {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
}

pub fn execute_classifier_rpc(
    reader: &mut dyn Read,
    body_length: u64,
    control: &ExecutionControl,
) -> Result<Value, ClassifierRpcError> {
    validator_backend_from_env().map_err(|_| {
        ClassifierRpcError::before_execution("cat_classifier_configuration_unavailable")
    })?;
    let config = classifier_config_from_env().map_err(|_| {
        ClassifierRpcError::before_execution("cat_classifier_configuration_unavailable")
    })?;
    execute_with_config(reader, body_length, control, config)
}

fn execute_with_config(
    reader: &mut dyn Read,
    body_length: u64,
    control: &ExecutionControl,
    mut config: CatRecordingClassifierConfig,
) -> Result<Value, ClassifierRpcError> {
    let manifest = read_manifest(
        reader,
        &config.expected_model_sha256,
        Some(body_length),
        &|| control.should_stop(),
    )?;
    if !config.classifier_bin.is_file() || !config.model_path.is_file() {
        return Err(ClassifierRpcError::before_execution(
            "cat_classifier_material_unavailable",
        ));
    }
    config.threshold_ppm = manifest.threshold_ppm;
    let mut staged = StagedFrames::new()?;
    staged.receive(reader, &manifest, &|| control.should_stop())?;
    if control.should_stop() {
        return Err(ClassifierRpcError::before_execution(
            "cat_classifier_cancelled",
        ));
    }
    let command = if manifest.probe {
        build_classifier_probe_command(&config)
    } else {
        build_classifier_command(&config, &staged.frames).map_err(|_| {
            ClassifierRpcError::before_execution("cat_classifier_invalid_frame_manifest")
        })?
    };
    let lease = acquire_ai_resource_lease_until(
        AiWorkload::CatRecordingVerifier,
        control.deadline(),
        control.cancel_flag(),
    )
    .map_err(|error| {
        ClassifierRpcError::before_execution(&format!("cat_classifier_{}", error.code()))
    })?;
    let lease = ExecutionLease::new(lease);
    if control.should_stop() {
        lease.confirm_stopped();
        return Err(ClassifierRpcError::before_execution(
            "cat_classifier_cancelled",
        ));
    }
    let output = match run_owned_ai_command(
        command,
        Duration::from_secs(15),
        Duration::from_secs(5),
        control,
    ) {
        Ok(output) => output,
        Err(error) => {
            if error.exit_confirmed {
                lease.confirm_stopped();
            } else {
                staged.exit_unconfirmed = true;
                lease.quarantine(AiLeaseQuarantineReason::ProcessExitUnconfirmed);
            }
            return Err(ClassifierRpcError {
                code: "cat_classifier_runner_failed".to_string(),
                message: if error.exit_confirmed {
                    "classifier could not complete"
                } else {
                    "classifier exit unconfirmed; AI resource quarantined"
                }
                .to_string(),
                exit_confirmed: error.exit_confirmed,
            });
        }
    };
    if let Some(reason) =
        classifier_quarantine_reason(output.timed_out, output.status.success(), &output.stderr)
    {
        lease.quarantine(reason);
        return Err(ClassifierRpcError {
            code: if output.timed_out {
                "cat_classifier_timeout"
            } else {
                "cat_classifier_runtime_failed"
            }
            .to_string(),
            message: "classifier execution failed; AI resource quarantined".to_string(),
            exit_confirmed: true,
        });
    }
    lease.confirm_stopped();
    if output.cancelled || control.should_stop() {
        return Err(ClassifierRpcError::before_execution(
            "cat_classifier_cancelled",
        ));
    }
    if !output.status.success() {
        return Err(ClassifierRpcError::before_execution(
            "cat_classifier_failed",
        ));
    }
    if manifest.probe {
        parse_classifier_probe_output(&output.stdout, &config).map_err(|_| {
            ClassifierRpcError::before_execution("cat_classifier_invalid_probe_output")
        })?;
    } else {
        let mut indices = manifest
            .frames
            .iter()
            .map(|frame| frame.frame_index)
            .collect::<Vec<_>>();
        indices.sort_unstable();
        parse_classifier_output(&output.stdout, &config, &indices)
            .map_err(|_| ClassifierRpcError::before_execution("cat_classifier_invalid_output"))?;
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|_| ClassifierRpcError::before_execution("cat_classifier_invalid_output"))
}

fn classifier_quarantine_reason(
    timed_out: bool,
    success: bool,
    stderr: &[u8],
) -> Option<AiLeaseQuarantineReason> {
    if timed_out {
        return Some(AiLeaseQuarantineReason::InferenceTimeout);
    }
    if success {
        return None;
    }
    let text = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    ([
        "onnxruntimeerror",
        "onnx runtime error",
        "spacemitexecutionprovider",
        "spacemit execution provider",
        "spacemit_onnx_runtime",
    ]
    .iter()
    .any(|marker| text.contains(marker))
        || text
            .split(|character: char| !character.is_ascii_alphanumeric())
            .any(|token| matches!(token, "npu" | "tcm")))
    .then_some(AiLeaseQuarantineReason::RuntimeFailure)
}

struct ClassifierBody {
    prefix: Cursor<Vec<u8>>,
    frames: VecDeque<Take<File>>,
}

impl Read for ClassifierBody {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let read = self.prefix.read(buffer)?;
        if read > 0 {
            return Ok(read);
        }
        while let Some(frame) = self.frames.front_mut() {
            let read = frame.read(buffer)?;
            if read > 0 {
                return Ok(read);
            }
            if frame.limit() > 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "classifier frame was truncated",
                ));
            }
            self.frames.pop_front();
        }
        Ok(0)
    }
}

fn classifier_body(
    sample_frames: &[(u8, PathBuf)],
    config: &CatRecordingClassifierConfig,
    probe: bool,
) -> Result<(ClassifierBody, u64), String> {
    let mut manifest = ClassifierManifest {
        schema_version: 1,
        probe,
        model_sha256: config.expected_model_sha256.clone(),
        threshold_ppm: config.threshold_ppm,
        frames: Vec::new(),
    };
    let mut frames = VecDeque::new();
    for (frame_index, path) in sample_frames {
        let file = File::open(path).map_err(|_| "cat_classifier_frame_read_failed")?;
        let metadata = file
            .metadata()
            .map_err(|_| "cat_classifier_frame_read_failed")?;
        if !metadata.is_file() {
            return Err("cat_classifier_frame_read_failed".to_string());
        }
        manifest.frames.push(FrameManifest {
            frame_index: *frame_index,
            length: metadata.len(),
        });
        frames.push_back(file.take(metadata.len()));
    }
    let payload_length =
        validate_manifest(&manifest, &config.expected_model_sha256).map_err(|error| error.code)?;
    let encoded =
        serde_json::to_vec(&manifest).map_err(|_| "cat_classifier_manifest_encode_failed")?;
    if encoded.len() > MAX_MANIFEST_BYTES {
        return Err("cat_classifier_invalid_manifest_length".to_string());
    }
    let mut prefix = (encoded.len() as u32).to_be_bytes().to_vec();
    prefix.extend(encoded);
    let body_length = prefix.len() as u64 + payload_length;
    Ok((
        ClassifierBody {
            prefix: Cursor::new(prefix),
            frames,
        },
        body_length,
    ))
}

pub fn probe_classifier_rpc(
    config: &CatRecordingClassifierConfig,
) -> Result<CatRecordingClassifierProbeOutput, String> {
    let (body, length) = classifier_body(&[], config, true)?;
    let output = send_classifier_rpc(body, length)?;
    parse_classifier_probe_output(&output, config)
}

pub fn classify_frames_rpc(
    sample_frames: &[(u8, PathBuf)],
    config: &CatRecordingClassifierConfig,
) -> Result<CatRecordingClassifierOutput, String> {
    let (body, length) = classifier_body(sample_frames, config, false)?;
    let output = send_classifier_rpc(body, length)?;
    let mut indices = sample_frames
        .iter()
        .map(|(index, _)| *index)
        .collect::<Vec<_>>();
    indices.sort_unstable();
    parse_classifier_output(&output, config, &indices)
}

fn send_classifier_rpc(body: ClassifierBody, length: u64) -> Result<Vec<u8>, String> {
    let token = model_api_verifier_token()
        .map_err(|_| "cat_classifier_model_auth_unavailable")?
        .current;
    let client = Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(110))
        .build()
        .map_err(|_| "cat_classifier_rpc_client_failed")?;
    let upstream = Url::parse(CLASSIFIER_UPSTREAM).expect("fixed loopback classifier URL");
    send_classifier_rpc_to(&client, &upstream, &token, body, length)
}

fn send_classifier_rpc_to(
    client: &Client,
    upstream: &Url,
    token: &str,
    body: ClassifierBody,
    length: u64,
) -> Result<Vec<u8>, String> {
    let id = Uuid::new_v4().to_string();
    let url = upstream
        .join(CLASSIFIER_RPC_PATH)
        .map_err(|_| "cat_classifier_rpc_url_invalid")?;
    let response = client
        .post(url)
        .bearer_auth(token)
        .header(EXECUTION_ID_HEADER, &id)
        .header("Content-Type", "application/octet-stream")
        .body(Body::sized(body, length))
        .send();
    let response = match response {
        Ok(response) => response,
        Err(_) => {
            request_execution_cancel(client, upstream, token, &id);
            return Err("cat_classifier_rpc_unavailable; exit_unconfirmed=true".to_string());
        }
    };
    let status = response.status();
    let stopped = response
        .headers()
        .get("X-Harbor-Execution-Stopped")
        .is_some_and(|value| value == "true");
    let mut output = Vec::new();
    if response
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut output)
        .is_err()
        || output.len() as u64 > MAX_RESPONSE_BYTES
    {
        request_execution_cancel(client, upstream, token, &id);
        return Err("cat_classifier_rpc_invalid_response; exit_unconfirmed=true".to_string());
    }
    if !stopped {
        request_execution_cancel(client, upstream, token, &id);
        return Err("cat_classifier_rpc_exit_unconfirmed; exit_unconfirmed=true".to_string());
    }
    if !status.is_success() {
        let body: Value = serde_json::from_slice(&output).unwrap_or(Value::Null);
        let code = body
            .pointer("/error/code")
            .and_then(Value::as_str)
            .unwrap_or("");
        let code = match code {
            "EXECUTION_QUEUE_FULL" | "MODEL_QUEUE_FULL" => "cat_classifier_ai_resource_queue_full",
            "MODEL_QUEUE_TIMEOUT" => "cat_classifier_ai_resource_wait_timeout",
            value
                if value.starts_with("cat_classifier_")
                    && value.len() <= 128
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_') =>
            {
                value
            }
            _ => "cat_classifier_rpc_failed",
        };
        return Err(code.to_string());
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::super::ai_execution::ExecutionRegistry;
    use super::super::cat_recording_classifier::CAT_RECORDING_CLASSIFIER_MODEL_SHA256;
    use super::*;
    use std::cell::Cell;
    use std::io::Cursor;
    use std::time::Instant;
    use tiny_http::{Header, Response, Server};

    const JPEG: &[u8] = b"\xff\xd8\xff\xe0synthetic-jpeg";
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\nsynthetic-png";

    fn manifest() -> ClassifierManifest {
        ClassifierManifest {
            schema_version: 1,
            probe: false,
            model_sha256: CAT_RECORDING_CLASSIFIER_MODEL_SHA256.to_string(),
            threshold_ppm: 620_000,
            frames: vec![FrameManifest {
                frame_index: 1,
                length: JPEG.len() as u64,
            }],
        }
    }

    fn config() -> CatRecordingClassifierConfig {
        CatRecordingClassifierConfig {
            python_bin: "python3".to_string(),
            classifier_bin: PathBuf::from("/nonexistent-classifier-fixture"),
            model_path: PathBuf::from("/nonexistent-model-fixture"),
            expected_model_sha256: CAT_RECORDING_CLASSIFIER_MODEL_SHA256.to_string(),
            threshold_ppm: 620_000,
            ai_threads: 4,
            affinity: "12;13;14;15".to_string(),
        }
    }

    fn encoded(value: &ClassifierManifest) -> Vec<u8> {
        let json = serde_json::to_vec(value).unwrap();
        let mut bytes = (json.len() as u32).to_be_bytes().to_vec();
        bytes.extend(json);
        bytes
    }

    fn assert_invalid(value: &ClassifierManifest) {
        assert!(validate_manifest(value, CAT_RECORDING_CLASSIFIER_MODEL_SHA256).is_err());
    }

    #[test]
    fn rpc_accepts_nine_maximum_frames_without_allocating_their_payload() {
        let mut value = manifest();
        value.frames = (1..=9)
            .map(|frame_index| FrameManifest {
                frame_index,
                length: MAX_FRAME_BYTES,
            })
            .collect();
        assert_eq!(
            validate_manifest(&value, CAT_RECORDING_CLASSIFIER_MODEL_SHA256).unwrap(),
            288 * 1024 * 1024
        );
    }

    #[test]
    fn rpc_probe_requires_no_frames() {
        let mut value = manifest();
        value.probe = true;
        assert_invalid(&value);
        value.frames.clear();
        assert_eq!(validate_manifest(&value, &value.model_sha256).unwrap(), 0);
        value.probe = false;
        assert_invalid(&value);
    }

    #[test]
    fn rpc_rejects_invalid_frame_counts_indices_and_lengths() {
        let mut value = manifest();
        value.frames = vec![value.frames[0].clone(); 10];
        assert_invalid(&value);
        value.frames.truncate(2);
        assert_invalid(&value);
        value.frames.truncate(1);
        for index in [0, 10, 255] {
            value.frames[0].frame_index = index;
            assert_invalid(&value);
        }
        value.frames[0].frame_index = 1;
        for length in [0, MAX_FRAME_BYTES + 1, u64::MAX] {
            value.frames[0].length = length;
            assert_invalid(&value);
        }
    }

    #[test]
    fn rpc_rejects_schema_hash_and_threshold_drift() {
        let mut value = manifest();
        value.schema_version = 2;
        assert_invalid(&value);
        value.schema_version = 1;
        value.threshold_ppm = 1_000_001;
        assert_invalid(&value);
        value.threshold_ppm = 0;
        assert!(validate_manifest(&value, &value.model_sha256).is_ok());
        value.threshold_ppm = 1_000_000;
        assert!(validate_manifest(&value, &value.model_sha256).is_ok());
        value.model_sha256 = "not-the-server-model".to_string();
        assert_invalid(&value);
    }

    #[test]
    fn rpc_manifest_is_big_endian_bounded_and_stops_before_frame_bytes() {
        let value = manifest();
        let mut bytes = encoded(&value);
        let prefix_length = bytes.len();
        bytes.extend(JPEG);
        let body_length = bytes.len() as u64;
        let mut reader = Cursor::new(bytes);
        let decoded = read_manifest(&mut reader, &value.model_sha256, Some(body_length), &|| {
            false
        })
        .unwrap();
        assert_eq!(decoded.threshold_ppm, value.threshold_ppm);
        assert_eq!(reader.position(), prefix_length as u64);
    }

    #[test]
    fn rpc_manifest_rejects_truncation_oversize_and_unknown_fields() {
        for bytes in [vec![], vec![0, 0, 1], vec![0, 0, 0, 0], vec![0, 1, 0, 0]] {
            assert!(read_manifest(
                &mut Cursor::new(bytes),
                CAT_RECORDING_CLASSIFIER_MODEL_SHA256,
                None,
                &|| false,
            )
            .is_err());
        }
        let mut value = serde_json::to_value(manifest()).unwrap();
        value["command"] = serde_json::json!("must-not-be-accepted");
        let json = serde_json::to_vec(&value).unwrap();
        let mut bytes = (json.len() as u32).to_be_bytes().to_vec();
        bytes.extend(json);
        assert!(read_manifest(
            &mut Cursor::new(bytes),
            CAT_RECORDING_CLASSIFIER_MODEL_SHA256,
            None,
            &|| false,
        )
        .is_err());
    }

    #[test]
    fn rpc_body_length_mismatch_is_rejected_before_payload_read() {
        let value = manifest();
        let bytes = encoded(&value);
        let exact = bytes.len() as u64 + JPEG.len() as u64;
        for length in [0, exact - 1, exact + 1, CLASSIFIER_MAX_BODY + 1] {
            let error = read_manifest(
                &mut Cursor::new(bytes.clone()),
                &value.model_sha256,
                Some(length),
                &|| false,
            )
            .unwrap_err();
            assert_eq!(error.code, "cat_classifier_invalid_body_length");
        }
    }

    #[test]
    fn rpc_frame_copy_accepts_only_jpeg_or_png_and_never_reads_the_next_frame() {
        for image in [JPEG, PNG] {
            let mut bytes = image.to_vec();
            bytes.extend(b"next-frame");
            let mut reader = Cursor::new(bytes);
            let mut output = Vec::new();
            copy_frame(&mut reader, &mut output, image.len() as u64, &|| false).unwrap();
            assert_eq!(output, image);
            assert_eq!(reader.position(), image.len() as u64);
        }
        let error =
            copy_frame(&mut Cursor::new(b"GIF89a"), &mut Vec::new(), 6, &|| false).unwrap_err();
        assert_eq!(error.code, "cat_classifier_invalid_frame_format");
    }

    #[test]
    fn rpc_frame_copy_rejects_truncation() {
        let error = copy_frame(
            &mut Cursor::new(JPEG),
            &mut Vec::new(),
            JPEG.len() as u64 + 1,
            &|| false,
        )
        .unwrap_err();
        assert_eq!(error.code, "cat_classifier_truncated_body");
    }

    #[test]
    fn rpc_cancelled_body_does_not_read_or_write() {
        let mut reader = Cursor::new(encoded(&manifest()));
        let error = read_manifest(
            &mut reader,
            CAT_RECORDING_CLASSIFIER_MODEL_SHA256,
            None,
            &|| true,
        )
        .unwrap_err();
        assert_eq!(error.code, "cat_classifier_cancelled");
        assert_eq!(reader.position(), 0);
        let checks = Cell::new(0);
        let error = copy_frame(
            &mut Cursor::new(JPEG),
            &mut Vec::new(),
            JPEG.len() as u64,
            &|| {
                checks.set(checks.get() + 1);
                true
            },
        )
        .unwrap_err();
        assert_eq!(error.code, "cat_classifier_cancelled");
        assert!(checks.get() > 0);
    }

    #[test]
    fn rpc_streaming_roundtrip_preserves_frame_bytes_and_server_owns_paths() {
        let source = StagedFrames::new().unwrap();
        let source_path = source.directory.join("private-source.jpg");
        fs::write(&source_path, JPEG).unwrap();
        let config = config();
        let (mut body, length) =
            classifier_body(&[(3, source_path.clone())], &config, false).unwrap();
        let mut encoded = Vec::new();
        body.read_to_end(&mut encoded).unwrap();
        assert_eq!(encoded.len() as u64, length);
        assert!(!String::from_utf8_lossy(&encoded).contains("private-source"));
        let mut reader = Cursor::new(encoded);
        let manifest = read_manifest(
            &mut reader,
            &config.expected_model_sha256,
            Some(length),
            &|| false,
        )
        .unwrap();
        let mut received = StagedFrames::new().unwrap();
        let directory = received.directory.clone();
        received.receive(&mut reader, &manifest, &|| false).unwrap();
        assert_eq!(received.frames[0].0, 3);
        assert_eq!(fs::read(&received.frames[0].1).unwrap(), JPEG);
        assert_ne!(received.frames[0].1, source_path);
        drop(received);
        assert!(!directory.exists());
    }

    #[test]
    fn rpc_truncated_staging_removes_partial_files() {
        let mut staged = StagedFrames::new().unwrap();
        let directory = staged.directory.clone();
        let mut manifest = manifest();
        manifest.frames[0].length += 1;
        assert!(staged
            .receive(&mut Cursor::new(JPEG), &manifest, &|| false)
            .is_err());
        drop(staged);
        assert!(!directory.exists());
    }

    #[test]
    fn rpc_unconfirmed_child_keeps_its_frames_until_runtime_cleanup() {
        let mut staged = StagedFrames::new().unwrap();
        let directory = staged.directory.clone();
        staged
            .receive(&mut Cursor::new(JPEG), &manifest(), &|| false)
            .unwrap();
        staged.exit_unconfirmed = true;
        drop(staged);
        assert_eq!(fs::read(directory.join("frame-1.image")).unwrap(), JPEG);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn rpc_missing_classifier_material_is_local_and_confirmed_before_execution() {
        let mut value = manifest();
        value.probe = true;
        value.frames.clear();
        let bytes = encoded(&value);
        let registry = ExecutionRegistry::new(1);
        let ticket = registry
            .register(None, Instant::now() + Duration::from_secs(5))
            .unwrap();
        let error = execute_with_config(
            &mut Cursor::new(&bytes),
            bytes.len() as u64,
            &ticket.control(),
            config(),
        )
        .unwrap_err();
        assert_eq!(error.code, "cat_classifier_material_unavailable");
        assert!(error.exit_confirmed);
    }

    #[test]
    fn rpc_timeout_and_provider_failures_quarantine_but_missing_pil_does_not() {
        assert_eq!(
            classifier_quarantine_reason(true, false, b""),
            Some(AiLeaseQuarantineReason::InferenceTimeout)
        );
        assert_eq!(
            classifier_quarantine_reason(false, false, b"SpaceMITExecutionProvider: error"),
            Some(AiLeaseQuarantineReason::RuntimeFailure)
        );
        assert_eq!(
            classifier_quarantine_reason(
                false,
                false,
                b"ModuleNotFoundError: No module named 'PIL'"
            ),
            None
        );
        assert_eq!(
            classifier_quarantine_reason(false, true, b"SpaceMITExecutionProvider"),
            None
        );
    }

    fn mock_rpc(stopped: Option<bool>, status: u16, delay: bool) -> Result<Vec<u8>, String> {
        let server = Server::http("127.0.0.1:0").unwrap();
        let upstream =
            Url::parse(&format!("http://{}", server.server_addr().to_ip().unwrap())).unwrap();
        let (body, length) = classifier_body(&[], &config(), true).unwrap();
        let token = "synthetic-classifier-rpc-token-00000000";
        let worker = std::thread::spawn(move || {
            let mut request = server
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .unwrap();
            assert_eq!(request.url(), CLASSIFIER_RPC_PATH);
            assert_eq!(request.body_length(), Some(length as usize));
            assert_eq!(
                request
                    .headers()
                    .iter()
                    .find(|header| header.field.equiv("Authorization"))
                    .unwrap()
                    .value
                    .as_str(),
                format!("Bearer {token}")
            );
            let id = request
                .headers()
                .iter()
                .find(|header| header.field.equiv(EXECUTION_ID_HEADER))
                .unwrap()
                .value
                .as_str()
                .to_string();
            assert_eq!(Uuid::parse_str(&id).unwrap().to_string(), id);
            let mut received = Vec::new();
            request.as_reader().read_to_end(&mut received).unwrap();
            assert_eq!(received.len() as u64, length);
            if delay {
                std::thread::sleep(Duration::from_millis(200));
            }
            let mut response = Response::from_string(if status == 429 {
                "{\"error\":{\"code\":\"cat_classifier_ai_resource_queue_full\"}}"
            } else {
                "{\"status\":\"ok\"}"
            })
            .with_status_code(status);
            if let Some(stopped) = stopped {
                response = response.with_header(
                    Header::from_bytes(
                        "X-Harbor-Execution-Stopped",
                        if stopped { "true" } else { "false" },
                    )
                    .unwrap(),
                );
            }
            let _ = request.respond(response);
            if stopped != Some(true) || delay {
                let cancel = server
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap()
                    .unwrap();
                assert_eq!(cancel.method(), &tiny_http::Method::Post);
                assert_eq!(cancel.url(), format!("/internal/ai/executions/{id}/cancel"));
                assert_eq!(
                    cancel
                        .headers()
                        .iter()
                        .find(|header| header.field.equiv("Authorization"))
                        .unwrap()
                        .value
                        .as_str(),
                    format!("Bearer {token}")
                );
                cancel
                    .respond(Response::from_string(
                        "{\"state\":\"cancel_requested\",\"execution_stopped\":false}",
                    ))
                    .unwrap();
            }
        });
        let client = Client::builder()
            .no_proxy()
            .timeout(if delay {
                Duration::from_millis(100)
            } else {
                Duration::from_secs(3)
            })
            .build()
            .unwrap();
        let result = send_classifier_rpc_to(&client, &upstream, token, body, length);
        worker.join().unwrap();
        result
    }

    #[test]
    fn rpc_client_authenticates_sized_body_and_accepts_confirmed_response() {
        assert_eq!(
            mock_rpc(Some(true), 200, false).unwrap(),
            b"{\"status\":\"ok\"}"
        );
    }

    #[test]
    fn rpc_client_preserves_resource_contention_code() {
        assert_eq!(
            mock_rpc(Some(true), 429, false).unwrap_err(),
            "cat_classifier_ai_resource_queue_full"
        );
    }

    #[test]
    fn rpc_client_never_converts_unknown_stop_or_cancel_receipt_into_success() {
        for stopped in [None, Some(false)] {
            assert!(mock_rpc(stopped, 200, false)
                .unwrap_err()
                .contains("exit_unconfirmed=true"));
        }
    }

    #[test]
    fn rpc_client_timeout_sends_cancellation_for_the_same_execution() {
        assert!(mock_rpc(Some(true), 200, true)
            .unwrap_err()
            .contains("exit_unconfirmed=true"));
    }
}
