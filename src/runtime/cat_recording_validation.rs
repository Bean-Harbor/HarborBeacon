//! Durable validation state for YOLO-triggered cat recordings.

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::connectors::harborlink_media::HarborLinkRecordingArtifact;
use crate::runtime::cat_recording_classifier::CatRecordingFramePrediction;

pub const CAT_RECORDING_VALIDATION_MODE_ENV: &str = "HARBOR_K3_CAT_RECORDING_VALIDATION_MODE";
pub const CAT_RECORDING_VALIDATION_STORE_PATH_ENV: &str =
    "HARBOR_K3_CAT_RECORDING_VALIDATION_STORE_PATH";
pub const CAT_RECORDING_VALIDATION_POLICY_VERSION: &str = "cat-recording-validation-v5";
pub const CAT_RECORDING_MINIMUM_DURATION_MS: u64 = 5_000;

const DEFAULT_STORE_PATH: &str = ".harborbeacon/cat-recording-validations.jsonl";
const MAX_STORE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 64 * 1024;
const MAX_TEXT_CHARS: usize = 500;
const MAX_BEHAVIOR_TAGS: usize = 16;
const MAX_CAT_DETECTION_EVIDENCE: usize = 256;
const MAX_CAT_RECORDING_SAMPLE_FRAMES: usize = 10;
const MAX_ARTIFACT_DISCARD_ATTEMPTS: u32 = 3;

static STORE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CatRecordingValidationMode {
    Off,
    Shadow,
    Enforce,
}

