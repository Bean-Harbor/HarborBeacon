//! HarborLink-owned DVR contract projections exposed by HarborBeacon.
//!
//! HarborBeacon keeps policy and API compatibility metadata only. Recording,
//! retention, artifact storage, cleanup, and media I/O are executed by HarborLink.

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

const HARBORLINK_DVR_ROOT: &str = "/mnt/software/harborlink/camera-dvr";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DvrRecordingSettings {
    #[serde(default = "default_recording_root")]
    pub recording_root: String,
    #[serde(default = "default_media_library_root")]
    pub media_library_root: String,
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
    #[serde(default = "default_segment_seconds")]
    pub segment_seconds: u32,
    #[serde(default = "default_true")]
    pub continuous_recording_enabled: bool,
    #[serde(default = "default_true")]
    pub low_bitrate_stream_preferred: bool,
    #[serde(default = "default_continuous_bitrate_mbps")]
    pub continuous_bitrate_mbps: u32,
    #[serde(default = "default_true")]
    pub high_res_event_clips_enabled: bool,
    #[serde(default = "default_high_res_event_clip_seconds")]
    pub high_res_event_clip_seconds: u32,
    #[serde(default = "default_continuous_stream_path_hint")]
    pub continuous_stream_path_hint: String,
    #[serde(default = "default_high_res_stream_path_hint")]
    pub high_res_stream_path_hint: String,
    #[serde(default)]
    pub disk_budget_gb: Option<u64>,
    #[serde(default = "default_keyframe_count")]
    pub keyframe_count: u32,
    #[serde(default = "default_keyframe_interval_seconds")]
    pub keyframe_interval_seconds: u32,
    #[serde(default)]
    pub enabled_device_ids: Vec<String>,
}

