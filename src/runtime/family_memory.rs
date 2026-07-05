//! Metadata-only family memory review overlays for local vision events.

use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::runtime::family_timeline::{
    family_timeline_event, FamilyTimelineEvent, FamilyTimelineQueryOptions,
};
use crate::runtime::vision_event::{
    now_epoch_ms_string, reject_local_path_value, reject_sensitive_value, StoredLocalVisionEvent,
};

pub const FAMILY_MEMORY_OVERLAY_PATH_ENV: &str = "HARBOR_FAMILY_MEMORY_OVERLAY_PATH";
pub const DEFAULT_FAMILY_MEMORY_FEEDBACK_LIMIT: usize = 5_000;
const FAMILY_MEMORY_COMPACT_AFTER_RECORDS: usize = DEFAULT_FAMILY_MEMORY_FEEDBACK_LIMIT + 500;
const MAX_FAMILY_MEMORY_LINE_BYTES: usize = 32 * 1024;
const MAX_CORRECTED_SUMMARY_CHARS: usize = 500;
const MAX_CORRECTED_LABELS: usize = 16;
const MAX_CORRECTED_LABEL_CHARS: usize = 64;
const MAX_FEEDBACK_NOTE_CHARS: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FamilyMemoryFeedbackAction {
    ConfirmUseful,
    Favorite,
    Hide,
    Restore,
    CorrectSummary,
    CorrectLabels,
}

impl FamilyMemoryFeedbackAction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ConfirmUseful => "confirm_useful",
            Self::Favorite => "favorite",
            Self::Hide => "hide",
            Self::Restore => "restore",
            Self::CorrectSummary => "correct_summary",
            Self::CorrectLabels => "correct_labels",
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
pub struct FamilyMemoryFeedbackRequest {
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrected_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrected_labels: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FamilyMemoryFeedbackRecord {
    pub feedback_id: String,
    pub event_id: String,
    pub action: FamilyMemoryFeedbackAction,
    pub created_at: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrected_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrected_labels: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub metadata_only: bool,
    pub secret_scan: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct FamilyMemoryOverlay {
    pub event_id: String,
    pub confirmed_useful: bool,
    pub favorite: bool,
    pub hidden: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrected_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrected_labels: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corrected_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    pub feedback_count: usize,
}

impl FamilyMemoryOverlay {
    pub fn corrected(&self) -> bool {
        self.corrected_summary.is_some() || self.corrected_labels.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FamilyMemoryStats {
    pub generated_at: String,
    pub total_feedback_records: usize,
    pub event_count: usize,
    pub confirmed_count: usize,
    pub favorite_count: usize,
    pub hidden_count: usize,
    pub corrected_count: usize,
    pub bounded_limit: usize,
    pub metadata_only: bool,
    pub secret_scan: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FamilyMemoryEventView {
    #[serde(flatten)]
    pub event: FamilyTimelineEvent,
    pub overlay: FamilyMemoryOverlay,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FamilyMemoryEventFilter {
    pub include_hidden: bool,
    pub favorites_only: bool,
    pub hidden_only: bool,
    pub corrected_only: bool,
    pub limit: usize,
}

impl Default for FamilyMemoryEventFilter {
    fn default() -> Self {
        Self {
            include_hidden: false,
            favorites_only: false,
            hidden_only: false,
            corrected_only: false,
            limit: 50,
        }
    }
}

pub fn default_family_memory_overlay_path() -> PathBuf {
    std::env::var(FAMILY_MEMORY_OVERLAY_PATH_ENV)
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".harborbeacon/family-memory/feedback.jsonl"))
}

pub fn append_family_memory_feedback_default(
    event_id: &str,
    request: FamilyMemoryFeedbackRequest,
    source: &str,
) -> Result<FamilyMemoryFeedbackRecord, String> {
    append_family_memory_feedback(
        &default_family_memory_overlay_path(),
        event_id,
        request,
        source,
    )
}

pub fn append_family_memory_feedback(
    store_path: &Path,
    event_id: &str,
    request: FamilyMemoryFeedbackRequest,
    source: &str,
) -> Result<FamilyMemoryFeedbackRecord, String> {
    let record = sanitize_family_memory_feedback(event_id, request, source)?;
    if let Some(parent) = store_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create family memory overlay directory {}: {error}",
                parent.display()
            )
        })?;
    }
    let line = serde_json::to_string(&record)
        .map_err(|error| format!("failed to serialize family memory feedback: {error}"))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(store_path)
        .map_err(|error| {
            format!(
                "failed to open family memory overlay {}: {error}",
                store_path.display()
            )
        })?;
    writeln!(file, "{line}")
        .map_err(|error| format!("failed to append family memory feedback: {error}"))?;
    compact_family_memory_feedback_if_needed(store_path)?;
    Ok(record)
}