impl CatRecordingValidationMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Shadow => "shadow",
            Self::Enforce => "enforce",
        }
    }

    pub fn validates_candidates(self) -> bool {
        !matches!(self, Self::Off)
    }

    pub fn enforces_publication(self) -> bool {
        matches!(self, Self::Enforce)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CatRecordingValidationStatus {
    PendingValidation,
    Processing,
    Accepted,
    ReviewRequired,
    Rejected,
    Failed,
}

impl CatRecordingValidationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PendingValidation => "pending_validation",
            Self::Processing => "processing",
            Self::Accepted => "accepted",
            Self::ReviewRequired => "review_required",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CatRecordingPublicationStatus {
    Unpublished,
    Published,
    Withdrawn,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CatRecordingArtifactDisposition {
    #[default]
    Retained,
    DiscardPending,
    Discarded,
    DiscardFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatDetectionEvidence {
    pub sequence: u64,
    pub frame_epoch_ms: u64,
    pub confidence_ppm: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatRecordingValidationDecision {
    pub cat_present: bool,
    #[serde(default)]
    pub cat_frame_indices: Vec<u8>,
    #[serde(default)]
    pub behavior_tags: Vec<String>,
    pub summary: String,
    pub reason_code: String,
    pub sampled_frame_count: u8,
    #[serde(default)]
    pub sampling_strategy: String,
    #[serde(default)]
    pub validation_rounds: u8,
    #[serde(default)]
    pub sampled_offsets_ms: Vec<u64>,
    #[serde(default)]
    pub frame_predictions: Vec<CatRecordingFramePrediction>,
    pub model_endpoint_id: String,
    pub model_name: String,
    pub elapsed_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatRecordingValidationRecord {
    pub schema_version: String,
    pub validation_id: String,
    pub policy_version: String,
    pub artifact_id: String,
    pub event_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_source: Option<String>,
    pub camera_id: String,
    pub mime_type: String,
    pub byte_size: u64,
    pub started_at_epoch_ms: u128,
    pub ended_at_epoch_ms: u128,
    pub duration_seconds: u32,
    pub validation_status: CatRecordingValidationStatus,
    pub publication_status: CatRecordingPublicationStatus,
    pub attempt_count: u32,
    #[serde(default)]
    pub detection_evidence: Vec<CatDetectionEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_retry_at_epoch_ms: Option<u128>,
    pub created_at_epoch_ms: u128,
    pub updated_at_epoch_ms: u128,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<CatRecordingValidationDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default)]
    pub artifact_disposition: CatRecordingArtifactDisposition,
    #[serde(default)]
    pub discard_attempt_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discarded_at_epoch_ms: Option<u128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discard_error: Option<String>,
}

impl CatRecordingValidationRecord {
    pub fn is_published(&self) -> bool {
        self.validation_status == CatRecordingValidationStatus::Accepted
            && self.publication_status == CatRecordingPublicationStatus::Published
    }

    pub fn is_physical_discard_eligible(&self) -> bool {
        self.policy_version == CAT_RECORDING_VALIDATION_POLICY_VERSION
            && self.artifact_id.starts_with("recordings~")
            && self.event_id.starts_with("cat-activity-")
            && self.artifact_source.as_deref() == Some("yolo_cat_activity")
            && matches!(
                self.validation_status,
                CatRecordingValidationStatus::Rejected
                    | CatRecordingValidationStatus::ReviewRequired
            )
            && self.publication_status == CatRecordingPublicationStatus::Unpublished
            && self.artifact_disposition != CatRecordingArtifactDisposition::Discarded
    }
}

pub fn cat_recording_hard_gate_reason(
    record: &CatRecordingValidationRecord,
    measured_duration_ms: u64,
) -> Option<(CatRecordingValidationStatus, &'static str)> {
    if measured_duration_ms < CAT_RECORDING_MINIMUM_DURATION_MS {
        return Some((
            CatRecordingValidationStatus::Rejected,
            "recording_shorter_than_minimum_duration",
        ));
    }
    let media_end_epoch_ms = record
        .started_at_epoch_ms
        .saturating_add(u128::from(measured_duration_ms));
    let has_in_window_evidence = record.detection_evidence.iter().any(|evidence| {
        let frame_epoch_ms = u128::from(evidence.frame_epoch_ms);
        frame_epoch_ms >= record.started_at_epoch_ms && frame_epoch_ms <= media_end_epoch_ms
    });
    if !has_in_window_evidence {
        return Some((
            CatRecordingValidationStatus::ReviewRequired,
            "recording_has_no_in_window_yolo_evidence",
        ));
    }
    None
}

#[derive(Debug, Clone)]
pub struct CatRecordingValidationStore {
    path: PathBuf,
}

impl Default for CatRecordingValidationStore {
    fn default() -> Self {
        Self::new(default_store_path())
    }
}

impl CatRecordingValidationStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn register_candidate(
        &self,
        artifact: &HarborLinkRecordingArtifact,
    ) -> Result<CatRecordingValidationRecord, String> {
        self.register_candidate_with_evidence(artifact, &[])
    }

    pub fn register_candidate_with_evidence(
        &self,
        artifact: &HarborLinkRecordingArtifact,
        detection_evidence: &[CatDetectionEvidence],
    ) -> Result<CatRecordingValidationRecord, String> {
        if artifact.kind != "recording" || !artifact.mime_type.starts_with("video/") {
            return Err("cat validation accepts recording video artifacts only".to_string());
        }
        let event_id = artifact
            .event_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "cat recording artifact is missing event_id".to_string())?;

        let _guard = store_lock()
            .lock()
            .map_err(|_| "cat recording validation store lock is unavailable".to_string())?;
        if let Some(existing) = self
            .latest_records_unlocked()?
            .remove(artifact.artifact_id.as_str())
        {
            return Ok(existing);
        }

        let now = epoch_ms();
        let record = CatRecordingValidationRecord {
            schema_version: "1.3".to_string(),
            validation_id: stable_validation_id(&artifact.artifact_id),
            policy_version: CAT_RECORDING_VALIDATION_POLICY_VERSION.to_string(),
            artifact_id: artifact.artifact_id.clone(),
            event_id: event_id.to_string(),
            artifact_source: artifact.source.clone(),
            camera_id: artifact.camera_id.clone(),
            mime_type: artifact.mime_type.clone(),
            byte_size: artifact.byte_size,
            started_at_epoch_ms: artifact.started_at_epoch_ms,
            ended_at_epoch_ms: artifact.ended_at_epoch_ms,
            duration_seconds: artifact.duration_seconds,
            validation_status: CatRecordingValidationStatus::PendingValidation,
            publication_status: CatRecordingPublicationStatus::Unpublished,
            attempt_count: 0,
            detection_evidence: sanitize_detection_evidence(detection_evidence),
            next_retry_at_epoch_ms: None,
            created_at_epoch_ms: now,
            updated_at_epoch_ms: now,
            decision: None,
            last_error: None,
            artifact_disposition: CatRecordingArtifactDisposition::Retained,
            discard_attempt_count: 0,
            discarded_at_epoch_ms: None,
            discard_error: None,
        };
        self.append_unlocked(&record)?;
        Ok(record)
    }

    pub fn mark_processing(
        &self,
        artifact_id: &str,
    ) -> Result<CatRecordingValidationRecord, String> {
        self.update_record(artifact_id, |record| {
            record.validation_status = CatRecordingValidationStatus::Processing;
            record.publication_status = CatRecordingPublicationStatus::Unpublished;
            record.attempt_count = record.attempt_count.saturating_add(1);
            record.next_retry_at_epoch_ms = None;
            record.last_error = None;
        })
    }

    pub fn complete(
        &self,
        artifact_id: &str,
        validation_status: CatRecordingValidationStatus,
        decision: Option<CatRecordingValidationDecision>,
        error: Option<&str>,
    ) -> Result<CatRecordingValidationRecord, String> {
        self.update_record(artifact_id, |record| {
            record.validation_status = validation_status;
            record.publication_status =
                if validation_status == CatRecordingValidationStatus::Accepted {
                    CatRecordingPublicationStatus::Published
                } else {
                    CatRecordingPublicationStatus::Unpublished
                };
            record.decision = decision;
            record.next_retry_at_epoch_ms = None;
            record.last_error = error.map(sanitize_text);
        })
    }

    pub fn schedule_retry(
        &self,
        artifact_id: &str,
        next_retry_at_epoch_ms: u128,
        error: &str,
    ) -> Result<CatRecordingValidationRecord, String> {
        self.update_record(artifact_id, |record| {
            record.validation_status = CatRecordingValidationStatus::PendingValidation;
            record.publication_status = CatRecordingPublicationStatus::Unpublished;
            record.decision = None;
            record.next_retry_at_epoch_ms = Some(next_retry_at_epoch_ms);
            record.last_error = Some(sanitize_text(error));
        })
    }

    pub fn defer_resource_contention(
        &self,
        artifact_id: &str,
        next_retry_at_epoch_ms: u128,
        error: &str,
    ) -> Result<CatRecordingValidationRecord, String> {
        self.update_record(artifact_id, |record| {
            if record.validation_status == CatRecordingValidationStatus::Processing {
                record.attempt_count = record.attempt_count.saturating_sub(1);
            }
            record.validation_status = CatRecordingValidationStatus::PendingValidation;
            record.publication_status = CatRecordingPublicationStatus::Unpublished;
            record.decision = None;
            record.next_retry_at_epoch_ms = Some(next_retry_at_epoch_ms);
            record.last_error = Some(sanitize_text(error));
        })
    }

    pub fn recover_interrupted(&self) -> Result<usize, String> {
        let records = self.list_latest()?;
        let mut recovered = 0;
        for record in records
            .into_iter()
            .filter(|record| record.validation_status == CatRecordingValidationStatus::Processing)
        {
            self.update_record(&record.artifact_id, |record| {
                record.validation_status = CatRecordingValidationStatus::PendingValidation;
                record.publication_status = CatRecordingPublicationStatus::Unpublished;
                record.next_retry_at_epoch_ms = None;
                record.last_error = Some("validation_worker_restarted".to_string());
            })?;
            recovered += 1;
        }
        Ok(recovered)
    }

    pub fn mark_artifact_discard_pending(
        &self,
        artifact_id: &str,
    ) -> Result<CatRecordingValidationRecord, String> {
        self.update_record(artifact_id, |record| {
            record.artifact_disposition = CatRecordingArtifactDisposition::DiscardPending;
            record.discard_attempt_count = record.discard_attempt_count.saturating_add(1);
            record.discard_error = None;
        })
    }

    pub fn mark_artifact_discarded(
        &self,
        artifact_id: &str,
    ) -> Result<CatRecordingValidationRecord, String> {
        self.update_record(artifact_id, |record| {
            record.artifact_disposition = CatRecordingArtifactDisposition::Discarded;
            record.discarded_at_epoch_ms = Some(epoch_ms());
            record.discard_error = None;
        })
    }

    pub fn mark_artifact_discard_failed(
        &self,
        artifact_id: &str,
        error: &str,
    ) -> Result<CatRecordingValidationRecord, String> {
        self.update_record(artifact_id, |record| {
            record.artifact_disposition = CatRecordingArtifactDisposition::DiscardFailed;
            record.discard_error = Some(sanitize_text(error));
        })
    }

    pub fn pending_discards(&self) -> Result<Vec<CatRecordingValidationRecord>, String> {
        Ok(self
            .list_latest()?
            .into_iter()
            .filter(|record| {
                matches!(
                    record.artifact_disposition,
                    CatRecordingArtifactDisposition::DiscardPending
                        | CatRecordingArtifactDisposition::DiscardFailed
                ) && record.discard_attempt_count < MAX_ARTIFACT_DISCARD_ATTEMPTS
                    && record.is_physical_discard_eligible()
            })
            .collect())
    }

    pub fn next_pending(&self) -> Result<Option<CatRecordingValidationRecord>, String> {
        let now = epoch_ms();
        let mut records = self
            .list_latest()?
            .into_iter()
            .filter(|record| {
                record.validation_status == CatRecordingValidationStatus::PendingValidation
                    && record
                        .next_retry_at_epoch_ms
                        .is_none_or(|retry_at| retry_at <= now)
            })
            .collect::<Vec<_>>();
        records.sort_by_key(|record| record.created_at_epoch_ms);
        Ok(records.into_iter().next())
    }

    pub fn list_latest(&self) -> Result<Vec<CatRecordingValidationRecord>, String> {
        let _guard = store_lock()
            .lock()
            .map_err(|_| "cat recording validation store lock is unavailable".to_string())?;
        let mut records = self
            .latest_records_unlocked()?
            .into_values()
            .collect::<Vec<_>>();
        records.sort_by_key(|record| std::cmp::Reverse(record.updated_at_epoch_ms));
        Ok(records)
    }

    pub fn records_for_artifacts(
        &self,
        artifact_ids: &HashSet<String>,
    ) -> Result<HashMap<String, CatRecordingValidationRecord>, String> {
        if artifact_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let _guard = store_lock()
            .lock()
            .map_err(|_| "cat recording validation store lock is unavailable".to_string())?;
        Ok(self
            .latest_records_unlocked()?
            .into_iter()
            .filter(|(artifact_id, _)| artifact_ids.contains(artifact_id))
            .collect())
    }

    fn update_record<F>(
        &self,
        artifact_id: &str,
        update: F,
    ) -> Result<CatRecordingValidationRecord, String>
    where
        F: FnOnce(&mut CatRecordingValidationRecord),
    {
        let _guard = store_lock()
            .lock()
            .map_err(|_| "cat recording validation store lock is unavailable".to_string())?;
        let mut record = self
            .latest_records_unlocked()?
            .remove(artifact_id)
            .ok_or_else(|| format!("cat recording validation record not found: {artifact_id}"))?;
        update(&mut record);
        record.updated_at_epoch_ms = epoch_ms();
        self.append_unlocked(&record)?;
        Ok(record)
    }

    fn latest_records_unlocked(
        &self,
    ) -> Result<HashMap<String, CatRecordingValidationRecord>, String> {
        if !self.path.exists() {
            return Ok(HashMap::new());
        }
        let metadata = fs::metadata(&self.path).map_err(|error| {
            format!(
                "failed to inspect cat recording validation store {}: {error}",
                self.path.display()
            )
        })?;
        if metadata.len() > MAX_STORE_BYTES {
            return Err(format!(
                "cat recording validation store exceeds {} bytes",
                MAX_STORE_BYTES
            ));
        }
        let file = File::open(&self.path).map_err(|error| {
            format!(
                "failed to open cat recording validation store {}: {error}",
                self.path.display()
            )
        })?;
        let mut latest = HashMap::new();
        for line in BufReader::new(file).lines() {
            let Ok(line) = line else {
                continue;
            };
            if line.len() > MAX_RECORD_BYTES {
                continue;
            }
            let Ok(record) = serde_json::from_str::<CatRecordingValidationRecord>(&line) else {
                continue;
            };
            latest.insert(record.artifact_id.clone(), record);
        }
        Ok(latest)
    }

    fn append_unlocked(&self, record: &CatRecordingValidationRecord) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "failed to create cat recording validation store directory {}: {error}",
                    parent.display()
                )
            })?;
        }
        let line = serde_json::to_vec(record)
            .map_err(|error| format!("failed to serialize cat validation record: {error}"))?;
        if line.len() > MAX_RECORD_BYTES {
            return Err("cat recording validation record is too large".to_string());
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|error| {
                format!(
                    "failed to append cat recording validation store {}: {error}",
                    self.path.display()
                )
            })?;
        file.write_all(&line)
            .and_then(|_| file.write_all(b"\n"))
            .and_then(|_| file.sync_data())
            .map_err(|error| format!("failed to persist cat validation record: {error}"))
    }
}

