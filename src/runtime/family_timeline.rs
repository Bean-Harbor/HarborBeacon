//! Metadata-only family timeline projections derived from local vision events.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::runtime::vision_event::{
    local_vision_automation_summary, local_vision_vlm_status, now_epoch_ms, now_epoch_ms_string,
    parse_epoch_ms, reject_local_path_value, reject_sensitive_value, StoredLocalVisionEvent,
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FamilyTimelineArtifactMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_size: Option<u64>,
    pub sha256_present: bool,
    pub local_path_redacted: bool,
    pub raw_image_included: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FamilyTimelineEvent {
    pub event_id: String,
    pub camera_id: String,
    pub event_type: String,
    pub summary: String,
    pub confidence: f32,
    pub labels: Vec<String>,
    pub started_at: String,
    pub received_at: String,
    pub latency_ms: u64,
    pub vlm_status: String,
    pub artifact: FamilyTimelineArtifactMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FamilyTimelineBucket {
    pub bucket_id: String,
    pub camera_id: String,
    pub started_at: String,
    pub ended_at: String,
    pub event_count: usize,
    pub event_types: Vec<String>,
    pub top_labels: Vec<String>,
    pub event_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FamilyTimelineResponse {
    pub generated_at: String,
    pub window_seconds: u64,
    pub event_count: usize,
    pub metadata_only: bool,
    pub buckets: Vec<FamilyTimelineBucket>,
    pub events: Vec<FamilyTimelineEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FamilyTimelineDigest {
    pub generated_at: String,
    pub status: String,
    pub window_seconds: u64,
    pub event_count: usize,
    pub headline: String,
    pub bullets: Vec<String>,
    pub top_labels: Vec<String>,
    pub cameras: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_event_id: Option<String>,
    pub metadata_only: bool,
    pub secret_scan: String,
    pub vlm_coverage: FamilyTimelineVlmCoverage,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct FamilyTimelineVlmCoverage {
    pub total: usize,
    pub active: usize,
    pub degraded: usize,
    pub not_sampled: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FamilyTimelineQueryOptions {
    pub window_seconds: u64,
    pub camera_filter: Option<String>,
    pub label_filter: Option<String>,
    pub limit: usize,
}

impl Default for FamilyTimelineQueryOptions {
    fn default() -> Self {
        Self {
            window_seconds: 24 * 60 * 60,
            camera_filter: None,
            label_filter: None,
            limit: 50,
        }
    }
}

impl FamilyTimelineQueryOptions {
    pub fn new(
        window_seconds: u64,
        camera_filter: Option<String>,
        label_filter: Option<String>,
        limit: usize,
    ) -> Self {
        Self {
            window_seconds: window_seconds.clamp(60, 7 * 24 * 60 * 60),
            camera_filter: normalize_optional_filter(camera_filter),
            label_filter: normalize_optional_filter(label_filter),
            limit: limit.clamp(1, 100),
        }
    }
}

pub fn build_family_timeline(
    events: &[StoredLocalVisionEvent],
    window_seconds: u64,
    camera_filter: Option<&str>,
    label_filter: Option<&str>,
    limit: usize,
) -> Result<FamilyTimelineResponse, String> {
    let options = FamilyTimelineQueryOptions::new(
        window_seconds,
        camera_filter.map(ToString::to_string),
        label_filter.map(ToString::to_string),
        limit,
    );
    build_family_timeline_from_query(events, &options)
}

pub fn build_family_timeline_from_query(
    events: &[StoredLocalVisionEvent],
    options: &FamilyTimelineQueryOptions,
) -> Result<FamilyTimelineResponse, String> {
    let camera_filter = options
        .camera_filter
        .as_deref()
        .map(str::to_ascii_lowercase);
    let label_filter = options.label_filter.as_deref().map(str::to_ascii_lowercase);
    let window_seconds = options.window_seconds.max(1);
    let cutoff_ms = now_epoch_ms().saturating_sub(window_seconds.saturating_mul(1000));
    let mut timeline_events = events
        .iter()
        .filter(|stored| {
            let event = &stored.event;
            if let Some(camera_filter) = camera_filter.as_ref() {
                if event.camera_id.to_ascii_lowercase() != *camera_filter {
                    return false;
                }
            }
            if let Some(label_filter) = label_filter.as_ref() {
                let label_match = event
                    .labels
                    .iter()
                    .any(|label| label.to_ascii_lowercase() == *label_filter);
                if !label_match && event.event_type.to_ascii_lowercase() != *label_filter {
                    return false;
                }
            }
            parse_epoch_ms(&event.started_at)
                .or_else(|| parse_epoch_ms(&stored.received_at))
                .map(|timestamp| timestamp >= cutoff_ms)
                .unwrap_or(true)
        })
        .map(family_timeline_event)
        .collect::<Vec<_>>();
    timeline_events.sort_by(|left, right| {
        parse_epoch_ms(&right.started_at)
            .unwrap_or_default()
            .cmp(&parse_epoch_ms(&left.started_at).unwrap_or_default())
            .then_with(|| right.event_id.cmp(&left.event_id))
    });
    timeline_events.truncate(options.limit.clamp(1, 100));
    let buckets = build_family_timeline_buckets(&timeline_events);
    let response = FamilyTimelineResponse {
        generated_at: now_epoch_ms_string(),
        window_seconds,
        event_count: timeline_events.len(),
        metadata_only: true,
        buckets,
        events: timeline_events,
    };
    validate_family_timeline_response(&response)?;
    Ok(response)
}

pub fn build_family_timeline_digest(
    events: &[StoredLocalVisionEvent],
    window_seconds: u64,
) -> Result<FamilyTimelineDigest, String> {
    let options = FamilyTimelineQueryOptions::new(window_seconds, None, None, 50);
    build_family_timeline_digest_from_query(events, &options)
}

pub fn build_family_timeline_digest_from_query(
    events: &[StoredLocalVisionEvent],
    options: &FamilyTimelineQueryOptions,
) -> Result<FamilyTimelineDigest, String> {
    let timeline = build_family_timeline_from_query(events, options)?;
    let latest_event_id = timeline.events.first().map(|event| event.event_id.clone());
    let cameras = counted_strings(
        timeline
            .events
            .iter()
            .map(|event| event.camera_id.as_str())
            .collect::<Vec<_>>(),
        5,
    );
    let top_labels = counted_strings(
        timeline
            .events
            .iter()
            .flat_map(|event| event.labels.iter().map(String::as_str))
            .collect::<Vec<_>>(),
        8,
    );
    let status = if timeline.event_count == 0 {
        "quiet"
    } else {
        "available"
    }
    .to_string();
    let window_label = family_timeline_window_label(timeline.window_seconds);
    let headline = if timeline.event_count == 0 {
        format!("最近 {window_label} 没有可用的家庭视觉事件。")
    } else {
        format!(
            "最近 {window_label} 记录到 {} 条家庭视觉事件，覆盖 {} 个摄像头。",
            timeline.event_count,
            cameras.len().max(1)
        )
    };
    let mut bullets = Vec::new();
    for bucket in timeline.buckets.iter().take(5) {
        let label_text = if bucket.top_labels.is_empty() {
            "无主要标签".to_string()
        } else {
            bucket.top_labels.join(", ")
        };
        bullets.push(format!(
            "{}: {} 条事件，主要标签 {}。",
            bucket.camera_id, bucket.event_count, label_text
        ));
    }
    if bullets.is_empty() {
        bullets.push("暂无可汇总事件。".to_string());
    }
    let vlm_coverage = family_timeline_vlm_coverage(&timeline.events);
    if vlm_coverage.active > 0 || vlm_coverage.degraded > 0 {
        bullets.push(format!(
            "VLM 覆盖：{} 条 active，{} 条 degraded，{} 条未抽样。",
            vlm_coverage.active, vlm_coverage.degraded, vlm_coverage.not_sampled
        ));
    }
    let digest = FamilyTimelineDigest {
        generated_at: now_epoch_ms_string(),
        status,
        window_seconds: timeline.window_seconds,
        event_count: timeline.event_count,
        headline,
        bullets,
        top_labels,
        cameras,
        latest_event_id,
        metadata_only: true,
        secret_scan: "clean".to_string(),
        vlm_coverage,
    };
    validate_family_timeline_digest(&digest)?;
    Ok(digest)
}

pub fn validate_family_timeline_response(response: &FamilyTimelineResponse) -> Result<(), String> {
    let payload = serde_json::to_value(response)
        .map_err(|error| format!("failed to inspect family timeline: {error}"))?;
    reject_sensitive_value(&payload)?;
    reject_local_path_value(&payload)?;
    Ok(())
}

pub fn validate_family_timeline_digest(digest: &FamilyTimelineDigest) -> Result<(), String> {
    let payload = serde_json::to_value(digest)
        .map_err(|error| format!("failed to inspect family timeline digest: {error}"))?;
    reject_sensitive_value(&payload)?;
    reject_local_path_value(&payload)?;
    Ok(())
}

fn normalize_optional_filter(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn family_timeline_window_label(window_seconds: u64) -> String {
    if window_seconds % (24 * 60 * 60) == 0 {
        let days = window_seconds / (24 * 60 * 60);
        if days == 1 {
            "24 小时".to_string()
        } else {
            format!("{days} 天")
        }
    } else if window_seconds % (60 * 60) == 0 {
        format!("{} 小时", window_seconds / (60 * 60))
    } else if window_seconds % 60 == 0 {
        format!("{} 分钟", window_seconds / 60)
    } else {
        format!("{window_seconds} 秒")
    }
}

pub fn family_timeline_event(stored: &StoredLocalVisionEvent) -> FamilyTimelineEvent {
    let event = &stored.event;
    FamilyTimelineEvent {
        event_id: event.event_id.clone(),
        camera_id: event.camera_id.clone(),
        event_type: event.event_type.clone(),
        summary: local_vision_automation_summary(event),
        confidence: event.confidence,
        labels: event.labels.clone(),
        started_at: event.started_at.clone(),
        received_at: stored.received_at.clone(),
        latency_ms: event.latency_ms,
        vlm_status: local_vision_vlm_status(event),
        artifact: FamilyTimelineArtifactMetadata {
            artifact_id: event.snapshot_artifact.artifact_id.clone(),
            mime_type: event.snapshot_artifact.mime_type.clone(),
            byte_size: event.snapshot_artifact.byte_size,
            sha256_present: event.snapshot_artifact.sha256.is_some(),
            local_path_redacted: true,
            raw_image_included: false,
        },
    }
}

fn build_family_timeline_buckets(events: &[FamilyTimelineEvent]) -> Vec<FamilyTimelineBucket> {
    let mut grouped: BTreeMap<String, Vec<&FamilyTimelineEvent>> = BTreeMap::new();
    for event in events {
        grouped
            .entry(event.camera_id.clone())
            .or_default()
            .push(event);
    }
    let mut buckets = grouped
        .into_iter()
        .map(|(camera_id, mut events)| {
            events.sort_by(|left, right| {
                parse_epoch_ms(&left.started_at)
                    .unwrap_or_default()
                    .cmp(&parse_epoch_ms(&right.started_at).unwrap_or_default())
            });
            let started_at = events
                .first()
                .map(|event| event.started_at.clone())
                .unwrap_or_default();
            let ended_at = events
                .last()
                .map(|event| event.started_at.clone())
                .unwrap_or_else(|| started_at.clone());
            let event_ids = events
                .iter()
                .map(|event| event.event_id.clone())
                .collect::<Vec<_>>();
            let event_types = counted_strings(
                events
                    .iter()
                    .map(|event| event.event_type.as_str())
                    .collect::<Vec<_>>(),
                6,
            );
            let top_labels = counted_strings(
                events
                    .iter()
                    .flat_map(|event| event.labels.iter().map(String::as_str))
                    .collect::<Vec<_>>(),
                8,
            );
            FamilyTimelineBucket {
                bucket_id: format!("family_timeline_{camera_id}"),
                camera_id,
                started_at,
                ended_at,
                event_count: event_ids.len(),
                event_types,
                top_labels,
                event_ids,
            }
        })
        .collect::<Vec<_>>();
    buckets.sort_by(|left, right| {
        parse_epoch_ms(&right.ended_at)
            .unwrap_or_default()
            .cmp(&parse_epoch_ms(&left.ended_at).unwrap_or_default())
            .then_with(|| right.event_count.cmp(&left.event_count))
    });
    buckets
}

fn family_timeline_vlm_coverage(events: &[FamilyTimelineEvent]) -> FamilyTimelineVlmCoverage {
    let mut coverage = FamilyTimelineVlmCoverage {
        total: events.len(),
        ..FamilyTimelineVlmCoverage::default()
    };
    for event in events {
        match event.vlm_status.trim().to_ascii_lowercase().as_str() {
            "active" | "completed" | "ready" => {
                coverage.active = coverage.active.saturating_add(1);
            }
            "" | "not_sampled" | "not-sampled" | "none" | "not_available" | "unavailable" => {
                coverage.not_sampled = coverage.not_sampled.saturating_add(1);
            }
            _ => {
                coverage.degraded = coverage.degraded.saturating_add(1);
            }
        }
    }
    coverage
}

fn counted_strings(values: Vec<&str>, limit: usize) -> Vec<String> {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for value in values {
        let normalized = value.trim().to_ascii_lowercase();
        if !normalized.is_empty() {
            *counts.entry(normalized).or_default() += 1;
        }
    }
    let mut pairs = counts.into_iter().collect::<Vec<_>>();
    pairs.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    pairs
        .into_iter()
        .take(limit.max(1))
        .map(|(value, _)| value)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    use crate::runtime::vision_event::{LocalVisionEvent, SnapshotArtifact};

    #[test]
    fn family_timeline_digest_is_metadata_only_and_redacted() {
        let mut event = sample_event();
        event.started_at = now_epoch_ms_string();
        event.camera_id = "front-door".to_string();
        event.event_type = "person_detected".to_string();
        event.labels = vec!["person".to_string(), "door".to_string()];
        event.summary = "门口有人经过。".to_string();
        let stored = sample_stored_event(event);

        let timeline = build_family_timeline(&[stored.clone()], 24 * 60 * 60, None, None, 10)
            .expect("timeline");
        let digest =
            build_family_timeline_digest(&[stored], 24 * 60 * 60).expect("timeline digest");
        let text =
            serde_json::to_string(&(timeline.clone(), digest.clone())).expect("serialize timeline");

        assert_eq!(timeline.event_count, 1);
        assert_eq!(timeline.buckets.len(), 1);
        assert!(timeline.events[0].artifact.local_path_redacted);
        assert!(!timeline.events[0].artifact.raw_image_included);
        assert_eq!(digest.secret_scan, "clean");
        assert!(digest.headline.contains("1 条"));
        assert_eq!(digest.vlm_coverage.total, 1);
        assert_eq!(digest.vlm_coverage.not_sampled, 1);
        assert!(!text.contains("/tmp/source.jpg"));
        assert!(!text.contains("rtsp://"));
        assert!(!text.contains("data:image"));
    }

    #[test]
    fn family_timeline_digest_reports_vlm_coverage() {
        let mut active = sample_event();
        active.event_id = "event-active".to_string();
        active.vlm = Some(crate::runtime::vision_event::LocalVisionEventVlmSummary {
            status: "active".to_string(),
            summary: "VLM 已描述门口有人经过。".to_string(),
            derived_text: "门口有人经过。".to_string(),
            tags: vec!["person".to_string()],
            labels: vec!["person".to_string()],
            error: None,
            artifacts: Vec::new(),
            ingest_metadata: json!({"redacted": true}),
            vlm_metrics: json!({"elapsed_ms": 1200}),
        });
        let mut degraded = sample_event();
        degraded.event_id = "event-degraded".to_string();
        degraded.vlm = Some(crate::runtime::vision_event::LocalVisionEventVlmSummary {
            status: "degraded".to_string(),
            summary: "VLM endpoint unavailable.".to_string(),
            derived_text: String::new(),
            tags: Vec::new(),
            labels: Vec::new(),
            error: Some("endpoint unavailable".to_string()),
            artifacts: Vec::new(),
            ingest_metadata: json!({"redacted": true}),
            vlm_metrics: json!({}),
        });
        let plain = sample_event();
        let digest = build_family_timeline_digest(
            &[
                sample_stored_event(active),
                sample_stored_event(degraded),
                sample_stored_event(plain),
            ],
            24 * 60 * 60,
        )
        .expect("digest");

        assert_eq!(digest.vlm_coverage.total, 3);
        assert_eq!(digest.vlm_coverage.active, 1);
        assert_eq!(digest.vlm_coverage.degraded, 1);
        assert_eq!(digest.vlm_coverage.not_sampled, 1);
        assert!(digest
            .bullets
            .iter()
            .any(|bullet| bullet.contains("VLM 覆盖")));
    }

    #[test]
    fn family_timeline_query_filters_and_limits_after_sorting() {
        let now = now_epoch_ms();
        let mut older = sample_event();
        older.event_id = "event-older".to_string();
        older.camera_id = "front-door".to_string();
        older.labels = vec!["person".to_string()];
        older.started_at = now.saturating_sub(5000).to_string();

        let mut newest = older.clone();
        newest.event_id = "event-newest".to_string();
        newest.started_at = now.to_string();

        let mut other_camera = older.clone();
        other_camera.event_id = "event-side".to_string();
        other_camera.camera_id = "side-yard".to_string();

        let events = vec![
            sample_stored_event(older),
            sample_stored_event(other_camera),
            sample_stored_event(newest),
        ];
        let query = FamilyTimelineQueryOptions::new(
            60 * 60,
            Some(" front-door ".to_string()),
            Some(" PERSON ".to_string()),
            1,
        );
        let timeline = build_family_timeline_from_query(&events, &query).expect("timeline");

        assert_eq!(query.camera_filter.as_deref(), Some("front-door"));
        assert_eq!(query.label_filter.as_deref(), Some("PERSON"));
        assert_eq!(timeline.event_count, 1);
        assert_eq!(timeline.events[0].event_id, "event-newest");

        let digest = build_family_timeline_digest_from_query(&events, &query).expect("digest");
        assert!(digest.headline.contains("1 小时"));
        assert_eq!(digest.latest_event_id.as_deref(), Some("event-newest"));
    }

    #[test]
    fn family_timeline_query_options_clamp_window_and_limit() {
        let low = FamilyTimelineQueryOptions::new(1, None, None, 0);
        assert_eq!(low.window_seconds, 60);
        assert_eq!(low.limit, 1);

        let high = FamilyTimelineQueryOptions::new(10 * 24 * 60 * 60, None, None, 1000);
        assert_eq!(high.window_seconds, 7 * 24 * 60 * 60);
        assert_eq!(high.limit, 100);

        let trimmed = FamilyTimelineQueryOptions::new(
            24 * 60 * 60,
            Some("  ".to_string()),
            Some(" motion ".to_string()),
            10,
        );
        assert_eq!(trimmed.camera_filter, None);
        assert_eq!(trimmed.label_filter.as_deref(), Some("motion"));
    }

    fn sample_event() -> LocalVisionEvent {
        LocalVisionEvent {
            event_id: "event-1".to_string(),
            camera_id: "cam-1".to_string(),
            event_type: "motion_like_scene".to_string(),
            confidence: 0.77,
            labels: vec!["motion".to_string()],
            summary: "K3 本地视觉检测到画面变化。".to_string(),
            snapshot_artifact: SnapshotArtifact {
                artifact_id: Some("artifact-1".to_string()),
                path: Some("/tmp/source.jpg".to_string()),
                mime_type: Some("image/jpeg".to_string()),
                byte_size: Some(123),
                sha256: Some("abcd".to_string()),
                source: Some("fixture".to_string()),
            },
            started_at: now_epoch_ms_string(),
            analyzer: "fixture-yolo".to_string(),
            latency_ms: 42,
            metrics: json!({}),
            vlm: None,
        }
    }

    fn sample_stored_event(event: LocalVisionEvent) -> StoredLocalVisionEvent {
        StoredLocalVisionEvent {
            received_at: now_epoch_ms_string(),
            audit_record: json!({"audit_kind": "fixture"}),
            ha_mqtt_payload: json!({}),
            event,
        }
    }
}