pub fn list_family_memory_feedback_default(
    limit: usize,
) -> Result<Vec<FamilyMemoryFeedbackRecord>, String> {
    list_family_memory_feedback(&default_family_memory_overlay_path(), limit)
}

pub fn list_family_memory_feedback(
    store_path: &Path,
    limit: usize,
) -> Result<Vec<FamilyMemoryFeedbackRecord>, String> {
    if !store_path.exists() {
        return Ok(Vec::new());
    }
    let target_limit = limit.max(1).min(DEFAULT_FAMILY_MEMORY_FEEDBACK_LIMIT);
    let file = fs::File::open(store_path).map_err(|error| {
        format!(
            "failed to open family memory overlay {}: {error}",
            store_path.display()
        )
    })?;
    let mut records: VecDeque<FamilyMemoryFeedbackRecord> = VecDeque::with_capacity(target_limit);
    for line in BufReader::new(file).lines() {
        let line =
            line.map_err(|error| format!("failed to read family memory overlay: {error}"))?;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.len() > MAX_FAMILY_MEMORY_LINE_BYTES {
            continue;
        }
        let Ok(record) = serde_json::from_str::<FamilyMemoryFeedbackRecord>(trimmed) else {
            continue;
        };
        if records.len() == target_limit {
            records.pop_front();
        }
        records.push_back(record);
    }
    Ok(records.into_iter().collect())
}

pub fn family_memory_overlays_default() -> Result<BTreeMap<String, FamilyMemoryOverlay>, String> {
    let records = list_family_memory_feedback_default(DEFAULT_FAMILY_MEMORY_FEEDBACK_LIMIT)?;
    Ok(build_family_memory_overlays(&records))
}

pub fn build_family_memory_overlays(
    records: &[FamilyMemoryFeedbackRecord],
) -> BTreeMap<String, FamilyMemoryOverlay> {
    let mut overlays: BTreeMap<String, FamilyMemoryOverlay> = BTreeMap::new();
    for record in records {
        let overlay =
            overlays
                .entry(record.event_id.clone())
                .or_insert_with(|| FamilyMemoryOverlay {
                    event_id: record.event_id.clone(),
                    ..FamilyMemoryOverlay::default()
                });
        overlay.feedback_count = overlay.feedback_count.saturating_add(1);
        overlay.updated_at = Some(record.created_at.clone());
        match record.action {
            FamilyMemoryFeedbackAction::ConfirmUseful => overlay.confirmed_useful = true,
            FamilyMemoryFeedbackAction::Favorite => overlay.favorite = true,
            FamilyMemoryFeedbackAction::Hide => overlay.hidden = true,
            FamilyMemoryFeedbackAction::Restore => overlay.hidden = false,
            FamilyMemoryFeedbackAction::CorrectSummary => {
                if let Some(summary) = record.corrected_summary.as_ref() {
                    overlay.corrected_summary = Some(summary.clone());
                    overlay.corrected_at = Some(record.created_at.clone());
                }
            }
            FamilyMemoryFeedbackAction::CorrectLabels => {
                if let Some(labels) = record.corrected_labels.as_ref() {
                    overlay.corrected_labels = Some(labels.clone());
                    overlay.corrected_at = Some(record.created_at.clone());
                }
            }
        }
    }
    overlays
}