pub fn validation_mode_from_env() -> Result<CatRecordingValidationMode, String> {
    match env::var(CAT_RECORDING_VALIDATION_MODE_ENV)
        .unwrap_or_else(|_| "off".to_string())
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "off" => Ok(CatRecordingValidationMode::Off),
        "shadow" => Ok(CatRecordingValidationMode::Shadow),
        "enforce" => Ok(CatRecordingValidationMode::Enforce),
        _ => Err(format!(
            "{CAT_RECORDING_VALIDATION_MODE_ENV} must be off, shadow, or enforce"
        )),
    }
}

pub fn default_store_path() -> PathBuf {
    env::var_os(CAT_RECORDING_VALIDATION_STORE_PATH_ENV)
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from(DEFAULT_STORE_PATH))
}

fn stable_validation_id(artifact_id: &str) -> String {
    let digest = Sha256::digest(
        format!("{CAT_RECORDING_VALIDATION_POLICY_VERSION}:{artifact_id}").as_bytes(),
    );
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("catval_{}", &hex[..24])
}

fn sanitize_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control() || *character == '\n')
        .take(MAX_TEXT_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

pub fn sanitize_decision(
    mut decision: CatRecordingValidationDecision,
) -> CatRecordingValidationDecision {
    decision
        .cat_frame_indices
        .retain(|index| (1..=MAX_CAT_RECORDING_SAMPLE_FRAMES as u8).contains(index));
    decision.cat_frame_indices.sort_unstable();
    decision.cat_frame_indices.dedup();
    decision.behavior_tags = decision
        .behavior_tags
        .into_iter()
        .map(|value| sanitize_text(&value))
        .filter(|value| !value.is_empty())
        .take(MAX_BEHAVIOR_TAGS)
        .collect();
    decision.summary = sanitize_text(&decision.summary);
    decision.reason_code = sanitize_text(&decision.reason_code);
    decision.sampled_frame_count = decision
        .sampled_frame_count
        .min(MAX_CAT_RECORDING_SAMPLE_FRAMES as u8);
    decision.sampling_strategy = sanitize_text(&decision.sampling_strategy);
    decision.validation_rounds = decision.validation_rounds.min(2);
    decision
        .sampled_offsets_ms
        .truncate(MAX_CAT_RECORDING_SAMPLE_FRAMES);
    decision.frame_predictions.retain(|prediction| {
        (1..=MAX_CAT_RECORDING_SAMPLE_FRAMES as u8).contains(&prediction.frame_index)
    });
    for prediction in &mut decision.frame_predictions {
        prediction.cat_probability_ppm = prediction.cat_probability_ppm.min(1_000_000);
    }
    decision
        .frame_predictions
        .sort_by_key(|prediction| prediction.frame_index);
    decision
        .frame_predictions
        .dedup_by_key(|prediction| prediction.frame_index);
    decision
        .frame_predictions
        .truncate(MAX_CAT_RECORDING_SAMPLE_FRAMES);
    decision.model_endpoint_id = sanitize_text(&decision.model_endpoint_id);
    decision.model_name = sanitize_text(&decision.model_name);
    decision
}

fn sanitize_detection_evidence(evidence: &[CatDetectionEvidence]) -> Vec<CatDetectionEvidence> {
    let mut evidence = evidence
        .iter()
        .filter(|item| item.sequence > 0 && item.frame_epoch_ms > 0)
        .cloned()
        .collect::<Vec<_>>();
    evidence.sort_by_key(|item| (item.frame_epoch_ms, item.sequence));
    let mut seen_sequences = HashSet::new();
    evidence.retain(|item| seen_sequences.insert(item.sequence));
    for item in &mut evidence {
        item.confidence_ppm = item.confidence_ppm.min(1_000_000);
    }
    if evidence.len() > MAX_CAT_DETECTION_EVIDENCE {
        let last = evidence.pop();
        evidence.truncate(MAX_CAT_DETECTION_EVIDENCE - usize::from(last.is_some()));
        if let Some(last) = last {
            evidence.push(last);
        }
    }
    evidence
}

fn store_lock() -> &'static Mutex<()> {
    STORE_LOCK.get_or_init(|| Mutex::new(()))
}

