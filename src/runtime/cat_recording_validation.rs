//! Durable validation state for YOLO-triggered cat recordings.

use std::collections::{HashMap, HashSet};
use std::env;
#[cfg(test)]
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::connectors::harborlink_media::HarborLinkRecordingArtifact;
use crate::runtime::cat_recording_classifier::CatRecordingFramePrediction;
use crate::runtime::secure_store_path::{SecureFileIdentity, SecureStorePath};

pub const CAT_RECORDING_VALIDATION_MODE_ENV: &str = "HARBOR_K3_CAT_RECORDING_VALIDATION_MODE";
pub const CAT_RECORDING_VALIDATION_STORE_PATH_ENV: &str =
    "HARBOR_K3_CAT_RECORDING_VALIDATION_STORE_PATH";
pub const CAT_RECORDING_VALIDATION_POLICY_VERSION: &str = "cat-recording-validation-v5";
pub const CAT_RECORDING_MINIMUM_DURATION_MS: u64 = 5_000;

const DEFAULT_STORE_PATH: &str = "/data/harborbeacon/cat-activity/validations.jsonl";
const MAX_STORE_BYTES: u64 = 64 * 1024 * 1024;
const COMPACTION_THRESHOLD_BYTES: u64 = 48 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 64 * 1024;
const MAX_TEXT_CHARS: usize = 500;
const MAX_BEHAVIOR_TAGS: usize = 16;
const MAX_CAT_DETECTION_EVIDENCE: usize = 256;
const MAX_CAT_RECORDING_SAMPLE_FRAMES: usize = 9;
const MAX_ARTIFACT_DISCARD_ATTEMPTS: u32 = 3;
const DEFAULT_ARTIFACT_DISCARD_CLAIM_LEASE_MS: u64 = 120_000;

static STORE_INDEXES: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<ValidationStoreIndex>>>>> =
    OnceLock::new();

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
pub struct CatRecordingDiscardTombstone {
    pub completed: bool,
    pub deleted_at_epoch_ms: u128,
    pub provider: String,
    pub provider_deleted: bool,
    pub provider_already_absent: bool,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_lease_deadline_epoch_ms: Option<u128>,
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
    #[serde(default)]
    pub discard_attempt_generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discard_attempt_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discard_attempt_lease_deadline_epoch_ms: Option<u128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discarded_at_epoch_ms: Option<u128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discard_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discard_tombstone: Option<CatRecordingDiscardTombstone>,
}

impl CatRecordingValidationRecord {
    pub fn is_published(&self) -> bool {
        self.validation_status == CatRecordingValidationStatus::Accepted
            && self.publication_status == CatRecordingPublicationStatus::Published
    }