pub fn build_family_memory_stats(records: &[FamilyMemoryFeedbackRecord]) -> FamilyMemoryStats {
    let overlays = build_family_memory_overlays(records);
    build_family_memory_stats_from_overlays(records.len(), &overlays)
}

pub fn build_family_memory_stats_from_overlays(
    total_feedback_records: usize,
    overlays: &BTreeMap<String, FamilyMemoryOverlay>,
) -> FamilyMemoryStats {
    FamilyMemoryStats {
        generated_at: now_epoch_ms_string(),
        total_feedback_records,
        event_count: overlays.len(),
        confirmed_count: overlays
            .values()
            .filter(|overlay| overlay.confirmed_useful)
            .count(),
        favorite_count: overlays.values().filter(|overlay| overlay.favorite).count(),
        hidden_count: overlays.values().filter(|overlay| overlay.hidden).count(),
        corrected_count: overlays
            .values()
            .filter(|overlay| overlay.corrected())
            .count(),
        bounded_limit: DEFAULT_FAMILY_MEMORY_FEEDBACK_LIMIT,
        metadata_only: true,
        secret_scan: "clean".to_string(),
    }
}

pub fn family_memory_stats_default() -> Result<FamilyMemoryStats, String> {
    let records = list_family_memory_feedback_default(DEFAULT_FAMILY_MEMORY_FEEDBACK_LIMIT)?;
    Ok(build_family_memory_stats(&records))
}

pub fn apply_family_memory_overlay_to_event(
    stored: &StoredLocalVisionEvent,
    overlay: Option<&FamilyMemoryOverlay>,
) -> StoredLocalVisionEvent {
    let mut applied = stored.clone();
    if let Some(overlay) = overlay {
        if let Some(summary) = overlay.corrected_summary.as_ref() {
            applied.event.summary = summary.clone();
            if let Some(vlm) = applied.event.vlm.as_mut() {
                vlm.summary = summary.clone();
                vlm.derived_text = summary.clone();
            }
        }
        if let Some(labels) = overlay.corrected_labels.as_ref() {
            applied.event.labels = labels.clone();
        }
    }
    applied
}

pub fn apply_family_memory_overlays_to_events(
    events: &[StoredLocalVisionEvent],
    overlays: &BTreeMap<String, FamilyMemoryOverlay>,
    include_hidden: bool,
) -> Vec<StoredLocalVisionEvent> {
    events
        .iter()
        .filter_map(|stored| {
            let overlay = overlays.get(&stored.event.event_id);
            if !include_hidden && overlay.is_some_and(|overlay| overlay.hidden) {
                return None;
            }
            Some(apply_family_memory_overlay_to_event(stored, overlay))
        })
        .collect()
}

pub fn build_family_memory_event_views(
    events: &[StoredLocalVisionEvent],
    overlays: &BTreeMap<String, FamilyMemoryOverlay>,
    filter: &FamilyMemoryEventFilter,
    timeline_options: &FamilyTimelineQueryOptions,
) -> Result<Vec<FamilyMemoryEventView>, String> {
    let mut views = events
        .iter()
        .filter_map(|stored| {
            let overlay = overlays
                .get(&stored.event.event_id)
                .cloned()
                .unwrap_or_else(|| FamilyMemoryOverlay {
                    event_id: stored.event.event_id.clone(),
                    ..FamilyMemoryOverlay::default()
                });
            if filter.hidden_only && !overlay.hidden {
                return None;
            }
            if !filter.include_hidden && overlay.hidden {
                return None;
            }
            if filter.favorites_only && !overlay.favorite {
                return None;
            }
            if filter.corrected_only && !overlay.corrected() {
                return None;
            }
            let applied = apply_family_memory_overlay_to_event(stored, Some(&overlay));
            let event = family_timeline_event(&applied);
            if !family_timeline_event_matches_query(&event, timeline_options) {
                return None;
            }
            Some(FamilyMemoryEventView { event, overlay })
        })
        .collect::<Vec<_>>();
    views.truncate(filter.limit.clamp(1, 100));
    let payload = serde_json::to_value(&views)
        .map_err(|error| format!("failed to inspect memory events: {error}"))?;
    reject_sensitive_value(&payload)?;
    reject_local_path_value(&payload)?;
    Ok(views)
}