fn epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artifact(id: &str) -> HarborLinkRecordingArtifact {
        HarborLinkRecordingArtifact {
            media_contract_version: "1.0".to_string(),
            artifact_id: id.to_string(),
            camera_id: "camera-252".to_string(),
            kind: "recording".to_string(),
            mime_type: "video/mp4".to_string(),
            byte_size: 1024,
            started_at_epoch_ms: 1000,
            ended_at_epoch_ms: 6000,
            duration_seconds: 5,
            stream_kind: "mainstream".to_string(),
            modified_at_epoch_ms: 6000,
            preview_url: "/v1/dvr/artifacts/test".to_string(),
            event_id: Some("cat-activity-1".to_string()),
            labels: vec!["cat".to_string()],
            source: Some("yolo_cat_activity".to_string()),
        }
    }

    fn temp_store(name: &str) -> CatRecordingValidationStore {
        let unique = epoch_ms();
        CatRecordingValidationStore::new(
            env::temp_dir().join(format!("{name}-{}-{unique}.jsonl", std::process::id())),
        )
    }

    #[test]
    fn candidate_registration_is_idempotent_and_acceptance_publishes() {
        let store = temp_store("cat-validation-idempotent");
        let first = store.register_candidate(&artifact("artifact-1")).unwrap();
        let second = store.register_candidate(&artifact("artifact-1")).unwrap();
        assert_eq!(first.validation_id, second.validation_id);
        assert_eq!(store.list_latest().unwrap().len(), 1);

        let processing = store.mark_processing("artifact-1").unwrap();
        assert_eq!(processing.attempt_count, 1);
        let accepted = store
            .complete(
                "artifact-1",
                CatRecordingValidationStatus::Accepted,
                Some(CatRecordingValidationDecision {
                    cat_present: true,
                    cat_frame_indices: vec![2, 4],
                    behavior_tags: vec!["walking".to_string()],
                    summary: "猫在房间内走动".to_string(),
                    reason_code: "cat_in_multiple_frames".to_string(),
                    sampled_frame_count: 5,
                    sampling_strategy: "yolo_guided".to_string(),
                    validation_rounds: 1,
                    sampled_offsets_ms: vec![500, 1500, 2500, 3500, 4500],
                    frame_predictions: Vec::new(),
                    model_endpoint_id: "vlm-local".to_string(),
                    model_name: "test-vlm".to_string(),
                    elapsed_ms: 100,
                }),
                None,
            )
            .unwrap();
        assert!(accepted.is_published());

        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn candidate_registration_sanitizes_bounded_detection_evidence() {
        let store = temp_store("cat-validation-evidence");
        let evidence = vec![
            CatDetectionEvidence {
                sequence: 2,
                frame_epoch_ms: 2500,
                confidence_ppm: 1_500_000,
            },
            CatDetectionEvidence {
                sequence: 1,
                frame_epoch_ms: 1500,
                confidence_ppm: 800_000,
            },
            CatDetectionEvidence {
                sequence: 1,
                frame_epoch_ms: 1600,
                confidence_ppm: 900_000,
            },
        ];

        let record = store
            .register_candidate_with_evidence(&artifact("artifact-evidence"), &evidence)
            .unwrap();

        assert_eq!(record.detection_evidence.len(), 2);
        assert_eq!(record.detection_evidence[0].sequence, 1);
        assert_eq!(record.detection_evidence[1].confidence_ppm, 1_000_000);
        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn scheduled_retry_is_not_claimed_before_due_time() {
        let store = temp_store("cat-validation-retry");
        store
            .register_candidate(&artifact("artifact-retry"))
            .unwrap();
        store.mark_processing("artifact-retry").unwrap();
        store
            .schedule_retry("artifact-retry", epoch_ms() + 60_000, "vlm_busy")
            .unwrap();

        assert!(store.next_pending().unwrap().is_none());

        store
            .schedule_retry("artifact-retry", 0, "retry_due")
            .unwrap();
        let pending = store.next_pending().unwrap().unwrap();
        assert_eq!(pending.attempt_count, 1);
        assert_eq!(pending.last_error.as_deref(), Some("retry_due"));
        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn resource_contention_defers_without_consuming_attempt() {
        let store = temp_store("cat-validation-resource-contention");
        store
            .register_candidate(&artifact("artifact-resource-contention"))
            .unwrap();
        let processing = store
            .mark_processing("artifact-resource-contention")
            .unwrap();
        assert_eq!(processing.attempt_count, 1);

        let deferred = store
            .defer_resource_contention(
                "artifact-resource-contention",
                epoch_ms() + 5_000,
                "cat_classifier_ai_resource_wait_timeout",
            )
            .unwrap();

        assert_eq!(
            deferred.validation_status,
            CatRecordingValidationStatus::PendingValidation
        );
        assert_eq!(deferred.attempt_count, 0);
        assert!(deferred.next_retry_at_epoch_ms.is_some());
        assert_eq!(
            deferred.last_error.as_deref(),
            Some("cat_classifier_ai_resource_wait_timeout")
        );
        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn validation_records_without_v3_fields_remain_readable() {
        let store = temp_store("cat-validation-backward-compatible");
        let record = store
            .register_candidate(&artifact("artifact-legacy"))
            .unwrap();
        let mut value = serde_json::to_value(record).unwrap();
        let object = value.as_object_mut().unwrap();
        object.remove("detection_evidence");
        object.remove("next_retry_at_epoch_ms");

        let parsed = serde_json::from_value::<CatRecordingValidationRecord>(value).unwrap();

        assert!(parsed.detection_evidence.is_empty());
        assert!(parsed.next_retry_at_epoch_ms.is_none());
        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn interrupted_processing_returns_to_pending() {
        let store = temp_store("cat-validation-recovery");
        store.register_candidate(&artifact("artifact-2")).unwrap();
        store.mark_processing("artifact-2").unwrap();

        assert_eq!(store.recover_interrupted().unwrap(), 1);
        let pending = store.next_pending().unwrap().unwrap();
        assert_eq!(
            pending.validation_status,
            CatRecordingValidationStatus::PendingValidation
        );
        assert_eq!(pending.attempt_count, 1);

        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn malformed_tail_is_ignored() {
        let store = temp_store("cat-validation-malformed");
        store.register_candidate(&artifact("artifact-3")).unwrap();
        let mut file = OpenOptions::new().append(true).open(store.path()).unwrap();
        file.write_all(b"{broken\n").unwrap();

        assert_eq!(store.list_latest().unwrap().len(), 1);
        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn hard_gate_rejects_short_recordings_before_model_acceptance() {
        let mut candidate = artifact("recordings~camera-252~short.mp4");
        candidate.started_at_epoch_ms = 10_000;
        let store = temp_store("cat-validation-short-gate");
        let record = store.register_candidate(&candidate).unwrap();

        assert_eq!(
            cat_recording_hard_gate_reason(&record, 4_999),
            Some((
                CatRecordingValidationStatus::Rejected,
                "recording_shorter_than_minimum_duration"
            ))
        );
        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn hard_gate_requires_yolo_evidence_inside_media_window() {
        let mut candidate = artifact("recordings~camera-252~timing.mp4");
        candidate.started_at_epoch_ms = 10_000;
        let store = temp_store("cat-validation-timing-gate");
        let mut record = store.register_candidate(&candidate).unwrap();

        assert_eq!(
            cat_recording_hard_gate_reason(&record, 8_000),
            Some((
                CatRecordingValidationStatus::ReviewRequired,
                "recording_has_no_in_window_yolo_evidence"
            ))
        );
        record.detection_evidence = vec![CatDetectionEvidence {
            sequence: 1,
            frame_epoch_ms: 18_001,
            confidence_ppm: 900_000,
        }];
        assert!(cat_recording_hard_gate_reason(&record, 8_000).is_some());
        record.detection_evidence[0].frame_epoch_ms = 12_000;
        assert_eq!(cat_recording_hard_gate_reason(&record, 8_000), None);
        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn only_new_auto_cat_rejections_are_eligible_for_physical_discard() {
        let store = temp_store("cat-validation-discard-policy");
        let mut rejected = store
            .register_candidate(&artifact("recordings~camera-252~rejected.mp4"))
            .unwrap();
        rejected.validation_status = CatRecordingValidationStatus::Rejected;
        assert!(rejected.is_physical_discard_eligible());

        let mut manual = rejected.clone();
        manual.artifact_source = Some("manual_recording".to_string());
        assert!(!manual.is_physical_discard_eligible());

        let mut legacy = rejected.clone();
        legacy.policy_version = "cat-recording-validation-v4".to_string();
        assert!(!legacy.is_physical_discard_eligible());

        let mut accepted = rejected;
        accepted.validation_status = CatRecordingValidationStatus::Accepted;
        accepted.publication_status = CatRecordingPublicationStatus::Published;
        assert!(!accepted.is_physical_discard_eligible());
        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn physical_discard_keeps_an_append_only_audit_tombstone() {
        let store = temp_store("cat-validation-discard-audit");
        store
            .register_candidate(&artifact("recordings~camera-252~discard.mp4"))
            .unwrap();
        store
            .complete(
                "recordings~camera-252~discard.mp4",
                CatRecordingValidationStatus::ReviewRequired,
                None,
                Some("recording_has_no_in_window_yolo_evidence"),
            )
            .unwrap();

        let pending = store
            .mark_artifact_discard_pending("recordings~camera-252~discard.mp4")
            .unwrap();
        assert_eq!(
            pending.artifact_disposition,
            CatRecordingArtifactDisposition::DiscardPending
        );
        let discarded = store
            .mark_artifact_discarded("recordings~camera-252~discard.mp4")
            .unwrap();
        assert_eq!(
            discarded.artifact_disposition,
            CatRecordingArtifactDisposition::Discarded
        );
        assert!(discarded.discarded_at_epoch_ms.is_some());
        assert!(store.pending_discards().unwrap().is_empty());
        assert_eq!(store.list_latest().unwrap().len(), 1);
        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn failed_physical_discard_is_retried_at_most_three_times() {
        let store = temp_store("cat-validation-discard-retry");
        store
            .register_candidate(&artifact("recordings~camera-252~retry-discard.mp4"))
            .unwrap();
        store
            .complete(
                "recordings~camera-252~retry-discard.mp4",
                CatRecordingValidationStatus::Rejected,
                None,
                Some("recording_shorter_than_minimum_duration"),
            )
            .unwrap();

        for attempt in 1..=3 {
            let pending = store
                .mark_artifact_discard_pending("recordings~camera-252~retry-discard.mp4")
                .unwrap();
            assert_eq!(pending.discard_attempt_count, attempt);
            store
                .mark_artifact_discard_failed(
                    "recordings~camera-252~retry-discard.mp4",
                    "harborlink_unavailable",
                )
                .unwrap();
            assert_eq!(store.pending_discards().unwrap().is_empty(), attempt == 3);
        }
        let _ = fs::remove_file(store.path());
    }
}