    pub fn is_physical_discard_eligible(&self, mode: CatRecordingValidationMode) -> bool {
        mode == CatRecordingValidationMode::Enforce
            && self.policy_version == CAT_RECORDING_VALIDATION_POLICY_VERSION
            && self.artifact_id.starts_with("recordings~")
            && self.event_id.starts_with("cat-activity-")
            && self.artifact_source.as_deref() == Some("yolo_cat_activity")
            && self.validation_status == CatRecordingValidationStatus::Rejected
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
    secure_path: Arc<SecureStorePath>,
    generation_path: Arc<SecureStorePath>,
    compaction_threshold_bytes: u64,
    index: Arc<Mutex<ValidationStoreIndex>>,
    fail_next_compaction_before_commit: Arc<AtomicBool>,
    fail_next_append_after_generation: Arc<AtomicBool>,
    #[cfg(test)]
    test_hooks: Arc<ValidationStoreTestHooks>,
}

#[cfg(test)]
#[derive(Default)]
struct ValidationStoreTestHooks {
    before_archive_open: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    archive_enumeration_count: AtomicU64,
}

#[cfg(test)]
impl std::fmt::Debug for ValidationStoreTestHooks {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ValidationStoreTestHooks")
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ValidationMainFingerprint {
    len: u64,
    modified_epoch_nanos: Option<u128>,
    identity: SecureFileIdentity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ValidationStoreFingerprint {
    generation: u64,
    main: Option<ValidationMainFingerprint>,
}

#[derive(Debug, Clone)]
struct ValidationArchiveSegment {
    generation: u64,
    path: PathBuf,
}

#[derive(Debug, Default)]
struct ValidationStoreIndex {
    initialized: bool,
    latest: HashMap<String, CatRecordingValidationRecord>,
    fingerprint: Option<ValidationStoreFingerprint>,
    compaction_count: u64,
    disk_scan_count: u64,
}

struct ValidationStorePersistResult {
    fingerprint: ValidationStoreFingerprint,
    compacted: bool,
}

impl Default for CatRecordingValidationStore {
    fn default() -> Self {
        Self::new(default_store_path())
    }
}

impl CatRecordingValidationStore {
    pub fn new(path: PathBuf) -> Self {
        Self::try_new(path).unwrap_or_else(|error| {
            panic!("failed to initialize cat recording validation store: {error}")
        })
    }

    pub fn try_new(path: PathBuf) -> Result<Self, String> {
        let path = normalize_validation_store_path(path);
        let lock_path = validation_store_lock_path(&path);
        let secure_path = Arc::new(SecureStorePath::try_new(path.clone(), lock_path.clone())?);
        let path = secure_path.data_path().to_path_buf();
        let generation_path = Arc::new(SecureStorePath::try_new(
            validation_store_generation_path(&path),
            lock_path,
        )?);
        Ok(Self {
            index: shared_validation_store_index(&path),
            path,
            secure_path,
            generation_path,
            compaction_threshold_bytes: COMPACTION_THRESHOLD_BYTES,
            fail_next_compaction_before_commit: Arc::new(AtomicBool::new(false)),
            fail_next_append_after_generation: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            test_hooks: Arc::new(ValidationStoreTestHooks::default()),
        })
    }

    #[cfg(test)]
    fn new_with_compaction_threshold_for_test(path: PathBuf, threshold_bytes: u64) -> Self {
        Self {
            compaction_threshold_bytes: threshold_bytes.max(1),
            ..Self::new(path)
        }
    }

    #[cfg(test)]
    fn compaction_count_for_test(&self) -> u64 {
        self.index
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .compaction_count
    }

    #[cfg(test)]
    fn disk_scan_count_for_test(&self) -> u64 {
        self.index
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .disk_scan_count
    }

    #[cfg(test)]
    fn fail_next_compaction_before_commit_for_test(&self) {
        self.fail_next_compaction_before_commit
            .store(true, Ordering::SeqCst);
    }

    #[cfg(test)]
    fn fail_next_append_after_generation_for_test(&self) {
        self.fail_next_append_after_generation
            .store(true, Ordering::SeqCst);
    }

    #[cfg(test)]
    fn set_archive_open_hook_for_test(&self, hook: impl FnOnce() + Send + 'static) {
        *self
            .test_hooks
            .before_archive_open
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(Box::new(hook));
    }

    #[cfg(test)]
    fn archive_enumeration_count_for_test(&self) -> u64 {
        self.test_hooks
            .archive_enumeration_count
            .load(Ordering::SeqCst)
    }

    #[cfg(test)]
    fn lock_path_for_test(&self) -> PathBuf {
        validation_store_lock_path(&self.path)
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

        self.with_locked_index(|index| {
            if let Some(existing) = index.latest.get(artifact.artifact_id.as_str()) {
                return Ok(existing.clone());
            }

            let now = epoch_ms();
            let record = CatRecordingValidationRecord {
                schema_version: "1.5".to_string(),
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
                claim_owner: None,
                claim_token: None,
                claim_lease_deadline_epoch_ms: None,
                created_at_epoch_ms: now,
                updated_at_epoch_ms: now,
                decision: None,
                last_error: None,
                artifact_disposition: CatRecordingArtifactDisposition::Retained,
                discard_attempt_count: 0,
                discard_attempt_generation: 0,
                discard_attempt_token: None,
                discard_attempt_lease_deadline_epoch_ms: None,
                discarded_at_epoch_ms: None,
                discard_error: None,
                discard_tombstone: None,
            };
            let persisted = self.append_unlocked(&record)?;
            index
                .latest
                .insert(record.artifact_id.clone(), record.clone());
            index.fingerprint = Some(persisted.fingerprint);
            if persisted.compacted {
                index.compaction_count = index.compaction_count.saturating_add(1);
            }
            Ok(record)
        })
    }

    pub fn claim_next_pending(
        &self,
        owner: &str,
        lease_duration_ms: u128,
    ) -> Result<Option<CatRecordingValidationRecord>, String> {
        self.claim_next_pending_at(owner, epoch_ms(), lease_duration_ms)
    }

    fn claim_next_pending_at(
        &self,
        owner: &str,
        now: u128,
        lease_duration_ms: u128,
    ) -> Result<Option<CatRecordingValidationRecord>, String> {
        let owner = sanitize_text(owner);
        if owner.trim().is_empty() {
            return Err("cat recording validation claim owner is required".to_string());
        }
        if lease_duration_ms == 0 {
            return Err("cat recording validation claim lease must be positive".to_string());
        }
        self.with_locked_index(|index| {
            let Some(mut record) = index
                .latest
                .values()
                .filter(|record| {
                    record.validation_status == CatRecordingValidationStatus::PendingValidation
                        && record
                            .next_retry_at_epoch_ms
                            .is_none_or(|retry_at| retry_at <= now)
                })
                .min_by_key(|record| (record.created_at_epoch_ms, record.validation_id.clone()))
                .cloned()
            else {
                return Ok(None);
            };
            record.validation_status = CatRecordingValidationStatus::Processing;
            record.publication_status = CatRecordingPublicationStatus::Unpublished;
            record.attempt_count = record.attempt_count.saturating_add(1);
            record.next_retry_at_epoch_ms = None;
            record.last_error = None;
            record.claim_owner = Some(owner);
            record.claim_token = Some(Uuid::new_v4().simple().to_string());
            record.claim_lease_deadline_epoch_ms = Some(now.saturating_add(lease_duration_ms));
            record.updated_at_epoch_ms = now;
            self.persist_index_record(index, record).map(Some)
        })
    }

    pub fn complete(
        &self,
        artifact_id: &str,
        claim_token: &str,
        validation_status: CatRecordingValidationStatus,
        decision: Option<CatRecordingValidationDecision>,
        error: Option<&str>,
    ) -> Result<CatRecordingValidationRecord, String> {
        self.complete_at(
            artifact_id,
            claim_token,
            epoch_ms(),
            validation_status,
            decision,
            error,
        )
    }

    fn complete_at(
        &self,
        artifact_id: &str,
        claim_token: &str,
        now: u128,
        validation_status: CatRecordingValidationStatus,
        decision: Option<CatRecordingValidationDecision>,
        error: Option<&str>,
    ) -> Result<CatRecordingValidationRecord, String> {
        self.update_record_checked(artifact_id, |record| {
            validate_processing_claim(record, claim_token, now)?;
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
            clear_processing_claim(record);
            Ok(())
        })
    }

    pub fn schedule_retry(
        &self,
        artifact_id: &str,
        claim_token: &str,
        next_retry_at_epoch_ms: u128,
        error: &str,
    ) -> Result<CatRecordingValidationRecord, String> {
        self.schedule_retry_at(
            artifact_id,
            claim_token,
            epoch_ms(),
            next_retry_at_epoch_ms,
            error,
        )
    }

    fn schedule_retry_at(
        &self,
        artifact_id: &str,
        claim_token: &str,
        now: u128,
        next_retry_at_epoch_ms: u128,
        error: &str,
    ) -> Result<CatRecordingValidationRecord, String> {
        self.update_record_checked(artifact_id, |record| {
            validate_processing_claim(record, claim_token, now)?;
            record.validation_status = CatRecordingValidationStatus::PendingValidation;
            record.publication_status = CatRecordingPublicationStatus::Unpublished;
            record.decision = None;
            record.next_retry_at_epoch_ms = Some(next_retry_at_epoch_ms);
            record.last_error = Some(sanitize_text(error));
            clear_processing_claim(record);
            Ok(())
        })
    }

    pub fn defer_resource_contention(
        &self,
        artifact_id: &str,
        claim_token: &str,
        next_retry_at_epoch_ms: u128,
        error: &str,
    ) -> Result<CatRecordingValidationRecord, String> {
        let now = epoch_ms();
        self.update_record_checked(artifact_id, |record| {
            validate_processing_claim(record, claim_token, now)?;
            record.attempt_count = record.attempt_count.saturating_sub(1);
            record.validation_status = CatRecordingValidationStatus::PendingValidation;
            record.publication_status = CatRecordingPublicationStatus::Unpublished;
            record.decision = None;
            record.next_retry_at_epoch_ms = Some(next_retry_at_epoch_ms);
            record.last_error = Some(sanitize_text(error));
            clear_processing_claim(record);
            Ok(())
        })
    }

    pub fn recover_expired_claims(&self) -> Result<usize, String> {
        self.recover_expired_claims_at(epoch_ms())
    }

    fn recover_expired_claims_at(&self, now: u128) -> Result<usize, String> {
        self.with_locked_index(|index| {
            let mut artifact_ids = index
                .latest
                .values()
                .filter(|record| {
                    record.validation_status == CatRecordingValidationStatus::Processing
                        && record
                            .claim_lease_deadline_epoch_ms
                            .is_none_or(|deadline| deadline <= now)
                })
                .map(|record| record.artifact_id.clone())
                .collect::<Vec<_>>();
            artifact_ids.sort();
            let recovered = artifact_ids.len();
            for artifact_id in artifact_ids {
                let mut record = index.latest.get(&artifact_id).cloned().ok_or_else(|| {
                    format!("cat recording validation record not found: {artifact_id}")
                })?;
                record.validation_status = CatRecordingValidationStatus::PendingValidation;
                record.publication_status = CatRecordingPublicationStatus::Unpublished;
                record.next_retry_at_epoch_ms = None;
                record.last_error = Some("validation_claim_expired".to_string());
                clear_processing_claim(&mut record);
                record.updated_at_epoch_ms = now;
                self.persist_index_record(index, record)?;
            }
            Ok(recovered)
        })
    }

    pub fn mark_artifact_discard_pending(
        &self,
        artifact_id: &str,
        mode: CatRecordingValidationMode,
    ) -> Result<CatRecordingValidationRecord, String> {
        self.claim_artifact_discard(artifact_id, mode, DEFAULT_ARTIFACT_DISCARD_CLAIM_LEASE_MS)?
            .ok_or_else(|| "cat recording discard intent is already claimed".to_string())
    }

    pub fn claim_artifact_discard(
        &self,
        artifact_id: &str,
        mode: CatRecordingValidationMode,
        lease_duration_ms: u64,
    ) -> Result<Option<CatRecordingValidationRecord>, String> {
        if lease_duration_ms == 0 {
            return Err("cat recording discard claim lease must be positive".to_string());
        }
        self.with_locked_index(|index| {
            let mut record = index.latest.get(artifact_id).cloned().ok_or_else(|| {
                format!("cat recording validation record not found: {artifact_id}")
            })?;
            if !record.is_physical_discard_eligible(mode) {
                return Err("cat recording is not eligible for physical discard".to_string());
            }
            let now = epoch_ms();
            if record.artifact_disposition == CatRecordingArtifactDisposition::DiscardPending
                && record
                    .discard_attempt_token
                    .as_deref()
                    .is_some_and(|token| !token.is_empty())
                && record
                    .discard_attempt_lease_deadline_epoch_ms
                    .is_some_and(|deadline| deadline > now)
            {
                return Ok(None);
            }
            if record.discard_attempt_count >= MAX_ARTIFACT_DISCARD_ATTEMPTS {
                return Err("cat recording discard attempts are exhausted".to_string());
            }
            record.artifact_disposition = CatRecordingArtifactDisposition::DiscardPending;
            record.discard_attempt_count = record.discard_attempt_count.saturating_add(1);
            record.discard_attempt_generation = record.discard_attempt_generation.saturating_add(1);
            record.discard_attempt_token = Some(format!(
                "discard-{}-{}",
                record.discard_attempt_generation,
                Uuid::new_v4().simple()
            ));
            record.discard_attempt_lease_deadline_epoch_ms =
                Some(now.saturating_add(u128::from(lease_duration_ms)));
            record.discard_error = None;
            record.updated_at_epoch_ms = now;
            self.persist_index_record(index, record).map(Some)
        })
    }

    pub fn mark_artifact_discarded(
        &self,
        artifact_id: &str,
        discard_attempt_token: &str,
        provider_deleted: bool,
        provider_already_absent: bool,
    ) -> Result<CatRecordingValidationRecord, String> {
        self.update_record_checked(artifact_id, |record| {
            validate_discard_attempt(record, discard_attempt_token, epoch_ms())?;
            if !provider_deleted && !provider_already_absent {
                return Err("HarborLink did not confirm artifact deletion".to_string());
            }
            let deleted_at_epoch_ms = epoch_ms();
            record.artifact_disposition = CatRecordingArtifactDisposition::Discarded;
            record.discarded_at_epoch_ms = Some(deleted_at_epoch_ms);
            record.discard_error = None;
            record.discard_attempt_token = None;
            record.discard_attempt_lease_deadline_epoch_ms = None;
            record.discard_tombstone = Some(CatRecordingDiscardTombstone {
                completed: true,
                deleted_at_epoch_ms,
                provider: "harborlink".to_string(),
                provider_deleted,
                provider_already_absent,
            });
            Ok(())
        })
    }

    pub fn mark_artifact_discard_failed(
        &self,
        artifact_id: &str,
        discard_attempt_token: &str,
        error: &str,
    ) -> Result<CatRecordingValidationRecord, String> {
        self.update_record_checked(artifact_id, |record| {
            validate_discard_attempt(record, discard_attempt_token, epoch_ms())?;
            record.artifact_disposition = CatRecordingArtifactDisposition::DiscardFailed;
            record.discard_error = Some(sanitize_text(error));
            record.discard_attempt_token = None;
            record.discard_attempt_lease_deadline_epoch_ms = None;
            Ok(())
        })
    }

    pub fn pending_discards(
        &self,
        mode: CatRecordingValidationMode,
    ) -> Result<Vec<CatRecordingValidationRecord>, String> {
        Ok(self
            .list_latest()?
            .into_iter()
            .filter(|record| {
                matches!(
                    record.artifact_disposition,
                    CatRecordingArtifactDisposition::DiscardPending
                        | CatRecordingArtifactDisposition::DiscardFailed
                ) && record.discard_attempt_count < MAX_ARTIFACT_DISCARD_ATTEMPTS
                    && record.is_physical_discard_eligible(mode)
            })
            .collect())
    }

    #[cfg(test)]
    fn expire_discard_claim_for_test(&self, artifact_id: &str) -> Result<(), String> {
        self.update_record_checked(artifact_id, |record| {
            if record.artifact_disposition != CatRecordingArtifactDisposition::DiscardPending {
                return Err("discard attempt is not pending".to_string());
            }
            record.discard_attempt_lease_deadline_epoch_ms = Some(0);
            Ok(())
        })
        .map(|_| ())
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
        self.with_locked_index(|index| {
            let mut records = index.latest.values().cloned().collect::<Vec<_>>();
            records.sort_by_key(|record| std::cmp::Reverse(record.updated_at_epoch_ms));
            Ok(records)
        })
    }

    pub fn records_for_artifacts(
        &self,
        artifact_ids: &HashSet<String>,
    ) -> Result<HashMap<String, CatRecordingValidationRecord>, String> {
        if artifact_ids.is_empty() {
            return Ok(HashMap::new());
        }
        self.with_locked_index(|index| {
            Ok(index
                .latest
                .iter()
                .filter(|(artifact_id, _)| artifact_ids.contains(*artifact_id))
                .map(|(artifact_id, record)| (artifact_id.clone(), record.clone()))
                .collect())
        })
    }

    fn update_record_checked<F>(
        &self,
        artifact_id: &str,
        update: F,
    ) -> Result<CatRecordingValidationRecord, String>
    where
        F: FnOnce(&mut CatRecordingValidationRecord) -> Result<(), String>,
    {
        self.with_locked_index(|index| {
            let mut record = index.latest.get(artifact_id).cloned().ok_or_else(|| {
                format!("cat recording validation record not found: {artifact_id}")
            })?;
            update(&mut record)?;
            record.updated_at_epoch_ms = epoch_ms();
            self.persist_index_record(index, record)
        })
    }

    fn persist_index_record(
        &self,
        index: &mut ValidationStoreIndex,
        record: CatRecordingValidationRecord,
    ) -> Result<CatRecordingValidationRecord, String> {
        let persisted = self.append_unlocked(&record)?;
        index
            .latest
            .insert(record.artifact_id.clone(), record.clone());
        index.fingerprint = Some(persisted.fingerprint);
        if persisted.compacted {
            index.compaction_count = index.compaction_count.saturating_add(1);
        }
        Ok(record)
    }

    fn scan_latest_unlocked(
        &self,
    ) -> Result<HashMap<String, CatRecordingValidationRecord>, String> {
        let archives = self.archive_segments()?;
        self.read_generation(&archives)?;
        #[cfg(test)]
        if let Some(hook) = self
            .test_hooks
            .before_archive_open
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
        {
            hook();
        }
        let mut latest = HashMap::new();
        for archive in archives {
            let archive_name = archive
                .path
                .file_name()
                .ok_or_else(|| "cat recording validation archive name is missing".to_string())?;
            let opened = self
                .secure_path
                .open_sibling_read(archive_name)?
                .ok_or_else(|| {
                    format!(
                        "cat recording validation archive disappeared: {}",
                        archive.path.display()
                    )
                })?;
            if opened.len > MAX_STORE_BYTES {
                return Err(format!(
                    "cat recording validation archive exceeds {} bytes: {}",
                    MAX_STORE_BYTES,
                    archive.path.display()
                ));
            }
            let mut bytes = Vec::with_capacity(opened.len as usize);
            BufReader::new(opened.file)
                .read_to_end(&mut bytes)
                .map_err(|error| {
                    format!(
                        "failed to read cat recording validation archive {}: {error}",
                        archive.path.display()
                    )
                })?;
            apply_strict_archive_records(&archive.path, &bytes, &mut latest)?;
        }
        let Some(opened) = self.secure_path.open_data_read()? else {
            return Ok(latest);
        };
        if opened.len > MAX_STORE_BYTES {
            return Err(format!(
                "cat recording validation store exceeds {} bytes",
                MAX_STORE_BYTES
            ));
        }
        for line in BufReader::new(opened.file).lines() {
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

    fn append_unlocked(
        &self,
        record: &CatRecordingValidationRecord,
    ) -> Result<ValidationStorePersistResult, String> {
        let line = serde_json::to_vec(record)
            .map_err(|error| format!("failed to serialize cat validation record: {error}"))?;
        if line.len() > MAX_RECORD_BYTES {
            return Err("cat recording validation record is too large".to_string());
        }
        let record_bytes = line.len() as u64 + 1;
        if record_bytes > self.compaction_threshold_bytes || record_bytes > MAX_STORE_BYTES {
            return Err("cat recording validation record exceeds active segment limit".to_string());
        }
        let current_bytes = self
            .secure_path
            .open_data_read()?
            .map(|opened| opened.len)
            .unwrap_or_default();
        let projected_bytes = current_bytes.saturating_add(record_bytes);
        let generation = self.advance_generation()?;
        if self
            .fail_next_append_after_generation
            .swap(false, Ordering::SeqCst)
        {
            return Err("injected append failure after generation commit".to_string());
        }
        let compacted = if current_bytes > 0 && projected_bytes > self.compaction_threshold_bytes {
            self.rotate_unlocked(&line, generation)?;
            true
        } else {
            let mut opened = self.secure_path.open_data_append_create()?;
            opened
                .file
                .write_all(&line)
                .and_then(|_| opened.file.write_all(b"\n"))
                .and_then(|_| opened.file.sync_data())
                .map_err(|error| format!("failed to persist cat validation record: {error}"))?;
            false
        };
        Ok(ValidationStorePersistResult {
            fingerprint: self.store_fingerprint()?,
            compacted,
        })
    }

    fn rotate_unlocked(&self, line: &[u8], generation: u64) -> Result<(), String> {
        let opened = self
            .secure_path
            .open_data_read()?
            .ok_or_else(|| "cat recording validation active segment disappeared".to_string())?;
        if opened.len > MAX_STORE_BYTES {
            return Err(format!(
                "cat recording validation store exceeds {} bytes",
                MAX_STORE_BYTES
            ));
        }
        let mut current = Vec::with_capacity(opened.len as usize);
        BufReader::new(opened.file)
            .read_to_end(&mut current)
            .map_err(|error| format!("failed to read active validation segment: {error}"))?;
        let archive_path = validation_store_archive_path(&self.path, generation)?;
        let mut validated = HashMap::new();
        apply_strict_archive_records(&archive_path, &current, &mut validated)?;
        let archive_name = archive_path
            .file_name()
            .ok_or_else(|| "cat recording validation archive name is missing".to_string())?;
        self.secure_path
            .create_sibling_atomically(archive_name, &current, || Ok(()))
            .map_err(|error| {
                format!(
                    "failed to publish cat recording validation archive {}: {error}",
                    archive_path.display()
                )
            })?;
        let mut next_main = Vec::with_capacity(line.len() + 1);
        next_main.extend_from_slice(line);
        next_main.push(b'\n');
        self.secure_path
            .replace_data_atomically(&next_main, || {
                if self
                    .fail_next_compaction_before_commit
                    .swap(false, Ordering::SeqCst)
                {
                    return Err(std::io::Error::other("injected precommit failure"));
                }
                Ok(())
            })
            .map_err(|error| {
                format!(
                    "failed to rotate cat recording validation store {}: {error}",
                    self.path.display()
                )
            })?;
        Ok(())
    }

    fn archive_segments(&self) -> Result<Vec<ValidationArchiveSegment>, String> {
        #[cfg(test)]
        self.test_hooks
            .archive_enumeration_count
            .fetch_add(1, Ordering::SeqCst);
        let file_name =
            self.secure_path.data_file_name().to_str().ok_or_else(|| {
                "cat recording validation file name must be valid UTF-8".to_string()
            })?;
        let prefix = format!("{file_name}.archive.");
        let mut archives = Vec::new();
        for name in self.secure_path.sibling_names()? {
            let Some(name_text) = name.to_str() else {
                continue;
            };
            let Some(remainder) = name_text.strip_prefix(&prefix) else {
                continue;
            };
            let generation_text = remainder.strip_suffix(".jsonl").ok_or_else(|| {
                format!("invalid cat recording validation archive name: {name_text}")
            })?;
            if generation_text.len() != 20
                || !generation_text
                    .chars()
                    .all(|character| character.is_ascii_digit())
            {
                return Err(format!(
                    "invalid cat recording validation archive generation: {name_text}"
                ));
            }
            let generation = generation_text.parse::<u64>().map_err(|error| {
                format!("invalid cat recording validation archive generation: {error}")
            })?;
            archives.push(ValidationArchiveSegment {
                generation,
                path: self
                    .path
                    .parent()
                    .ok_or_else(|| "validation store path is missing a parent".to_string())?
                    .join(name),
            });
        }
        archives.sort_by_key(|archive| archive.generation);
        Ok(archives)
    }

    fn read_generation(&self, archives: &[ValidationArchiveSegment]) -> Result<u64, String> {
        let max_archive_generation = archives.last().map(|archive| archive.generation);
        let Some(generation) = self.read_generation_sidecar()? else {
            return if max_archive_generation.is_some() {
                Err(
                    "cat recording validation generation is missing while archives exist"
                        .to_string(),
                )
            } else {
                Ok(0)
            };
        };
        if max_archive_generation.is_some_and(|archive| generation < archive) {
            return Err(
                "cat recording validation generation is behind immutable archives".to_string(),
            );
        }
        Ok(generation)
    }

    fn read_generation_sidecar(&self) -> Result<Option<u64>, String> {
        let Some(opened) = self.generation_path.open_data_read()? else {
            return Ok(None);
        };
        let mut bytes = Vec::with_capacity(opened.len as usize);
        BufReader::new(opened.file)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("failed to read validation generation: {error}"))?;
        if bytes.len() != 21
            || bytes[20] != b'\n'
            || !bytes[..20].iter().all(|byte| byte.is_ascii_digit())
        {
            return Err("cat recording validation generation is invalid".to_string());
        }
        let generation = std::str::from_utf8(&bytes[..20])
            .map_err(|_| "cat recording validation generation is invalid".to_string())?
            .parse::<u64>()
            .map_err(|_| "cat recording validation generation is invalid".to_string())?;
        Ok(Some(generation))
    }

    fn advance_generation(&self) -> Result<u64, String> {
        let generation = self
            .read_generation_sidecar()?
            .unwrap_or_default()
            .checked_add(1)
            .ok_or_else(|| "cat recording validation generation is exhausted".to_string())?;
        let bytes = format!("{generation:020}\n");
        self.generation_path
            .replace_data_atomically(bytes.as_bytes(), || Ok(()))
            .map_err(|error| format!("failed to persist validation generation: {error}"))?;
        Ok(generation)
    }

    fn store_fingerprint(&self) -> Result<ValidationStoreFingerprint, String> {
        let generation = self.read_generation_sidecar()?.unwrap_or_default();
        let main = self
            .secure_path
            .open_data_read()?
            .map(|opened| ValidationMainFingerprint {
                len: opened.len,
                modified_epoch_nanos: opened.modified_epoch_nanos,
                identity: opened.identity,
            });
        Ok(ValidationStoreFingerprint { generation, main })
    }

    fn with_locked_index<T>(
        &self,
        operation: impl FnOnce(&mut ValidationStoreIndex) -> Result<T, String>,
    ) -> Result<T, String> {
        let mut index = self
            .index
            .lock()
            .map_err(|_| "cat recording validation store index lock is unavailable".to_string())?;
        let lock_file = self.secure_path.open_lock()?;
        lock_file.lock_exclusive().map_err(|error| {
            format!(
                "failed to lock cat recording validation store {}: {error}",
                self.path.display()
            )
        })?;

        let result = self
            .secure_path
            .ensure_lock_identity()
            .and_then(|_| self.secure_path.ensure_parent_identity())
            .and_then(|_| self.generation_path.ensure_parent_identity())
            .and_then(|_| repair_validation_store_truncated_tail(&self.secure_path))
            .and_then(|repaired| {
                if repaired {
                    index.initialized = false;
                }
                self.refresh_index_if_needed(&mut index)
            })
            .and_then(|_| operation(&mut index));
        let unlock_result = fs2::FileExt::unlock(&lock_file).map_err(|error| {
            format!(
                "failed to unlock cat recording validation store {}: {error}",
                self.path.display()
            )
        });
        match (result, unlock_result) {
            (Ok(value), Ok(())) => Ok(value),
            (Err(error), _) => Err(error),
            (Ok(_), Err(error)) => Err(error),
        }
    }

    fn refresh_index_if_needed(&self, index: &mut ValidationStoreIndex) -> Result<(), String> {
        let fingerprint = self.store_fingerprint()?;
        if index.initialized && index.fingerprint == Some(fingerprint) {
            return Ok(());
        }
        let latest = self.scan_latest_unlocked()?;
        index.latest = latest;
        index.fingerprint = Some(self.store_fingerprint()?);
        index.initialized = true;
        index.disk_scan_count = index.disk_scan_count.saturating_add(1);
        Ok(())
    }
}

fn validate_processing_claim(
    record: &CatRecordingValidationRecord,
    claim_token: &str,
    now: u128,
) -> Result<(), String> {
    let claim_is_current = record.validation_status == CatRecordingValidationStatus::Processing
        && !claim_token.trim().is_empty()
        && record.claim_token.as_deref() == Some(claim_token)
        && record
            .claim_owner
            .as_deref()
            .is_some_and(|owner| !owner.is_empty())
        && record
            .claim_lease_deadline_epoch_ms
            .is_some_and(|deadline| deadline > now);
    if claim_is_current {
        Ok(())
    } else {
        Err("cat recording validation claim is stale, expired, or invalid".to_string())
    }
}

fn validate_discard_attempt(
    record: &CatRecordingValidationRecord,
    discard_attempt_token: &str,
    now: u128,
) -> Result<(), String> {
    let attempt_is_current = record.artifact_disposition
        == CatRecordingArtifactDisposition::DiscardPending
        && !discard_attempt_token.trim().is_empty()
        && record.discard_attempt_token.as_deref() == Some(discard_attempt_token)
        && record
            .discard_attempt_lease_deadline_epoch_ms
            .is_some_and(|deadline| deadline > now);
    if attempt_is_current {
        Ok(())
    } else {
        Err("cat recording discard attempt is stale, expired, or invalid".to_string())
    }
}

fn clear_processing_claim(record: &mut CatRecordingValidationRecord) {
    record.claim_owner = None;
    record.claim_token = None;
    record.claim_lease_deadline_epoch_ms = None;
}

fn repair_validation_store_truncated_tail(path: &SecureStorePath) -> Result<bool, String> {
    let Some(mut opened) = path.open_data_read_write()? else {
        return Ok(false);
    };
    let file = &mut opened.file;
    let length = opened.len;
    if length == 0 {
        return Ok(false);
    }

    file.seek(SeekFrom::End(-1)).map_err(|error| {
        format!(
            "failed to inspect cat recording validation store {}: {error}",
            path.data_path().display()
        )
    })?;
    let mut last = [0_u8; 1];
    file.read_exact(&mut last).map_err(|error| {
        format!(
            "failed to inspect cat recording validation store {}: {error}",
            path.data_path().display()
        )
    })?;
    if last[0] == b'\n' {
        return Ok(false);
    }

    const SCAN_CHUNK_BYTES: usize = 8 * 1024;
    let mut buffer = [0_u8; SCAN_CHUNK_BYTES];
    let mut position = length;
    let truncate_to = loop {
        let chunk_len = position.min(SCAN_CHUNK_BYTES as u64) as usize;
        position -= chunk_len as u64;
        file.seek(SeekFrom::Start(position))
            .and_then(|_| file.read_exact(&mut buffer[..chunk_len]))
            .map_err(|error| {
                format!(
                    "failed to scan cat recording validation store {}: {error}",
                    path.data_path().display()
                )
            })?;
        if let Some(newline_index) = buffer[..chunk_len].iter().rposition(|byte| *byte == b'\n') {
            break position + newline_index as u64 + 1;
        }
        if position == 0 {
            break 0;
        }
    };
    file.set_len(truncate_to)
        .and_then(|_| file.sync_data())
        .map_err(|error| {
            format!(
                "failed to repair cat recording validation store {}: {error}",
                path.data_path().display()
            )
        })?;
    Ok(true)
}

fn shared_validation_store_index(path: &Path) -> Arc<Mutex<ValidationStoreIndex>> {
    let registry = STORE_INDEXES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut indexes = registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(index) = indexes.get(path).and_then(Weak::upgrade) {
        return index;
    }
    indexes.retain(|_, index| index.strong_count() > 0);
    let index = Arc::new(Mutex::new(ValidationStoreIndex::default()));
    indexes.insert(path.to_path_buf(), Arc::downgrade(&index));
    index
}

fn normalize_validation_store_path(path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn apply_strict_archive_records(
    path: &Path,
    bytes: &[u8],
    latest: &mut HashMap<String, CatRecordingValidationRecord>,
) -> Result<(), String> {
    if bytes.is_empty() || !bytes.ends_with(b"\n") {
        return Err(format!(
            "cat recording validation archive is incomplete: {}",
            path.display()
        ));
    }
    for line in bytes[..bytes.len() - 1].split(|byte| *byte == b'\n') {
        if line.is_empty() || line.len() > MAX_RECORD_BYTES {
            return Err(format!(
                "cat recording validation archive contains an invalid record: {}",
                path.display()
            ));
        }
        let record =
            serde_json::from_slice::<CatRecordingValidationRecord>(line).map_err(|error| {
                format!(
                    "cat recording validation archive contains invalid JSON {}: {error}",
                    path.display()
                )
            })?;
        latest.insert(record.artifact_id.clone(), record);
    }
    Ok(())
}

fn validation_store_generation_path(path: &Path) -> PathBuf {
    let mut generation_path = path.as_os_str().to_os_string();
    generation_path.push(".generation");
    PathBuf::from(generation_path)
}

fn validation_store_archive_path(path: &Path, generation: u64) -> Result<PathBuf, String> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "cat recording validation file name must be valid UTF-8".to_string())?;
    let parent = path
        .parent()
        .ok_or_else(|| "cat recording validation store path is missing a parent".to_string())?;
    Ok(parent.join(format!("{file_name}.archive.{generation:020}.jsonl")))
}

fn validation_store_lock_path(path: &Path) -> PathBuf {
    let mut lock_path = path.as_os_str().to_os_string();
    lock_path.push(".lock");
    PathBuf::from(lock_path)
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
    decision.validation_rounds = decision.validation_rounds.min(1);
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

    fn validation_archive_paths(path: &Path) -> Vec<PathBuf> {
        let file_name = path
            .file_name()
            .expect("validation store file name")
            .to_string_lossy();
        let prefix = format!("{file_name}.archive.");
        let mut archives = fs::read_dir(path.parent().expect("validation store parent"))
            .expect("validation store parent entries")
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                let generation = name.strip_prefix(&prefix)?.strip_suffix(".jsonl")?;
                (generation.len() == 20
                    && generation
                        .chars()
                        .all(|character| character.is_ascii_digit()))
                .then(|| entry.path())
            })
            .collect::<Vec<_>>();
        archives.sort();
        archives
    }