pub fn compact_family_memory_feedback_evidence(record: &FamilyMemoryFeedbackRecord) -> Value {
    json!({
        "feedback_id": record.feedback_id,
        "event_id": record.event_id,
        "action": record.action.as_str(),
        "has_corrected_summary": record.corrected_summary.is_some(),
        "corrected_label_count": record.corrected_labels.as_ref().map(|labels| labels.len()).unwrap_or(0),
        "source": record.source,
        "created_at": record.created_at,
        "metadata_only": true,
        "secret_scan": "clean",
    })
}

fn sanitize_family_memory_feedback(
    event_id: &str,
    request: FamilyMemoryFeedbackRequest,
    source: &str,
) -> Result<FamilyMemoryFeedbackRecord, String> {
    let event_id = event_id.trim();
    if event_id.is_empty() || event_id.len() > 128 {
        return Err("family memory feedback requires a valid event_id".to_string());
    }
    let source = source.trim();
    let action = parse_family_memory_feedback_action(&request.action)?;
    let corrected_summary = match action {
        FamilyMemoryFeedbackAction::CorrectSummary => Some(sanitize_corrected_summary(
            request
                .corrected_summary
                .as_deref()
                .or(request.note.as_deref())
                .unwrap_or_default(),
        )?),
        _ => request
            .corrected_summary
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .map(sanitize_corrected_summary)
            .transpose()?,
    };
    let corrected_labels = match action {
        FamilyMemoryFeedbackAction::CorrectLabels => Some(sanitize_corrected_labels(
            request.corrected_labels.unwrap_or_default(),
        )?),
        _ => request
            .corrected_labels
            .map(sanitize_corrected_labels)
            .transpose()?,
    };
    let note = request
        .note
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .map(sanitize_feedback_note)
        .transpose()?;
    let record = FamilyMemoryFeedbackRecord {
        feedback_id: format!("fmfb_{}", Uuid::new_v4().simple()),
        event_id: event_id.to_string(),
        action,
        created_at: now_epoch_ms_string(),
        source: if source.is_empty() {
            "unknown".to_string()
        } else {
            source.chars().take(64).collect()
        },
        corrected_summary,
        corrected_labels,
        note,
        metadata_only: true,
        secret_scan: "clean".to_string(),
    };
    let payload = serde_json::to_value(&record)
        .map_err(|error| format!("failed to inspect family memory feedback: {error}"))?;
    reject_sensitive_value(&payload)?;
    reject_local_path_value(&payload)?;
    Ok(record)
}

fn parse_family_memory_feedback_action(value: &str) -> Result<FamilyMemoryFeedbackAction, String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "confirm_useful" | "confirm" | "useful" => Ok(FamilyMemoryFeedbackAction::ConfirmUseful),
        "favorite" | "favourite" => Ok(FamilyMemoryFeedbackAction::Favorite),
        "hide" => Ok(FamilyMemoryFeedbackAction::Hide),
        "restore" | "unhide" => Ok(FamilyMemoryFeedbackAction::Restore),
        "correct_summary" | "summary" => Ok(FamilyMemoryFeedbackAction::CorrectSummary),
        "correct_labels" | "labels" => Ok(FamilyMemoryFeedbackAction::CorrectLabels),
        _ => Err("unsupported family memory feedback action".to_string()),
    }
}

fn sanitize_corrected_summary(value: &str) -> Result<String, String> {
    let summary = value.trim();
    if summary.is_empty() {
        return Err("corrected summary cannot be empty".to_string());
    }
    let clipped = summary
        .chars()
        .take(MAX_CORRECTED_SUMMARY_CHARS)
        .collect::<String>();
    let payload = json!({ "corrected_summary": clipped });
    reject_sensitive_value(&payload)?;
    reject_local_path_value(&payload)?;
    Ok(clipped)
}