impl Default for DvrRecordingSettings {
    fn default() -> Self {
        Self {
            recording_root: default_recording_root(),
            media_library_root: default_media_library_root(),
            retention_days: default_retention_days(),
            segment_seconds: default_segment_seconds(),
            continuous_recording_enabled: true,
            low_bitrate_stream_preferred: true,
            continuous_bitrate_mbps: default_continuous_bitrate_mbps(),
            high_res_event_clips_enabled: true,
            high_res_event_clip_seconds: default_high_res_event_clip_seconds(),
            continuous_stream_path_hint: default_continuous_stream_path_hint(),
            high_res_stream_path_hint: default_high_res_stream_path_hint(),
            disk_budget_gb: None,
            keyframe_count: default_keyframe_count(),
            keyframe_interval_seconds: default_keyframe_interval_seconds(),
            enabled_device_ids: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DvrCapacityEstimate {
    pub camera_count: usize,
    pub enabled_camera_count: usize,
    pub retention_days: u32,
    pub bitrate_mbps: u32,
    pub estimated_bytes_per_camera: u64,
    pub estimated_bytes_enabled_total: u64,
    pub disk_budget_bytes: Option<u64>,
    pub disk_budget_warning: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DvrRecordingStatus {
    pub device_id: String,
    pub status: String,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub stream_kind: String,
    #[serde(default)]
    pub last_segment_path: Option<String>,
    #[serde(default)]
    pub live_mjpeg_url: Option<String>,
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DvrTimelineSegment {
    pub device_id: String,
    pub file_path: String,
    #[serde(default)]
    pub sidecar_path: Option<String>,
    #[serde(default = "default_media_kind_recording")]
    pub media_kind: String,
    pub stream_kind: String,
    pub started_at: String,
    #[serde(default)]
    pub created_at: String,
    pub ended_at: String,
    pub duration_seconds: u32,
    #[serde(default)]
    pub duration_actual_seconds: Option<u32>,
    pub retention_expires_at: String,
    pub size_bytes: u64,
    #[serde(default)]
    pub replay_url: Option<String>,
    #[serde(default)]
    pub thumbnail_url: Option<String>,
    #[serde(default = "default_true")]
    pub playable: bool,
    #[serde(default)]
    pub indexed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DvrTimelineResponse {
    pub generated_at: String,
    pub recording_root: String,
    pub media_library_root: String,
    pub segments: Vec<DvrTimelineSegment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DvrRecordingStatusResponse {
    pub generated_at: String,
    pub settings: DvrRecordingSettings,
    pub capacity: DvrCapacityEstimate,
    pub statuses: Vec<DvrRecordingStatus>,
    pub root_exists: bool,
    pub root_writable: bool,
}

pub fn sanitize_dvr_recording_settings(mut settings: DvrRecordingSettings) -> DvrRecordingSettings {
    settings.recording_root = default_recording_root();
    settings.media_library_root = default_media_library_root();
    settings.retention_days = settings.retention_days.clamp(1, 365);
    settings.segment_seconds = settings.segment_seconds.clamp(30, 3_600);
    settings.continuous_bitrate_mbps = settings.continuous_bitrate_mbps.clamp(1, 20);
    settings.high_res_event_clip_seconds = settings.high_res_event_clip_seconds.clamp(3, 600);
    settings.continuous_stream_path_hint =
        normalize_rtsp_path_hint(&settings.continuous_stream_path_hint, "/stream2");
    settings.high_res_stream_path_hint =
        normalize_rtsp_path_hint(&settings.high_res_stream_path_hint, "/stream1");
    settings.disk_budget_gb = settings.disk_budget_gb.filter(|value| *value > 0);
    settings.keyframe_count = settings.keyframe_count.clamp(1, 12);
    settings.keyframe_interval_seconds = settings.keyframe_interval_seconds.clamp(1, 3_600);
    settings.enabled_device_ids = dedupe_non_empty(settings.enabled_device_ids);
    settings
}

pub fn default_recording_root() -> String {
    HARBORLINK_DVR_ROOT.to_string()
}

pub fn default_media_library_root() -> String {
    format!("{HARBORLINK_DVR_ROOT}/library")
}

pub fn dvr_capacity_estimate(
    settings: &DvrRecordingSettings,
    camera_count: usize,
) -> DvrCapacityEstimate {
    let settings = sanitize_dvr_recording_settings(settings.clone());
    let enabled_camera_count = settings.enabled_device_ids.len();
    let seconds = u64::from(settings.retention_days) * 24 * 60 * 60;
    let estimated_bytes_per_camera =
        u64::from(settings.continuous_bitrate_mbps) * 1_000_000 * seconds / 8;
    let estimated_bytes_enabled_total =
        estimated_bytes_per_camera.saturating_mul(enabled_camera_count as u64);
    let disk_budget_bytes = settings
        .disk_budget_gb
        .map(|gb| gb.saturating_mul(1_000_000_000));
    let disk_budget_warning = disk_budget_bytes.and_then(|budget| {
        (enabled_camera_count > 0 && estimated_bytes_enabled_total > budget).then(|| {
            format!(
                "Estimated DVR usage {} GB exceeds configured disk budget {} GB.",
                bytes_to_decimal_gb(estimated_bytes_enabled_total),
                bytes_to_decimal_gb(budget)
            )
        })
    });
    DvrCapacityEstimate {
        camera_count,
        enabled_camera_count,
        retention_days: settings.retention_days,
        bitrate_mbps: settings.continuous_bitrate_mbps,
        estimated_bytes_per_camera,
        estimated_bytes_enabled_total,
        disk_budget_bytes,
        disk_budget_warning,
    }
}

pub fn build_status_response(
    settings: DvrRecordingSettings,
    statuses: Vec<DvrRecordingStatus>,
    camera_count: usize,
) -> DvrRecordingStatusResponse {
    let settings = sanitize_dvr_recording_settings(settings);
    let contract_available = statuses.iter().all(|status| status.status != "degraded");
    DvrRecordingStatusResponse {
        generated_at: now_unix_secs().to_string(),
        capacity: dvr_capacity_estimate(&settings, camera_count),
        root_exists: contract_available,
        root_writable: contract_available,
        settings,
        statuses,
    }
}

fn normalize_rtsp_path_hint(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

fn dedupe_non_empty(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    values
        .into_iter()
        .filter_map(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() || !seen.insert(trimmed.to_string()) {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
        .collect()
}

fn bytes_to_decimal_gb(bytes: u64) -> u64 {
    bytes / 1_000_000_000
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn default_true() -> bool {
    true
}

fn default_media_kind_recording() -> String {
    "recording".to_string()
}

fn default_retention_days() -> u32 {
    7
}

fn default_segment_seconds() -> u32 {
    300
}

fn default_continuous_bitrate_mbps() -> u32 {
    2
}

fn default_high_res_event_clip_seconds() -> u32 {
    30
}

fn default_continuous_stream_path_hint() -> String {
    "/stream2".to_string()
}

fn default_high_res_stream_path_hint() -> String {
    "/stream1".to_string()
}

fn default_keyframe_count() -> u32 {
    5
}

fn default_keyframe_interval_seconds() -> u32 {
    60
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_reject_beacon_owned_storage_paths() {
        let settings = sanitize_dvr_recording_settings(DvrRecordingSettings {
            recording_root: "/tmp/beacon-dvr".to_string(),
            media_library_root: "/tmp/beacon-library".to_string(),
            retention_days: 0,
            ..DvrRecordingSettings::default()
        });
        assert_eq!(settings.recording_root, HARBORLINK_DVR_ROOT);
        assert_eq!(
            settings.media_library_root,
            format!("{HARBORLINK_DVR_ROOT}/library")
        );
        assert_eq!(settings.retention_days, 1);
    }

    #[test]
    fn capacity_projection_does_not_touch_the_filesystem() {
        let settings = DvrRecordingSettings {
            retention_days: 7,
            continuous_bitrate_mbps: 2,
            enabled_device_ids: vec!["cam-1".to_string(), "cam-2".to_string()],
            ..DvrRecordingSettings::default()
        };
        let estimate = dvr_capacity_estimate(&settings, 3);
        assert_eq!(estimate.camera_count, 3);
        assert_eq!(estimate.enabled_camera_count, 2);
        assert!(estimate.estimated_bytes_enabled_total > 0);
    }

    #[test]
    fn degraded_link_status_marks_contract_unavailable() {
        let response = build_status_response(
            DvrRecordingSettings::default(),
            vec![DvrRecordingStatus {
                device_id: "cam-1".to_string(),
                status: "degraded".to_string(),
                started_at: None,
                updated_at: None,
                stream_kind: String::new(),
                last_segment_path: None,
                live_mjpeg_url: None,
                message: "HarborLink unavailable".to_string(),
            }],
            1,
        );
        assert!(!response.root_exists);
        assert!(!response.root_writable);
    }
}