    fn claim_test_record(
        store: &CatRecordingValidationStore,
        artifact_id: &str,
    ) -> CatRecordingValidationRecord {
        let claim = store
            .claim_next_pending("test-worker", 60_000)
            .expect("claim pending validation")
            .expect("pending validation record");
        assert_eq!(claim.artifact_id, artifact_id);
        claim
    }

    fn create_test_directory_alias(target: &Path, alias: &Path) {
        #[cfg(windows)]
        {
            let status = std::process::Command::new("cmd.exe")
                .args(["/c", "mklink", "/J"])
                .arg(alias)
                .arg(target)
                .status()
                .expect("create directory junction");
            assert!(status.success(), "create directory junction");
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(target, alias).expect("create directory symlink");
    }

    #[test]
    fn candidate_registration_is_idempotent_and_acceptance_publishes() {
        let store = temp_store("cat-validation-idempotent");
        let first = store.register_candidate(&artifact("artifact-1")).unwrap();
        let second = store.register_candidate(&artifact("artifact-1")).unwrap();
        assert_eq!(first.validation_id, second.validation_id);
        assert_eq!(store.list_latest().unwrap().len(), 1);

        let processing = claim_test_record(&store, "artifact-1");
        assert_eq!(processing.attempt_count, 1);
        let accepted = store
            .complete(
                "artifact-1",
                processing.claim_token.as_deref().expect("claim token"),
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
        let first_claim = claim_test_record(&store, "artifact-retry");
        store
            .schedule_retry(
                "artifact-retry",
                first_claim
                    .claim_token
                    .as_deref()
                    .expect("first claim token"),
                epoch_ms() + 60_000,
                "vlm_busy",
            )
            .unwrap();

        assert!(store.next_pending().unwrap().is_none());

        let second_claim = store
            .claim_next_pending_at("test-worker", epoch_ms() + 60_001, 60_000)
            .expect("claim due retry")
            .expect("due retry record");
        store
            .schedule_retry_at(
                "artifact-retry",
                second_claim
                    .claim_token
                    .as_deref()
                    .expect("second claim token"),
                epoch_ms() + 60_001,
                0,
                "retry_due",
            )
            .unwrap();
        let pending = store.next_pending().unwrap().unwrap();
        assert_eq!(pending.attempt_count, 2);
        assert_eq!(pending.last_error.as_deref(), Some("retry_due"));
        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn resource_contention_defers_without_consuming_attempt() {
        let store = temp_store("cat-validation-resource-contention");
        store
            .register_candidate(&artifact("artifact-resource-contention"))
            .unwrap();
        let processing = claim_test_record(&store, "artifact-resource-contention");
        assert_eq!(processing.attempt_count, 1);

        let deferred = store
            .defer_resource_contention(
                "artifact-resource-contention",
                processing.claim_token.as_deref().expect("claim token"),
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
        assert!(parsed.discard_tombstone.is_none());
        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn interrupted_processing_returns_to_pending() {
        let store = temp_store("cat-validation-recovery");
        store.register_candidate(&artifact("artifact-2")).unwrap();
        let processing = store
            .claim_next_pending_at("test-worker", 1_000, 100)
            .unwrap()
            .unwrap();
        assert_eq!(processing.artifact_id, "artifact-2");

        assert_eq!(store.recover_expired_claims_at(1_099).unwrap(), 0);
        assert_eq!(store.recover_expired_claims_at(1_101).unwrap(), 1);
        let pending = store.next_pending().unwrap().unwrap();
        assert_eq!(
            pending.validation_status,
            CatRecordingValidationStatus::PendingValidation
        );
        assert_eq!(pending.attempt_count, 1);

        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn concurrent_stores_atomically_claim_one_pending_artifact() {
        let bootstrap = temp_store("cat-validation-claim-race");
        let path = bootstrap.path().to_path_buf();
        bootstrap
            .register_candidate(&artifact("artifact-claim-race"))
            .expect("register claim candidate");
        drop(bootstrap);
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let mut threads = Vec::new();
        for owner in ["worker-a", "worker-b"] {
            let store = CatRecordingValidationStore::new(path.clone());
            let thread_barrier = barrier.clone();
            threads.push(std::thread::spawn(move || {
                thread_barrier.wait();
                store
                    .claim_next_pending_at(owner, 1_000, 5_000)
                    .expect("claim transaction")
            }));
        }
        barrier.wait();
        let claims = threads
            .into_iter()
            .filter_map(|thread| thread.join().expect("claim thread"))
            .collect::<Vec<_>>();

        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].attempt_count, 1);
        assert_eq!(
            claims[0].validation_status,
            CatRecordingValidationStatus::Processing
        );
        assert!(matches!(
            claims[0].claim_owner.as_deref(),
            Some("worker-a" | "worker-b")
        ));
        assert!(claims[0]
            .claim_token
            .as_deref()
            .is_some_and(|token| !token.is_empty()));
        assert_eq!(claims[0].claim_lease_deadline_epoch_ms, Some(6_000));
        let latest = CatRecordingValidationStore::new(path)
            .list_latest()
            .expect("latest claimed record");
        assert_eq!(latest.len(), 1);
        assert_eq!(latest[0].claim_token, claims[0].claim_token);
    }

    #[test]
    fn validation_store_rejects_directory_alias_without_changing_main() {
        let root = env::temp_dir().join(format!(
            "cat-validation-path-alias-{}-{}",
            std::process::id(),
            epoch_ms()
        ));
        let trusted_parent = root.join("trusted");
        let alias_parent = root.join("alias");
        fs::create_dir_all(&trusted_parent).expect("trusted parent");
        create_test_directory_alias(&trusted_parent, &alias_parent);
        let trusted_path = trusted_parent.join("validations.jsonl");
        let trusted = CatRecordingValidationStore::try_new(trusted_path.clone())
            .expect("trusted validation store");
        trusted
            .register_candidate(&artifact("artifact-trusted-path"))
            .expect("trusted append");
        let original_main = fs::read(&trusted_path).expect("trusted main bytes");

        let alias_error =
            CatRecordingValidationStore::try_new(alias_parent.join("validations.jsonl"))
                .expect_err("directory alias must be rejected");

        assert!(alias_error.contains("alias") || alias_error.contains("reparse"));
        assert_eq!(
            fs::read(&trusted_path).expect("main after alias"),
            original_main
        );
    }

    #[test]
    fn validation_store_rejects_parent_swap_after_capability_binding() {
        let root = env::temp_dir().join(format!(
            "cat-validation-parent-swap-{}-{}",
            std::process::id(),
            epoch_ms()
        ));
        let ambient_parent = root.join("data");
        let moved_trusted_parent = root.join("trusted-moved");
        let attacker_parent = root.join("attacker");
        fs::create_dir_all(&ambient_parent).expect("trusted parent");
        fs::create_dir_all(&attacker_parent).expect("attacker parent");
        let store = CatRecordingValidationStore::try_new(ambient_parent.join("validations.jsonl"))
            .expect("bind trusted parent capability");
        let attacker_main = attacker_parent.join("validations.jsonl");
        fs::write(&attacker_main, b"attacker-main\n").expect("attacker main");
        let attacker_original = fs::read(&attacker_main).expect("attacker original bytes");
        if fs::rename(&ambient_parent, &moved_trusted_parent).is_err() {
            assert_eq!(
                fs::read(&attacker_main).expect("attacker bytes after blocked swap"),
                attacker_original
            );
            assert!(!ambient_parent.join("validations.jsonl").exists());
            return;
        }
        fs::rename(&attacker_parent, &ambient_parent).expect("swap attacker parent");

        let error = store
            .register_candidate(&artifact("artifact-after-parent-swap"))
            .expect_err("parent swap must be rejected");

        assert!(error.contains("identity") || error.contains("replaced"));
        assert_eq!(
            fs::read(ambient_parent.join("validations.jsonl")).expect("attacker bytes after swap"),
            attacker_original
        );
        assert!(!moved_trusted_parent.join("validations.jsonl").exists());
    }

    #[test]
    fn expired_claim_is_fenced_after_another_worker_takes_over() {
        let store_a = temp_store("cat-validation-claim-fencing");
        let path = store_a.path().to_path_buf();
        store_a
            .register_candidate(&artifact("artifact-claim-fencing"))
            .expect("register fencing candidate");
        let claim_a = store_a
            .claim_next_pending_at("worker-a", 1_000, 100)
            .expect("claim A")
            .expect("claim A record");
        assert_eq!(store_a.recover_expired_claims_at(1_099).unwrap(), 0);
        assert_eq!(store_a.recover_expired_claims_at(1_101).unwrap(), 1);

        let store_b = CatRecordingValidationStore::new(path);
        let claim_b = store_b
            .claim_next_pending_at("worker-b", 1_101, 5_000)
            .expect("claim B")
            .expect("claim B record");
        let before_stale_writes = store_b.list_latest().expect("ledger before stale writes");

        let stale_complete = store_a.complete_at(
            &claim_a.artifact_id,
            claim_a.claim_token.as_deref().expect("claim A token"),
            1_102,
            CatRecordingValidationStatus::Accepted,
            None,
            None,
        );
        let stale_retry = store_a.schedule_retry_at(
            &claim_a.artifact_id,
            claim_a.claim_token.as_deref().expect("claim A token"),
            1_102,
            2_000,
            "late worker retry",
        );

        assert!(stale_complete
            .expect_err("stale completion must be fenced")
            .contains("claim"));
        assert!(stale_retry
            .expect_err("stale retry must be fenced")
            .contains("claim"));
        assert_eq!(
            store_b.list_latest().expect("ledger after stale writes"),
            before_stale_writes
        );
        assert_ne!(claim_a.claim_token, claim_b.claim_token);
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
    fn truncated_tail_is_removed_before_append_and_restart_keeps_new_commit() {
        let store = temp_store("cat-validation-truncated-tail");
        let path = store.path().to_path_buf();
        store
            .register_candidate(&artifact("artifact-before-truncated-tail"))
            .expect("bootstrap record");
        let mut file = OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open validation store tail");
        file.write_all(b"{\"schema_version\":\"1.4\"")
            .expect("write truncated tail");
        file.sync_data().expect("sync truncated tail");
        drop(file);

        store
            .register_candidate(&artifact("artifact-after-truncated-tail"))
            .expect("append after truncated tail");
        drop(store);

        let restarted = CatRecordingValidationStore::new(path);
        let artifact_ids = restarted
            .list_latest()
            .expect("restart latest records")
            .into_iter()
            .map(|record| record.artifact_id)
            .collect::<HashSet<_>>();
        assert_eq!(
            artifact_ids,
            HashSet::from([
                "artifact-before-truncated-tail".to_string(),
                "artifact-after-truncated-tail".to_string(),
            ])
        );
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
        assert!(rejected.is_physical_discard_eligible(CatRecordingValidationMode::Enforce));
        assert!(!rejected.is_physical_discard_eligible(CatRecordingValidationMode::Shadow));

        let mut manual = rejected.clone();
        manual.artifact_source = Some("manual_recording".to_string());
        assert!(!manual.is_physical_discard_eligible(CatRecordingValidationMode::Enforce));

        let mut legacy = rejected.clone();
        legacy.policy_version = "cat-recording-validation-v4".to_string();
        assert!(!legacy.is_physical_discard_eligible(CatRecordingValidationMode::Enforce));

        let mut accepted = rejected;
        accepted.validation_status = CatRecordingValidationStatus::Accepted;
        accepted.publication_status = CatRecordingPublicationStatus::Published;
        assert!(!accepted.is_physical_discard_eligible(CatRecordingValidationMode::Enforce));
        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn physical_discard_keeps_an_append_only_audit_tombstone() {
        let store = temp_store("cat-validation-discard-audit");
        store
            .register_candidate(&artifact("recordings~camera-252~discard.mp4"))
            .unwrap();
        let claim = claim_test_record(&store, "recordings~camera-252~discard.mp4");
        store
            .complete(
                "recordings~camera-252~discard.mp4",
                claim.claim_token.as_deref().expect("claim token"),
                CatRecordingValidationStatus::Rejected,
                None,
                Some("recording_has_no_in_window_yolo_evidence"),
            )
            .unwrap();

        let pending = store
            .mark_artifact_discard_pending(
                "recordings~camera-252~discard.mp4",
                CatRecordingValidationMode::Enforce,
            )
            .unwrap();
        assert_eq!(
            pending.artifact_disposition,
            CatRecordingArtifactDisposition::DiscardPending
        );
        let discard_attempt_token = pending
            .discard_attempt_token
            .as_deref()
            .expect("discard attempt token");
        let discarded = store
            .mark_artifact_discarded(
                "recordings~camera-252~discard.mp4",
                discard_attempt_token,
                true,
                false,
            )
            .unwrap();
        assert_eq!(
            discarded.artifact_disposition,
            CatRecordingArtifactDisposition::Discarded
        );
        assert!(discarded.discarded_at_epoch_ms.is_some());
        let tombstone = discarded
            .discard_tombstone
            .as_ref()
            .expect("discard audit tombstone");
        assert!(tombstone.completed);
        assert_eq!(tombstone.provider, "harborlink");
        assert!(tombstone.provider_deleted);
        assert!(!tombstone.provider_already_absent);
        assert!(store
            .pending_discards(CatRecordingValidationMode::Enforce)
            .unwrap()
            .is_empty());
        assert_eq!(store.list_latest().unwrap().len(), 1);
        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn failed_physical_discard_is_retried_at_most_three_times() {
        let store = temp_store("cat-validation-discard-retry");
        store
            .register_candidate(&artifact("recordings~camera-252~retry-discard.mp4"))
            .unwrap();
        let claim = claim_test_record(&store, "recordings~camera-252~retry-discard.mp4");
        store
            .complete(
                "recordings~camera-252~retry-discard.mp4",
                claim.claim_token.as_deref().expect("claim token"),
                CatRecordingValidationStatus::Rejected,
                None,
                Some("recording_shorter_than_minimum_duration"),
            )
            .unwrap();

        for attempt in 1..=3 {
            let pending = store
                .mark_artifact_discard_pending(
                    "recordings~camera-252~retry-discard.mp4",
                    CatRecordingValidationMode::Enforce,
                )
                .unwrap();
            assert_eq!(pending.discard_attempt_count, attempt);
            let discard_attempt_token = pending
                .discard_attempt_token
                .as_deref()
                .expect("discard attempt token");
            store
                .mark_artifact_discard_failed(
                    "recordings~camera-252~retry-discard.mp4",
                    discard_attempt_token,
                    "harborlink_unavailable",
                )
                .unwrap();
            assert_eq!(
                store
                    .pending_discards(CatRecordingValidationMode::Enforce)
                    .unwrap()
                    .is_empty(),
                attempt == 3
            );
        }
        let _ = fs::remove_file(store.path());
    }

    #[test]
    fn discard_claim_is_atomic_and_stale_attempts_cannot_overwrite_takeover() {
        let store_a = temp_store("cat-validation-discard-fencing");
        let path = store_a.path().to_path_buf();
        let artifact_id = "recordings~camera-252~fenced-discard.mp4";
        store_a.register_candidate(&artifact(artifact_id)).unwrap();
        let validation_claim = claim_test_record(&store_a, artifact_id);
        store_a
            .complete(
                artifact_id,
                validation_claim
                    .claim_token
                    .as_deref()
                    .expect("validation claim token"),
                CatRecordingValidationStatus::Rejected,
                None,
                Some("recording_shorter_than_minimum_duration"),
            )
            .unwrap();
        let store_b = CatRecordingValidationStore::new(path.clone());
        let barrier = Arc::new(std::sync::Barrier::new(3));
        let mut workers = Vec::new();
        for store in [store_a.clone(), store_b.clone()] {
            let barrier = barrier.clone();
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                store
                    .claim_artifact_discard(
                        artifact_id,
                        CatRecordingValidationMode::Enforce,
                        60_000,
                    )
                    .expect("discard claim")
            }));
        }
        barrier.wait();
        let claims = workers
            .into_iter()
            .map(|worker| worker.join().expect("discard worker"))
            .collect::<Vec<_>>();
        assert_eq!(claims.iter().filter(|claim| claim.is_some()).count(), 1);
        let attempt_a = claims
            .into_iter()
            .flatten()
            .next()
            .expect("winning discard claim");
        let token_a = attempt_a
            .discard_attempt_token
            .as_deref()
            .expect("attempt A token")
            .to_string();
        store_a
            .expire_discard_claim_for_test(artifact_id)
            .expect("expire attempt A");

        let attempt_b = store_b
            .claim_artifact_discard(artifact_id, CatRecordingValidationMode::Enforce, 60_000)
            .expect("take over expired discard")
            .expect("attempt B claim");
        let token_b = attempt_b
            .discard_attempt_token
            .as_deref()
            .expect("attempt B token")
            .to_string();
        assert_ne!(token_a, token_b);
        assert!(attempt_b.discard_attempt_generation > attempt_a.discard_attempt_generation);

        assert!(store_a
            .mark_artifact_discard_failed(artifact_id, &token_a, "late failure")
            .is_err());
        assert!(store_a
            .mark_artifact_discarded(artifact_id, &token_a, true, false)
            .is_err());
        let pending_b = store_b.list_latest().unwrap().pop().expect("pending B");
        assert_eq!(
            pending_b.discard_attempt_token.as_deref(),
            Some(token_b.as_str())
        );
        assert_eq!(
            pending_b.artifact_disposition,
            CatRecordingArtifactDisposition::DiscardPending
        );

        let discarded = store_b
            .mark_artifact_discarded(artifact_id, &token_b, true, false)
            .expect("attempt B terminal tombstone");
        assert_eq!(
            discarded.artifact_disposition,
            CatRecordingArtifactDisposition::Discarded
        );
        assert!(store_a
            .mark_artifact_discard_failed(artifact_id, &token_a, "very late failure")
            .is_err());
        assert_eq!(
            store_a
                .list_latest()
                .unwrap()
                .pop()
                .expect("terminal")
                .artifact_disposition,
            CatRecordingArtifactDisposition::Discarded
        );
    }

    #[test]
    fn validation_store_p2_near_threshold_compacts_latest_tombstone() {
        let path = temp_store("cat-validation-p2-near-threshold")
            .path()
            .to_path_buf();
        let bootstrap = CatRecordingValidationStore::new(path.clone());
        let artifact_id = "recordings~camera-252~compact-tombstone.mp4";
        bootstrap
            .register_candidate(&artifact(artifact_id))
            .expect("bootstrap validation");
        let first_record_bytes = fs::metadata(&path).expect("store metadata").len();
        drop(bootstrap);
        let store = CatRecordingValidationStore::new_with_compaction_threshold_for_test(
            path.clone(),
            first_record_bytes.saturating_mul(2),
        );

        let claim = claim_test_record(&store, artifact_id);
        store
            .complete(
                artifact_id,
                claim.claim_token.as_deref().expect("claim token"),
                CatRecordingValidationStatus::Rejected,
                None,
                Some("recording_shorter_than_minimum_duration"),
            )
            .expect("complete rejected validation");
        let pending = store
            .mark_artifact_discard_pending(artifact_id, CatRecordingValidationMode::Enforce)
            .expect("persist discard intent");
        store
            .mark_artifact_discarded(
                artifact_id,
                pending
                    .discard_attempt_token
                    .as_deref()
                    .expect("discard attempt token"),
                true,
                false,
            )
            .expect("persist discard tombstone");

        assert!(store.compaction_count_for_test() >= 1);
        assert_eq!(
            fs::read_to_string(&path)
                .expect("compacted store")
                .lines()
                .count(),
            1
        );
        let latest = store.list_latest().expect("latest records");
        assert_eq!(latest.len(), 1);
        assert_eq!(
            latest[0].artifact_disposition,
            CatRecordingArtifactDisposition::Discarded
        );
        assert!(latest[0].discard_tombstone.is_some());
    }

    #[test]
    fn validation_store_p2_unique_records_rotate_into_bounded_archives() {
        let path = temp_store("cat-validation-p2-unique-rotation")
            .path()
            .to_path_buf();
        let bootstrap = CatRecordingValidationStore::new(path.clone());
        bootstrap
            .register_candidate(&artifact("artifact-rotation-0"))
            .expect("bootstrap validation");
        let one_record_bytes = fs::metadata(&path).expect("store metadata").len();
        drop(bootstrap);
        let threshold = one_record_bytes.saturating_mul(2);
        let store = CatRecordingValidationStore::new_with_compaction_threshold_for_test(
            path.clone(),
            threshold,
        );
        for index in 1..=6 {
            store
                .register_candidate(&artifact(&format!("artifact-rotation-{index}")))
                .expect("unique validation should remain writable across rotations");
        }

        let archives = validation_archive_paths(&path);
        assert!(
            archives.len() >= 2,
            "unique records must rotate into archives"
        );
        assert!(
            fs::metadata(&path).expect("active main metadata").len() <= threshold,
            "active main must remain within its segment threshold"
        );
        for archive in &archives {
            let bytes = fs::read(archive).expect("archive bytes");
            assert!(bytes.ends_with(b"\n"));
            assert!(bytes.len() as u64 <= threshold);
            for line in bytes
                .split(|byte| *byte == b'\n')
                .filter(|line| !line.is_empty())
            {
                serde_json::from_slice::<CatRecordingValidationRecord>(line)
                    .expect("every immutable archive line must be complete JSON");
            }
        }
        drop(store);

        assert_eq!(
            CatRecordingValidationStore::new(path)
                .list_latest()
                .expect("restart latest records")
                .len(),
            7
        );
    }

    #[test]
    fn validation_store_p2_archive_open_stays_bound_to_enumerated_parent() {
        let root = env::temp_dir().join(format!(
            "cat-validation-p2-archive-parent-{}-{}",
            std::process::id(),
            Uuid::new_v4().simple()
        ));
        let ambient_parent = root.join("bound");
        let attacker_parent = root.join("attacker");
        fs::create_dir_all(&ambient_parent).expect("bound parent");
        fs::create_dir_all(&attacker_parent).expect("attacker parent");
        let path = ambient_parent.join("validations.jsonl");
        let attacker_path = attacker_parent.join("validations.jsonl");

        for (store_path, artifact_prefix) in
            [(path.clone(), "bound"), (attacker_path.clone(), "attacker")]
        {
            let bootstrap = CatRecordingValidationStore::new(store_path.clone());
            bootstrap
                .register_candidate(&artifact(&format!("artifact-{artifact_prefix}-0")))
                .expect("bootstrap archive parent fixture");
            let threshold = fs::metadata(&store_path)
                .expect("fixture main metadata")
                .len()
                .saturating_mul(2);
            drop(bootstrap);
            let rotating = CatRecordingValidationStore::new_with_compaction_threshold_for_test(
                store_path, threshold,
            );
            rotating
                .register_candidate(&artifact(&format!("artifact-{artifact_prefix}-1")))
                .expect("second archive parent fixture");
            rotating
                .register_candidate(&artifact(&format!("artifact-{artifact_prefix}-2")))
                .expect("rotate archive parent fixture");
        }
        let _attacker_bytes_before = fs::read(&attacker_path).expect("attacker main bytes");
        let moved_parent = root.join("moved-bound");
        let store = CatRecordingValidationStore::new(path);
        store.set_archive_open_hook_for_test(move || {
            #[cfg(unix)]
            {
                fs::rename(&ambient_parent, &moved_parent).expect("move bound parent");
                fs::rename(&attacker_parent, &ambient_parent).expect("swap attacker parent");
            }
            #[cfg(windows)]
            {
                assert!(
                    fs::rename(&ambient_parent, &moved_parent).is_err(),
                    "the bound Windows parent handle must block replacement"
                );
            }
        });

        #[cfg(unix)]
        {
            let error = store
                .list_latest()
                .expect_err("parent swap between enumeration and open must fail closed");
            assert!(error.contains("ancestor identity was replaced"), "{error}");
            assert_eq!(
                fs::read(root.join("bound").join("validations.jsonl"))
                    .expect("swapped attacker bytes"),
                _attacker_bytes_before
            );
        }
        #[cfg(windows)]
        assert!(store.list_latest().is_ok());
    }

    #[test]
    fn validation_store_p2_queries_do_not_enumerate_one_hundred_archives() {
        let path = temp_store("cat-validation-p2-archive-query-cost")
            .path()
            .to_path_buf();
        let bootstrap = CatRecordingValidationStore::new(path.clone());
        bootstrap
            .register_candidate(&artifact("artifact-enum-0000"))
            .expect("bootstrap enumeration fixture");
        let one_record_bytes = fs::metadata(&path).expect("store metadata").len();
        drop(bootstrap);
        let store = CatRecordingValidationStore::new_with_compaction_threshold_for_test(
            path.clone(),
            one_record_bytes.saturating_mul(2),
        );
        for index in 1..=201 {
            store
                .register_candidate(&artifact(&format!("artifact-enum-{index:04}")))
                .expect("create bounded archive fixture");
        }
        assert!(validation_archive_paths(&path).len() >= 100);
        let enumerations_before_queries = store.archive_enumeration_count_for_test();

        for _ in 0..100 {
            assert_eq!(
                store.list_latest().expect("cached latest records").len(),
                202
            );
        }

        assert_eq!(
            store.archive_enumeration_count_for_test(),
            enumerations_before_queries,
            "unchanged queries must use the fixed-size generation/main fingerprint"
        );
    }

    #[test]
    fn validation_store_p2_same_length_same_mtime_replace_refreshes_by_file_identity() {
        let store = temp_store("cat-validation-p2-file-identity-refresh");
        let path = store.path().to_path_buf();
        let artifact_id = "artifact-identity-refresh";
        store
            .register_candidate(&artifact(artifact_id))
            .expect("register identity fixture");
        let claim = claim_test_record(&store, artifact_id);
        store
            .complete(
                artifact_id,
                claim.claim_token.as_deref().expect("claim token"),
                CatRecordingValidationStatus::Accepted,
                None,
                None,
            )
            .expect("complete accepted fixture");
        store.list_latest().expect("initialize cached fingerprint");
        let scans_before_replace = store.disk_scan_count_for_test();
        let mut replacement = fs::read(&path).expect("main before replacement");
        let accepted = b"\"validation_status\":\"accepted\"";
        let rejected = b"\"validation_status\":\"rejected\"";
        assert_eq!(accepted.len(), rejected.len());
        let offset = replacement
            .windows(accepted.len())
            .rposition(|window| window == accepted)
            .expect("accepted status in latest record");
        replacement[offset..offset + accepted.len()].copy_from_slice(rejected);
        let old_fingerprint = store
            .index
            .lock()
            .expect("validation index")
            .fingerprint
            .expect("cached fingerprint");
        atomicwrites::AtomicFile::new(&path, atomicwrites::AllowOverwrite)
            .write(|file| file.write_all(&replacement).and_then(|_| file.sync_all()))
            .expect("same-length atomic replacement");
        let replacement_main = store
            .secure_path
            .open_data_read()
            .expect("open replacement")
            .expect("replacement main");
        assert_eq!(
            replacement_main.len,
            old_fingerprint.main.expect("old main fingerprint").len
        );
        assert_ne!(
            replacement_main.identity,
            old_fingerprint.main.expect("old main fingerprint").identity
        );
        {
            let mut index = store.index.lock().expect("validation index");
            index
                .fingerprint
                .as_mut()
                .expect("cached fingerprint")
                .main
                .as_mut()
                .expect("cached main fingerprint")
                .modified_epoch_nanos = replacement_main.modified_epoch_nanos;
        }

        let latest = store.list_latest().expect("refresh replaced main");

        assert_eq!(store.disk_scan_count_for_test(), scans_before_replace + 1);
        assert_eq!(latest.len(), 1);
        assert_eq!(
            latest[0].validation_status,
            CatRecordingValidationStatus::Rejected
        );
    }

    #[test]
    fn validation_store_p2_precommit_fault_keeps_old_main_and_restart_state() {
        let path = temp_store("cat-validation-p2-precommit")
            .path()
            .to_path_buf();
        let bootstrap = CatRecordingValidationStore::new(path.clone());
        bootstrap
            .register_candidate(&artifact("artifact-precommit"))
            .expect("bootstrap validation");
        let old_main = fs::read(&path).expect("old main bytes");
        let threshold = fs::metadata(&path)
            .expect("store metadata")
            .len()
            .saturating_mul(2);
        drop(bootstrap);
        let store = CatRecordingValidationStore::new_with_compaction_threshold_for_test(
            path.clone(),
            threshold,
        );
        store.fail_next_compaction_before_commit_for_test();

        let error = store
            .claim_next_pending("test-worker", 60_000)
            .expect_err("precommit fault must abort compaction");

        assert!(error.contains("injected precommit failure"), "{error}");
        assert_eq!(fs::read(&path).expect("main after fault"), old_main);
        let archives = validation_archive_paths(&path);
        assert_eq!(archives.len(), 1, "old main must be durably archived");
        assert_eq!(
            fs::read(&archives[0]).expect("archive after fault"),
            old_main,
            "published archive must contain only the previously committed main"
        );
        let orphan_dir = path.with_extension("atomicwrite-interrupted");
        fs::create_dir_all(&orphan_dir).expect("orphan atomicwrite dir");
        fs::write(orphan_dir.join("tmpfile.tmp"), b"uncommitted").expect("orphan temp file");
        drop(store);
        let restarted = CatRecordingValidationStore::new(path);
        let latest = restarted.list_latest().expect("restart latest state");
        assert_eq!(latest.len(), 1);
        assert_eq!(
            latest[0].validation_status,
            CatRecordingValidationStatus::PendingValidation
        );
        assert_eq!(latest[0].attempt_count, 0);
    }

    #[test]
    fn validation_store_p2_generation_commit_without_append_never_recovers_failed_record() {
        let path = temp_store("cat-validation-p2-generation-only")
            .path()
            .to_path_buf();
        let store = CatRecordingValidationStore::new(path.clone());
        store
            .register_candidate(&artifact("artifact-generation-old"))
            .expect("bootstrap validation");
        let old_main = fs::read(&path).expect("old main bytes");
        let scans_before_failure = store.disk_scan_count_for_test();
        store.fail_next_append_after_generation_for_test();

        let error = store
            .register_candidate(&artifact("artifact-generation-failed"))
            .expect_err("append failure after generation must reach caller");

        assert!(error.contains("injected append failure after generation commit"));
        assert_eq!(
            fs::read(&path).expect("main after append failure"),
            old_main
        );
        assert_eq!(store.disk_scan_count_for_test(), scans_before_failure);
        let latest = store
            .list_latest()
            .expect("refresh after generation-only commit");
        assert_eq!(store.disk_scan_count_for_test(), scans_before_failure + 1);
        assert!(
            latest
                .iter()
                .all(|record| record.artifact_id != "artifact-generation-failed"),
            "a record whose append failed must never be recovered"
        );
        store.list_latest().expect("stable second query");
        assert_eq!(store.disk_scan_count_for_test(), scans_before_failure + 1);
    }

    const VALIDATION_STORE_CHILD_PATH_ENV: &str = "HARBORBEACON_TEST_VALIDATION_STORE_CHILD_PATH";
    const VALIDATION_STORE_ROTATION_CHILD_PATH_ENV: &str =
        "HARBORBEACON_TEST_VALIDATION_STORE_ROTATION_CHILD_PATH";
    const VALIDATION_STORE_ROTATION_CHILD_THRESHOLD_ENV: &str =
        "HARBORBEACON_TEST_VALIDATION_STORE_ROTATION_CHILD_THRESHOLD";

    #[test]
    fn validation_store_p2_child_process_append_helper() {
        let Some(path) = env::var_os(VALIDATION_STORE_CHILD_PATH_ENV) else {
            return;
        };
        CatRecordingValidationStore::new(PathBuf::from(path))
            .register_candidate(&artifact("artifact-child-process"))
            .expect("child process append");
    }

    #[test]
    fn validation_store_p2_child_process_rotation_helper() {
        let Some(path) = env::var_os(VALIDATION_STORE_ROTATION_CHILD_PATH_ENV) else {
            return;
        };
        let threshold = env::var(VALIDATION_STORE_ROTATION_CHILD_THRESHOLD_ENV)
            .expect("rotation child threshold")
            .parse::<u64>()
            .expect("numeric rotation child threshold");
        CatRecordingValidationStore::new_with_compaction_threshold_for_test(
            PathBuf::from(path),
            threshold,
        )
        .register_candidate(&artifact("artifact-rotation-child"))
        .expect("child process rotation append");
    }

    #[test]
    fn validation_store_p2_parent_and_child_rotation_preserve_both_commits() {
        let path = temp_store("cat-validation-p2-process-rotation")
            .path()
            .to_path_buf();
        let bootstrap = CatRecordingValidationStore::new(path.clone());
        bootstrap
            .register_candidate(&artifact("artifact-rotation-base"))
            .expect("parent rotation bootstrap");
        let threshold = fs::metadata(&path)
            .expect("parent rotation metadata")
            .len()
            .saturating_mul(2);
        drop(bootstrap);
        let parent_store = CatRecordingValidationStore::new_with_compaction_threshold_for_test(
            path.clone(),
            threshold,
        );
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(parent_store.lock_path_for_test())
            .expect("rotation process lock");
        lock_file
            .lock_exclusive()
            .expect("hold rotation process lock");
        let mut child = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "runtime::cat_recording_validation::tests::validation_store_p2_child_process_rotation_helper",
                "--nocapture",
            ])
            .env(VALIDATION_STORE_ROTATION_CHILD_PATH_ENV, &path)
            .env(
                VALIDATION_STORE_ROTATION_CHILD_THRESHOLD_ENV,
                threshold.to_string(),
            )
            .spawn()
            .expect("spawn rotation child");
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(child.try_wait().expect("rotation child status").is_none());
        fs2::FileExt::unlock(&lock_file).expect("release rotation process lock");
        parent_store
            .register_candidate(&artifact("artifact-rotation-parent"))
            .expect("parent rotation append");
        assert!(child.wait().expect("rotation child exit").success());

        let archives = validation_archive_paths(&path);
        assert!(!archives.is_empty(), "serialized writers must rotate");
        let unique_archives = archives.iter().collect::<HashSet<_>>();
        assert_eq!(unique_archives.len(), archives.len());
        let latest = CatRecordingValidationStore::new(path)
            .list_latest()
            .expect("restart after process rotation");
        let artifact_ids = latest
            .iter()
            .map(|record| record.artifact_id.as_str())
            .collect::<HashSet<_>>();
        assert!(artifact_ids.contains("artifact-rotation-base"));
        assert!(artifact_ids.contains("artifact-rotation-parent"));
        assert!(artifact_ids.contains("artifact-rotation-child"));
    }

    #[test]
    fn validation_store_p2_replaced_lock_inode_cannot_create_a_second_lock_domain() {
        let store = temp_store("cat-validation-p2-lock-identity");
        store
            .register_candidate(&artifact("artifact-lock-baseline"))
            .expect("lock identity baseline");
        let main_before = fs::read(store.path()).expect("main before lock replacement");
        #[cfg(windows)]
        let mut old_lock = Some(store.secure_path.open_lock().expect("bound old lock"));
        #[cfg(not(windows))]
        let old_lock = Some(store.secure_path.open_lock().expect("bound old lock"));
        old_lock
            .as_ref()
            .expect("old lock handle")
            .lock_exclusive()
            .expect("hold old lock inode");
        let lock_path = store.lock_path_for_test();
        let replacement = atomicwrites::AtomicFile::new(&lock_path, atomicwrites::AllowOverwrite)
            .write(|file| {
                file.write_all(b"replacement-lock\n")
                    .and_then(|_| file.sync_all())
            });
        if let Err(error) = replacement {
            #[cfg(not(windows))]
            panic!("lock replacement must be supported on Unix: {error}");
            #[cfg(windows)]
            {
                let io_error = std::io::Error::from(error);
                assert!(matches!(io_error.raw_os_error(), Some(5 | 32)));
                fs2::FileExt::unlock(old_lock.as_ref().expect("old lock handle"))
                    .expect("release old lock inode");
                drop(old_lock.take());
                let released_replacement =
                    atomicwrites::AtomicFile::new(&lock_path, atomicwrites::AllowOverwrite).write(
                        |file| {
                            file.write_all(b"replacement-lock\n")
                                .and_then(|_| file.sync_all())
                        },
                    );
                if let Err(error) = released_replacement {
                    let io_error = std::io::Error::from(error);
                    assert!(matches!(io_error.raw_os_error(), Some(5 | 32)));
                    assert_eq!(
                        fs::read(store.path()).expect("main while lock replacement blocked"),
                        main_before
                    );
                    return;
                }
            }
        }

        let error = store
            .register_candidate(&artifact("artifact-lock-split-brain"))
            .expect_err("replacement lock inode must fail closed");

        assert!(error.contains("lock identity was replaced"), "{error}");
        assert_eq!(
            fs::read(store.path()).expect("main after lock replacement"),
            main_before
        );
        if let Some(old_lock) = old_lock {
            fs2::FileExt::unlock(&old_lock).expect("release old lock inode");
        }
    }

    #[test]
    fn validation_store_p2_thread_and_child_process_updates_are_serialized() {
        let path = temp_store("cat-validation-p2-process-lock")
            .path()
            .to_path_buf();
        let parent_store = CatRecordingValidationStore::new(path.clone());
        parent_store
            .register_candidate(&artifact("artifact-parent-process"))
            .expect("parent append");

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut threads = Vec::new();
        for artifact_id in ["artifact-thread-a", "artifact-thread-b"] {
            let thread_barrier = barrier.clone();
            let thread_store = CatRecordingValidationStore::new(path.clone());
            threads.push(std::thread::spawn(move || {
                thread_barrier.wait();
                thread_store
                    .register_candidate(&artifact(artifact_id))
                    .expect("thread append");
            }));
        }
        barrier.wait();
        for thread in threads {
            thread.join().expect("validation store thread");
        }

        let lock_path = parent_store.lock_path_for_test();
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .expect("validation store lock file");
        lock_file
            .lock_exclusive()
            .expect("hold parent process lock");
        let mut child = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "runtime::cat_recording_validation::tests::validation_store_p2_child_process_append_helper",
                "--nocapture",
            ])
            .env(VALIDATION_STORE_CHILD_PATH_ENV, &path)
            .spawn()
            .expect("spawn validation store child");
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(child.try_wait().expect("child status").is_none());
        fs2::FileExt::unlock(&lock_file).expect("release parent process lock");
        assert!(child.wait().expect("child exit").success());

        drop(parent_store);
        let latest = CatRecordingValidationStore::new(path)
            .list_latest()
            .expect("serialized process records");
        let artifact_ids = latest
            .into_iter()
            .map(|record| record.artifact_id)
            .collect::<HashSet<_>>();
        assert_eq!(
            artifact_ids,
            HashSet::from([
                "artifact-parent-process".to_string(),
                "artifact-thread-a".to_string(),
                "artifact-thread-b".to_string(),
                "artifact-child-process".to_string(),
            ])
        );
    }