fn sanitize_corrected_labels(values: Vec<String>) -> Result<Vec<String>, String> {
    let mut labels = Vec::new();
    for value in values.into_iter().take(MAX_CORRECTED_LABELS) {
        let label = value
            .trim()
            .to_ascii_lowercase()
            .chars()
            .take(MAX_CORRECTED_LABEL_CHARS)
            .collect::<String>();
        if label.is_empty() || labels.iter().any(|existing| existing == &label) {
            continue;
        }
        let payload = json!({ "label": label });
        reject_sensitive_value(&payload)?;
        reject_local_path_value(&payload)?;
        labels.push(label);
    }
    if labels.is_empty() {
        return Err("corrected labels cannot be empty".to_string());
    }
    Ok(labels)
}

fn sanitize_feedback_note(value: &str) -> Result<String, String> {
    let note = value
        .trim()
        .chars()
        .take(MAX_FEEDBACK_NOTE_CHARS)
        .collect::<String>();
    let payload = json!({ "note": note });
    reject_sensitive_value(&payload)?;
    reject_local_path_value(&payload)?;
    Ok(note)
}

fn compact_family_memory_feedback_if_needed(store_path: &Path) -> Result<(), String> {
    let records = list_family_memory_feedback(store_path, FAMILY_MEMORY_COMPACT_AFTER_RECORDS)?;
    if records.len() < FAMILY_MEMORY_COMPACT_AFTER_RECORDS {
        return Ok(());
    }
    let keep = records
        .into_iter()
        .rev()
        .take(DEFAULT_FAMILY_MEMORY_FEEDBACK_LIMIT)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();
    let mut file = fs::File::create(store_path).map_err(|error| {
        format!(
            "failed to compact family memory overlay {}: {error}",
            store_path.display()
        )
    })?;
    for record in keep {
        let line = serde_json::to_string(&record)
            .map_err(|error| format!("failed to serialize compacted feedback: {error}"))?;
        writeln!(file, "{line}")
            .map_err(|error| format!("failed to write compacted feedback: {error}"))?;
    }
    Ok(())
}