    #[test]
    fn validation_store_p2_restart_reads_each_artifacts_latest_record() {
        let path = temp_store("cat-validation-p2-restart-latest")
            .path()
            .to_path_buf();
        let store = CatRecordingValidationStore::new(path.clone());
        store
            .register_candidate(&artifact("artifact-restart-a"))
            .expect("register artifact a");
        store
            .register_candidate(&artifact("artifact-restart-b"))
            .expect("register artifact b");
        let claim = claim_test_record(&store, "artifact-restart-a");
        store
            .complete(
                "artifact-restart-a",
                claim.claim_token.as_deref().expect("claim token"),
                CatRecordingValidationStatus::Accepted,
                None,
                None,
            )
            .expect("complete artifact a");
        drop(store);

        let restarted = CatRecordingValidationStore::new(path);
        let latest = restarted
            .list_latest()
            .expect("restart latest records")
            .into_iter()
            .map(|record| (record.artifact_id.clone(), record))
            .collect::<HashMap<_, _>>();
        assert_eq!(latest.len(), 2);
        assert_eq!(
            latest["artifact-restart-a"].validation_status,
            CatRecordingValidationStatus::Accepted
        );
        assert_eq!(latest["artifact-restart-a"].attempt_count, 1);
        assert_eq!(
            latest["artifact-restart-b"].validation_status,
            CatRecordingValidationStatus::PendingValidation
        );
    }