fn family_timeline_event_matches_query(
    event: &FamilyTimelineEvent,
    options: &FamilyTimelineQueryOptions,
) -> bool {
    if let Some(camera_filter) = options.camera_filter.as_ref() {
        if event.camera_id.to_ascii_lowercase() != camera_filter.to_ascii_lowercase() {
            return false;
        }
    }
    if let Some(label_filter) = options.label_filter.as_ref() {
        let normalized = label_filter.to_ascii_lowercase();
        let label_match = event
            .labels
            .iter()
            .any(|label| label.to_ascii_lowercase() == normalized);
        if !label_match && event.event_type.to_ascii_lowercase() != normalized {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::vision_event::{
        LocalVisionEvent, LocalVisionEventVlmSummary, SnapshotArtifact,
    };

    fn stored_event(event_id: &str) -> StoredLocalVisionEvent {
        StoredLocalVisionEvent {
            received_at: "epoch_ms:10".to_string(),
            event: LocalVisionEvent {
                event_id: event_id.to_string(),
                camera_id: "front".to_string(),
                event_type: "person_detected".to_string(),
                confidence: 0.8,
                labels: vec!["person".to_string()],
                summary: "person detected".to_string(),
                snapshot_artifact: SnapshotArtifact {
                    artifact_id: Some("artifact_1".to_string()),
                    path: None,
                    mime_type: Some("image/jpeg".to_string()),
                    byte_size: Some(42),
                    sha256: Some("abc".to_string()),
                    source: Some("local_snapshot".to_string()),
                },
                started_at: "epoch_ms:10".to_string(),
                analyzer: "test".to_string(),
                latency_ms: 12,
                metrics: json!({}),
                vlm: None,
            },
            audit_record: json!({"metadata_only": true}),
            ha_mqtt_payload: json!({"metadata_only": true}),
        }
    }

    fn temp_overlay_path() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "harbor-family-memory-test-{}",
            Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir.join("feedback.jsonl")
    }

    #[test]
    fn overlay_append_read_bounded_and_applies_corrections() {
        let store = temp_overlay_path();
        append_family_memory_feedback(
            &store,
            "event_1",
            FamilyMemoryFeedbackRequest {
                action: "favorite".to_string(),
                ..FamilyMemoryFeedbackRequest::default()
            },
            "test",
        )
        .expect("favorite");
        append_family_memory_feedback(
            &store,
            "event_1",
            FamilyMemoryFeedbackRequest {
                action: "correct_summary".to_string(),
                corrected_summary: Some("快递到了门口".to_string()),
                ..FamilyMemoryFeedbackRequest::default()
            },
            "test",
        )
        .expect("summary");
        let records = list_family_memory_feedback(&store, 1).expect("records");
        assert_eq!(records.len(), 1);
        let overlays =
            build_family_memory_overlays(&list_family_memory_feedback(&store, 10).unwrap());
        let overlay = overlays.get("event_1").expect("overlay");
        assert!(overlay.favorite);
        assert_eq!(overlay.corrected_summary.as_deref(), Some("快递到了门口"));
        let applied = apply_family_memory_overlay_to_event(&stored_event("event_1"), Some(overlay));
        assert_eq!(applied.event.summary, "快递到了门口");
    }

    #[test]
    fn corrected_summary_overrides_active_vlm_timeline_text() {
        let mut stored = stored_event("event_1");
        stored.event.vlm = Some(LocalVisionEventVlmSummary {
            status: "active".to_string(),
            summary: "vlm original summary".to_string(),
            derived_text: "vlm original derived".to_string(),
            ..LocalVisionEventVlmSummary::default()
        });
        let mut overlay = FamilyMemoryOverlay {
            event_id: "event_1".to_string(),
            corrected_summary: Some("manual corrected summary".to_string()),
            ..FamilyMemoryOverlay::default()
        };
        overlay.updated_at = Some("epoch_ms:20".to_string());
        let applied = apply_family_memory_overlay_to_event(&stored, Some(&overlay));
        assert_eq!(applied.event.summary, "manual corrected summary");
        let vlm = applied.event.vlm.as_ref().expect("vlm clone");
        assert_eq!(vlm.summary, "manual corrected summary");
        assert_eq!(vlm.derived_text, "manual corrected summary");
        let view = family_timeline_event(&applied);
        assert_eq!(view.summary, "manual corrected summary");
    }

    #[test]
    fn hidden_event_is_filtered_by_default_and_restored() {
        let records = vec![
            FamilyMemoryFeedbackRecord {
                feedback_id: "a".to_string(),
                event_id: "event_1".to_string(),
                action: FamilyMemoryFeedbackAction::Hide,
                created_at: "epoch_ms:1".to_string(),
                source: "test".to_string(),
                corrected_summary: None,
                corrected_labels: None,
                note: None,
                metadata_only: true,
                secret_scan: "clean".to_string(),
            },
            FamilyMemoryFeedbackRecord {
                feedback_id: "b".to_string(),
                event_id: "event_1".to_string(),
                action: FamilyMemoryFeedbackAction::Restore,
                created_at: "epoch_ms:2".to_string(),
                source: "test".to_string(),
                corrected_summary: None,
                corrected_labels: None,
                note: None,
                metadata_only: true,
                secret_scan: "clean".to_string(),
            },
        ];
        let overlays = build_family_memory_overlays(&records);
        let events =
            apply_family_memory_overlays_to_events(&[stored_event("event_1")], &overlays, false);
        assert_eq!(events.len(), 1);
        assert_eq!(build_family_memory_stats(&records).hidden_count, 0);
    }

    #[test]
    fn feedback_rejects_secrets_and_paths() {
        let store = temp_overlay_path();
        let error = append_family_memory_feedback(
            &store,
            "event_1",
            FamilyMemoryFeedbackRequest {
                action: "correct_summary".to_string(),
                corrected_summary: Some("see rtsp://user:pass@example/stream".to_string()),
                ..FamilyMemoryFeedbackRequest::default()
            },
            "test",
        )
        .expect_err("secret rejected");
        assert!(error.contains("sensitive"));
    }
}