    #[test]
    fn validation_store_p2_queries_do_not_rescan_until_external_generation_changes() {
        let path_a = temp_store("cat-validation-p2-query-index-a")
            .path()
            .to_path_buf();
        let path_b = temp_store("cat-validation-p2-query-index-b")
            .path()
            .to_path_buf();
        let store_a = CatRecordingValidationStore::new(path_a.clone());
        let store_b = CatRecordingValidationStore::new(path_b);
        store_a
            .register_candidate(&artifact("artifact-index-a"))
            .expect("register indexed artifact");
        store_b
            .register_candidate(&artifact("artifact-index-b"))
            .expect("register isolated artifact");
        let baseline_scans = store_a.disk_scan_count_for_test();
        let artifact_ids = HashSet::from(["artifact-index-a".to_string()]);

        for _ in 0..20 {
            assert_eq!(store_a.list_latest().expect("latest query").len(), 1);
            assert!(store_a.next_pending().expect("pending query").is_some());
            assert!(store_a
                .pending_discards(CatRecordingValidationMode::Enforce)
                .expect("discard query")
                .is_empty());
            assert_eq!(
                store_a
                    .records_for_artifacts(&artifact_ids)
                    .expect("artifact query")
                    .len(),
                1
            );
        }
        assert_eq!(store_a.disk_scan_count_for_test(), baseline_scans);
        assert_eq!(store_b.list_latest().expect("isolated store").len(), 1);

        let mut externally_updated = store_a
            .list_latest()
            .expect("external source record")
            .remove(0);
        externally_updated.validation_status = CatRecordingValidationStatus::ReviewRequired;
        externally_updated.updated_at_epoch_ms = externally_updated.updated_at_epoch_ms + 1;
        let mut external_file = OpenOptions::new()
            .append(true)
            .open(&path_a)
            .expect("external append");
        serde_json::to_writer(&mut external_file, &externally_updated)
            .expect("external record serialization");
        external_file.write_all(b"\n").expect("external record LF");
        external_file.sync_data().expect("external append sync");

        assert_eq!(
            store_a.list_latest().expect("refreshed latest")[0].validation_status,
            CatRecordingValidationStatus::ReviewRequired
        );
        assert_eq!(store_a.disk_scan_count_for_test(), baseline_scans + 1);
        let _ = store_a.next_pending().expect("post-refresh pending query");
        assert_eq!(store_a.disk_scan_count_for_test(), baseline_scans + 1);
    }
}
