//! Local camera DVR settings, segment metadata, and ffmpeg process control.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::runtime::media_tools::{
    ffmpeg_resolution_hint, resolve_ffmpeg_bin, resolve_ffprobe_bin,
};
use crate::runtime::registry::CameraDevice;

const DEFAULT_HARBOROS_WRITABLE_ROOT: &str = "/mnt/software/harborbeacon-agent-ci";
const HARBOROS_WRITABLE_ROOT_ENV: &str = "HARBOR_HARBOROS_WRITABLE_ROOT";
const DVR_KNOWLEDGE_ROOT_ID: &str = "camera-dvr-recordings";
const MIN_PLAYABLE_MP4_BYTES: u64 = 4096;
const FFMPEG_GRACEFUL_STOP_TIMEOUT_MS: u64 = 5_000;
const FFMPEG_START_SETTLE_MS: u64 = 500;

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

#[derive(Debug)]
struct DvrProcess {
    child: Child,
    started_at: String,
    stream_kind: String,
    pattern: PathBuf,
}

#[derive(Debug, Clone, Default)]
pub struct DvrRuntime {
    processes: Arc<Mutex<HashMap<String, DvrProcess>>>,
    last_failures: Arc<Mutex<HashMap<String, DvrRecordingStatus>>>,
}

impl DvrRuntime {
    pub fn start_recording(
        &self,
        device: &CameraDevice,
        settings: &DvrRecordingSettings,
        public_origin: Option<&str>,
    ) -> Result<DvrRecordingStatus, String> {
        let settings = sanitize_dvr_recording_settings(settings.clone());
        if !settings.continuous_recording_enabled {
            return Err(
                "continuous DVR recording is disabled in Harbor Assistant settings".to_string(),
            );
        }
        let ffmpeg_bin = resolve_ffmpeg_bin().ok_or_else(|| {
            format!(
                "ffmpeg is required for local DVR recording but is unavailable; {}",
                ffmpeg_resolution_hint()
            )
        })?;
        let stream_url = recording_stream_url(device, &settings);
        if stream_url.trim().is_empty() {
            return Err(format!(
                "camera {} does not expose a usable RTSP stream URL",
                device.device_id
            ));
        }

        let root = media_library_root_path(&settings);
        fs::create_dir_all(
            root.join("recordings")
                .join(device_path_component(&device.device_id)),
        )
        .map_err(|error| format!("failed to create DVR recording directory: {error}"))?;
        prepare_segment_calendar_dirs(&root, &device.device_id, now_unix_secs(), &settings)
            .map_err(|error| {
                format!("failed to create DVR segment calendar directories: {error}")
            })?;
        let pattern = segment_output_pattern(&settings, &device.device_id);
        let mut command = Command::new(&ffmpeg_bin);
        command
            .env("TZ", "UTC")
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("warning")
            .arg("-rtsp_transport")
            .arg("tcp")
            .arg("-i")
            .arg(&stream_url)
            .arg("-map")
            .arg("0:v:0")
            .arg("-map")
            .arg("0:a:0?")
            .arg("-c:v")
            .arg("copy")
            .arg("-c:a")
            .arg("aac")
            .arg("-b:a")
            .arg("64k")
            .arg("-ac")
            .arg("1")
            .arg("-ar")
            .arg("48000")
            .arg("-f")
            .arg("segment")
            .arg("-segment_time")
            .arg(settings.segment_seconds.to_string())
            .arg("-segment_format_options")
            .arg("movflags=+faststart")
            .arg("-reset_timestamps")
            .arg("1")
            .arg("-strftime")
            .arg("1")
            .arg("-strftime_mkdir")
            .arg("1")
            .arg(pattern.to_string_lossy().into_owned())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| format!("failed to start DVR ffmpeg process: {error}"))?;
        thread::sleep(Duration::from_millis(FFMPEG_START_SETTLE_MS));
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("failed to inspect DVR ffmpeg process: {error}"))?
        {
            return Err(format!(
                "DVR ffmpeg process exited during start with status {}",
                process_status_label(status)
            ));
        }
        let started_at = now_unix_secs().to_string();
        let stream_kind = if settings.low_bitrate_stream_preferred {
            "substream"
        } else {
            "mainstream"
        }
        .to_string();

        let mut processes = self
            .processes
            .lock()
            .map_err(|_| "DVR runtime process lock is poisoned".to_string())?;
        if let Some(existing) = processes.remove(&device.device_id) {
            let _ = gracefully_stop_ffmpeg(existing.child);
        }
        processes.insert(
            device.device_id.clone(),
            DvrProcess {
                child,
                started_at: started_at.clone(),
                stream_kind: stream_kind.clone(),
                pattern,
            },
        );
        self.last_failures
            .lock()
            .map_err(|_| "DVR runtime failure lock is poisoned".to_string())?
            .remove(&device.device_id);

        Ok(DvrRecordingStatus {
            device_id: device.device_id.clone(),
            status: "recording".to_string(),
            started_at: Some(started_at.clone()),
            updated_at: Some(started_at),
            stream_kind,
            last_segment_path: latest_segment_path(&settings, &device.device_id),
            live_mjpeg_url: public_origin.map(|origin| {
                format!(
                    "{}/api/cameras/{}/live.mjpeg",
                    origin.trim_end_matches('/'),
                    url_encode_path_segment(&device.device_id)
                )
            }),
            message: "continuous low-bitrate DVR recording is running".to_string(),
        })
    }

    pub fn stop_recording(
        &self,
        device_id: &str,
        public_origin: Option<&str>,
    ) -> Result<DvrRecordingStatus, String> {
        let mut processes = self
            .processes
            .lock()
            .map_err(|_| "DVR runtime process lock is poisoned".to_string())?;
        let Some(process) = processes.remove(device_id) else {
            self.last_failures
                .lock()
                .map_err(|_| "DVR runtime failure lock is poisoned".to_string())?
                .remove(device_id);
            return Ok(DvrRecordingStatus {
                device_id: device_id.to_string(),
                status: "stopped".to_string(),
                started_at: None,
                updated_at: Some(now_unix_secs().to_string()),
                stream_kind: "substream".to_string(),
                last_segment_path: None,
                live_mjpeg_url: public_origin.map(|origin| {
                    format!(
                        "{}/api/cameras/{}/live.mjpeg",
                        origin.trim_end_matches('/'),
                        url_encode_path_segment(device_id)
                    )
                }),
                message: "DVR recording was not running".to_string(),
            });
        };
        self.last_failures
            .lock()
            .map_err(|_| "DVR runtime failure lock is poisoned".to_string())?
            .remove(device_id);
        let _ = gracefully_stop_ffmpeg(process.child);
        let last_segment_path = latest_segment_path_for_pattern(&process.pattern);
        Ok(DvrRecordingStatus {
            device_id: device_id.to_string(),
            status: "stopped".to_string(),
            started_at: Some(process.started_at),
            updated_at: Some(now_unix_secs().to_string()),
            stream_kind: process.stream_kind,
            last_segment_path,
            live_mjpeg_url: public_origin.map(|origin| {
                format!(
                    "{}/api/cameras/{}/live.mjpeg",
                    origin.trim_end_matches('/'),
                    url_encode_path_segment(device_id)
                )
            }),
            message: "DVR recording stopped".to_string(),
        })
    }

    pub fn statuses(
        &self,
        devices: &[CameraDevice],
        settings: &DvrRecordingSettings,
        public_origin: Option<&str>,
    ) -> Result<Vec<DvrRecordingStatus>, String> {
        let settings = sanitize_dvr_recording_settings(settings.clone());
        let mut processes = self
            .processes
            .lock()
            .map_err(|_| "DVR runtime process lock is poisoned".to_string())?;
        let previous_failures = self
            .last_failures
            .lock()
            .map_err(|_| "DVR runtime failure lock is poisoned".to_string())?
            .clone();
        let mut statuses = Vec::new();
        let mut exited = Vec::new();
        let mut new_failures = Vec::new();
        for device in devices {
            let live_mjpeg_url = public_origin.map(|origin| {
                format!(
                    "{}/api/cameras/{}/live.mjpeg",
                    origin.trim_end_matches('/'),
                    url_encode_path_segment(&device.device_id)
                )
            });
            if let Some(process) = processes.get_mut(&device.device_id) {
                match process.child.try_wait() {
                    Ok(Some(status)) => {
                        let failed = DvrRecordingStatus {
                            device_id: device.device_id.clone(),
                            status: "failed".to_string(),
                            started_at: Some(process.started_at.clone()),
                            updated_at: Some(now_unix_secs().to_string()),
                            stream_kind: process.stream_kind.clone(),
                            last_segment_path: latest_segment_path(&settings, &device.device_id),
                            live_mjpeg_url,
                            message: format!(
                                "DVR ffmpeg process exited with status {}",
                                process_status_label(status)
                            ),
                        };
                        new_failures.push(failed.clone());
                        statuses.push(failed);
                        exited.push(device.device_id.clone());
                    }
                    Ok(None) => statuses.push(DvrRecordingStatus {
                        device_id: device.device_id.clone(),
                        status: "recording".to_string(),
                        started_at: Some(process.started_at.clone()),
                        updated_at: Some(now_unix_secs().to_string()),
                        stream_kind: process.stream_kind.clone(),
                        last_segment_path: latest_segment_path(&settings, &device.device_id),
                        live_mjpeg_url,
                        message: format!(
                            "DVR segmenter writing {}",
                            process.pattern.to_string_lossy()
                        ),
                    }),
                    Err(error) => {
                        statuses.push(DvrRecordingStatus {
                            device_id: device.device_id.clone(),
                            status: "failed".to_string(),
                            started_at: Some(process.started_at.clone()),
                            updated_at: Some(now_unix_secs().to_string()),
                            stream_kind: process.stream_kind.clone(),
                            last_segment_path: latest_segment_path(&settings, &device.device_id),
                            live_mjpeg_url,
                            message: format!("failed to inspect DVR process: {error}"),
                        });
                        exited.push(device.device_id.clone());
                    }
                }
            } else if let Some(failed) = previous_failures.get(&device.device_id) {
                statuses.push(failed.clone());
            } else {
                statuses.push(DvrRecordingStatus {
                    device_id: device.device_id.clone(),
                    status: "stopped".to_string(),
                    started_at: None,
                    updated_at: Some(now_unix_secs().to_string()),
                    stream_kind: if settings.low_bitrate_stream_preferred {
                        "substream".to_string()
                    } else {
                        "mainstream".to_string()
                    },
                    last_segment_path: latest_segment_path(&settings, &device.device_id),
                    live_mjpeg_url,
                    message: "DVR recording is not running".to_string(),
                });
            }
        }
        for device_id in exited {
            processes.remove(&device_id);
        }
        if !new_failures.is_empty() {
            let mut failures = self
                .last_failures
                .lock()
                .map_err(|_| "DVR runtime failure lock is poisoned".to_string())?;
            for failure in new_failures {
                failures.insert(failure.device_id.clone(), failure);
            }
        }
        Ok(statuses)
    }
}

pub fn sanitize_dvr_recording_settings(mut settings: DvrRecordingSettings) -> DvrRecordingSettings {
    settings.recording_root = settings.recording_root.trim().to_string();
    if settings.recording_root.is_empty() {
        settings.recording_root = default_recording_root();
    }
    settings.media_library_root = settings.media_library_root.trim().to_string();
    if settings.media_library_root.is_empty()
        || (settings.media_library_root == default_media_library_root()
            && settings.recording_root != default_recording_root())
    {
        settings.media_library_root =
            default_media_library_root_for_recording_root(&settings.recording_root);
    }
    settings.retention_days = settings.retention_days.clamp(1, 365);
    settings.segment_seconds = settings.segment_seconds.clamp(30, 3600);
    settings.continuous_bitrate_mbps = settings.continuous_bitrate_mbps.clamp(1, 20);
    settings.high_res_event_clip_seconds = settings.high_res_event_clip_seconds.clamp(3, 600);
    settings.continuous_stream_path_hint =
        normalize_rtsp_path_hint(&settings.continuous_stream_path_hint, "/stream2");
    settings.high_res_stream_path_hint =
        normalize_rtsp_path_hint(&settings.high_res_stream_path_hint, "/stream1");
    settings.disk_budget_gb = settings.disk_budget_gb.filter(|value| *value > 0);
    settings.keyframe_count = settings.keyframe_count.clamp(1, 12);
    settings.keyframe_interval_seconds = settings.keyframe_interval_seconds.clamp(1, 3600);
    settings.enabled_device_ids = dedupe_non_empty(settings.enabled_device_ids);
    settings
}

pub fn default_recording_root() -> String {
    Path::new(
        &std::env::var(HARBOROS_WRITABLE_ROOT_ENV)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_HARBOROS_WRITABLE_ROOT.to_string()),
    )
    .join("camera-dvr")
    .to_string_lossy()
    .into_owned()
}

pub fn default_media_library_root() -> String {
    default_media_library_root_for_recording_root(&default_recording_root())
}

pub fn default_media_library_root_for_recording_root(recording_root: &str) -> String {
    Path::new(recording_root.trim())
        .join("library")
        .to_string_lossy()
        .into_owned()
}

pub fn dvr_knowledge_root_id() -> &'static str {
    DVR_KNOWLEDGE_ROOT_ID
}

pub fn dvr_capacity_estimate(
    settings: &DvrRecordingSettings,
    camera_count: usize,
) -> DvrCapacityEstimate {
    let settings = sanitize_dvr_recording_settings(settings.clone());
    let enabled_camera_count = settings.enabled_device_ids.len();
    let seconds = settings.retention_days as u64 * 24 * 60 * 60;
    let estimated_bytes_per_camera =
        settings.continuous_bitrate_mbps as u64 * 1_000_000 * seconds / 8;
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

pub fn recording_segment_path(recording_root: &Path, device_id: &str, unix_secs: u64) -> PathBuf {
    let (year, month, day, hour, minute, second) = unix_to_utc_parts(unix_secs);
    recording_root
        .join("recordings")
        .join(device_path_component(device_id))
        .join(format!("{year:04}"))
        .join(format!("{month:02}"))
        .join(format!("{day:02}"))
        .join(format!("{hour:02}{minute:02}{second:02}.mp4"))
}

pub fn scan_timeline(
    settings: &DvrRecordingSettings,
    devices: &[CameraDevice],
    device_filter: Option<&str>,
    from_secs: Option<u64>,
    to_secs: Option<u64>,
    public_origin: Option<&str>,
) -> Result<DvrTimelineResponse, String> {
    let settings = sanitize_dvr_recording_settings(settings.clone());
    let root = media_library_root_path(&settings);
    let mut segments = Vec::new();
    let device_lookup = devices
        .iter()
        .map(|device| (device.device_id.as_str(), device))
        .collect::<HashMap<_, _>>();
    let recording_root = root.join("recordings");
    if recording_root.exists() {
        collect_segments(
            &recording_root,
            &root,
            &settings,
            &device_lookup,
            device_filter,
            from_secs,
            to_secs,
            public_origin,
            &mut segments,
        )?;
    }
    let snapshot_root = root.join("snapshots");
    if snapshot_root.exists() {
        collect_snapshots(
            &snapshot_root,
            &root,
            &settings,
            &device_lookup,
            device_filter,
            from_secs,
            to_secs,
            public_origin,
            &mut segments,
        )?;
    }
    segments.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| right.file_path.cmp(&left.file_path))
    });
    Ok(DvrTimelineResponse {
        generated_at: now_unix_secs().to_string(),
        recording_root: settings.recording_root,
        media_library_root: settings.media_library_root,
        segments,
    })
}

pub fn apply_retention_policy(settings: &DvrRecordingSettings) -> Result<usize, String> {
    let settings = sanitize_dvr_recording_settings(settings.clone());
    let cutoff = now_unix_secs().saturating_sub(settings.retention_days as u64 * 24 * 60 * 60);
    let media_root = media_library_root_path(&settings);
    let recording_root = media_root.join("recordings");
    let snapshot_root = media_root.join("snapshots");
    if !recording_root.exists() && !snapshot_root.exists() {
        return Ok(0);
    }
    let mut removed = 0usize;
    let mut files = Vec::new();
    if recording_root.exists() {
        collect_mp4_paths(&recording_root, &mut files)?;
    }
    if snapshot_root.exists() {
        collect_snapshot_paths(&snapshot_root, &mut files)?;
    }
    for file in files {
        let timestamp = segment_timestamp_from_path(&file).or_else(|| file_modified_secs(&file));
        if timestamp.is_some_and(|value| value < cutoff) {
            remove_file_if_exists(&file, &mut removed)?;
            for sidecar in sidecar_candidates(&file) {
                remove_file_if_exists(&sidecar, &mut removed)?;
            }
            if let Some(stem) = file.file_stem().and_then(|value| value.to_str()) {
                remove_dir_if_exists(&file.with_file_name(format!("{stem}.frames")), &mut removed)?;
            }
        }
    }
    Ok(removed)
}

pub fn build_status_response(
    settings: DvrRecordingSettings,
    statuses: Vec<DvrRecordingStatus>,
    camera_count: usize,
) -> DvrRecordingStatusResponse {
    let settings = sanitize_dvr_recording_settings(settings);
    let root = media_library_root_path(&settings);
    DvrRecordingStatusResponse {
        generated_at: now_unix_secs().to_string(),
        capacity: dvr_capacity_estimate(&settings, camera_count),
        root_exists: root.exists(),
        root_writable: path_can_accept_write(&root),
        settings,
        statuses,
    }
}

pub fn recording_stream_url(device: &CameraDevice, settings: &DvrRecordingSettings) -> String {
    if !settings.low_bitrate_stream_preferred
        || rtsp_url_uses_low_bitrate_stream(&device.primary_stream.url)
    {
        return device.primary_stream.url.clone();
    }
    replace_rtsp_path(
        &device.primary_stream.url,
        &settings.continuous_stream_path_hint,
    )
    .unwrap_or_else(|| device.primary_stream.url.clone())
}

fn collect_segments(
    directory: &Path,
    media_root: &Path,
    settings: &DvrRecordingSettings,
    device_lookup: &HashMap<&str, &CameraDevice>,
    device_filter: Option<&str>,
    from_secs: Option<u64>,
    to_secs: Option<u64>,
    public_origin: Option<&str>,
    segments: &mut Vec<DvrTimelineSegment>,
) -> Result<(), String> {
    let entries = fs::read_dir(directory).map_err(|error| {
        format!(
            "failed to read DVR directory {}: {error}",
            directory.display()
        )
    })?;
    for entry in entries {
        let entry =
            entry.map_err(|error| format!("failed to read DVR directory entry: {error}"))?;
        let path = entry.path();
        if path.is_dir() {
            collect_segments(
                &path,
                media_root,
                settings,
                device_lookup,
                device_filter,
                from_secs,
                to_secs,
                public_origin,
                segments,
            )?;
            continue;
        }
        if path
            .extension()
            .and_then(|value| value.to_str())
            .is_none_or(|extension| !extension.eq_ignore_ascii_case("mp4"))
        {
            continue;
        }
        let device_id =
            device_id_from_media_path(media_root, "recordings", &path).unwrap_or_default();
        if device_id.is_empty() {
            continue;
        }
        if device_filter.is_some_and(|filter| filter != device_id) {
            continue;
        }
        let started = segment_timestamp_from_path(&path).or_else(|| file_modified_secs(&path));
        let Some(started) = started else {
            continue;
        };
        let size_bytes = fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let playback = recording_playback_info(&path, size_bytes, settings.segment_seconds);
        if !playback.playable {
            continue;
        }
        let actual_duration = playback
            .duration_seconds
            .unwrap_or(settings.segment_seconds.max(1));
        let ended = started.saturating_add(actual_duration as u64);
        if from_secs.is_some_and(|from| ended < from) || to_secs.is_some_and(|to| started > to) {
            continue;
        }
        let retention_expires_at =
            started.saturating_add(settings.retention_days as u64 * 24 * 60 * 60);
        let sidecar_path = path.with_extension("json");
        let sidecar_exists = sidecar_path.exists();
        let sidecar_path_text = sidecar_path.to_string_lossy().into_owned();
        let replay_url = public_origin.map(|origin| {
            format!(
                "{}/api/knowledge/preview?path={}",
                origin.trim_end_matches('/'),
                url_encode_query_component(&path.to_string_lossy())
            )
        });
        let mut segment = DvrTimelineSegment {
            device_id: device_id.clone(),
            file_path: path.to_string_lossy().into_owned(),
            sidecar_path: Some(sidecar_path_text),
            media_kind: "recording".to_string(),
            stream_kind: if settings.low_bitrate_stream_preferred {
                "substream".to_string()
            } else {
                "mainstream".to_string()
            },
            started_at: started.to_string(),
            created_at: started.to_string(),
            ended_at: ended.to_string(),
            duration_seconds: settings.segment_seconds,
            duration_actual_seconds: Some(actual_duration),
            retention_expires_at: retention_expires_at.to_string(),
            size_bytes,
            replay_url: replay_url.clone(),
            thumbnail_url: replay_url,
            playable: true,
            indexed: sidecar_exists,
        };
        if let Some(device) = device_lookup.get(device_id.as_str()) {
            write_segment_sidecar(&segment, device, settings)?;
            segment.indexed = true;
        }
        segments.push(segment);
    }
    Ok(())
}

fn collect_snapshots(
    directory: &Path,
    media_root: &Path,
    settings: &DvrRecordingSettings,
    device_lookup: &HashMap<&str, &CameraDevice>,
    device_filter: Option<&str>,
    from_secs: Option<u64>,
    to_secs: Option<u64>,
    public_origin: Option<&str>,
    segments: &mut Vec<DvrTimelineSegment>,
) -> Result<(), String> {
    let entries = fs::read_dir(directory).map_err(|error| {
        format!(
            "failed to read DVR snapshot directory {}: {error}",
            directory.display()
        )
    })?;
    for entry in entries {
        let entry = entry
            .map_err(|error| format!("failed to read DVR snapshot directory entry: {error}"))?;
        let path = entry.path();
        if path.is_dir() {
            collect_snapshots(
                &path,
                media_root,
                settings,
                device_lookup,
                device_filter,
                from_secs,
                to_secs,
                public_origin,
                segments,
            )?;
            continue;
        }
        if !is_snapshot_path(&path) {
            continue;
        }
        let device_id =
            device_id_from_media_path(media_root, "snapshots", &path).unwrap_or_default();
        if device_id.is_empty() {
            continue;
        }
        if device_filter.is_some_and(|filter| filter != device_id) {
            continue;
        }
        let created = segment_timestamp_from_path(&path).or_else(|| file_modified_secs(&path));
        let Some(created) = created else {
            continue;
        };
        if from_secs.is_some_and(|from| created < from) || to_secs.is_some_and(|to| created > to) {
            continue;
        }
        let size_bytes = fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if size_bytes == 0 {
            continue;
        }
        let retention_expires_at =
            created.saturating_add(settings.retention_days as u64 * 24 * 60 * 60);
        let sidecar_path = path.with_extension("json");
        let replay_url = public_origin.map(|origin| {
            format!(
                "{}/api/knowledge/preview?path={}",
                origin.trim_end_matches('/'),
                url_encode_query_component(&path.to_string_lossy())
            )
        });
        let mut segment = DvrTimelineSegment {
            device_id: device_id.clone(),
            file_path: path.to_string_lossy().into_owned(),
            sidecar_path: Some(sidecar_path.to_string_lossy().into_owned()),
            media_kind: "snapshot".to_string(),
            stream_kind: "snapshot".to_string(),
            started_at: created.to_string(),
            created_at: created.to_string(),
            ended_at: created.to_string(),
            duration_seconds: 0,
            duration_actual_seconds: Some(0),
            retention_expires_at: retention_expires_at.to_string(),
            size_bytes,
            replay_url: replay_url.clone(),
            thumbnail_url: replay_url,
            playable: true,
            indexed: sidecar_path.exists(),
        };
        if let Some(device) = device_lookup.get(device_id.as_str()) {
            write_snapshot_sidecar(&segment, device)?;
            segment.indexed = true;
        }
        segments.push(segment);
    }
    Ok(())
}

pub fn store_snapshot_bytes(
    settings: &DvrRecordingSettings,
    device: &CameraDevice,
    bytes: &[u8],
    public_origin: Option<&str>,
) -> Result<DvrTimelineSegment, String> {
    if bytes.is_empty() {
        return Err("DVR snapshot bytes were empty".to_string());
    }
    let settings = sanitize_dvr_recording_settings(settings.clone());
    let created = now_unix_secs();
    let path = snapshot_media_path(
        &media_library_root_path(&settings),
        &device.device_id,
        created,
    );
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create DVR snapshot directory {}: {error}",
                parent.display()
            )
        })?;
    }
    fs::write(&path, bytes)
        .map_err(|error| format!("failed to write DVR snapshot {}: {error}", path.display()))?;
    let retention_expires_at =
        created.saturating_add(settings.retention_days as u64 * 24 * 60 * 60);
    let sidecar_path = path.with_extension("json");
    let replay_url = public_origin.map(|origin| {
        format!(
            "{}/api/knowledge/preview?path={}",
            origin.trim_end_matches('/'),
            url_encode_query_component(&path.to_string_lossy())
        )
    });
    let segment = DvrTimelineSegment {
        device_id: device.device_id.clone(),
        file_path: path.to_string_lossy().into_owned(),
        sidecar_path: Some(sidecar_path.to_string_lossy().into_owned()),
        media_kind: "snapshot".to_string(),
        stream_kind: "snapshot".to_string(),
        started_at: created.to_string(),
        created_at: created.to_string(),
        ended_at: created.to_string(),
        duration_seconds: 0,
        duration_actual_seconds: Some(0),
        retention_expires_at: retention_expires_at.to_string(),
        size_bytes: bytes.len() as u64,
        replay_url: replay_url.clone(),
        thumbnail_url: replay_url,
        playable: true,
        indexed: true,
    };
    write_snapshot_sidecar(&segment, device)?;
    Ok(segment)
}

fn write_segment_sidecar(
    segment: &DvrTimelineSegment,
    device: &CameraDevice,
    settings: &DvrRecordingSettings,
) -> Result<(), String> {
    let sidecar_path = Path::new(&segment.file_path).with_extension("json");
    let payload = json!({
        "media_asset": {
            "kind": segment.media_kind,
            "source": "camera_dvr",
            "device_id": segment.device_id,
            "device_name": device.name,
            "room": device.room,
            "vendor": device.vendor,
            "model": device.model,
            "stream_kind": segment.stream_kind,
            "started_at": segment.started_at,
            "created_at": segment.created_at,
            "ended_at": segment.ended_at,
            "duration_seconds": segment.duration_seconds,
            "duration_actual_seconds": segment.duration_actual_seconds,
            "retention_expires_at": segment.retention_expires_at,
            "playable": segment.playable,
            "source_video_path": segment.file_path,
            "labels": ["video", "recording", "dvr", "analysis_pending"],
            "analysis_pipeline": "multimodal_rag_vlm",
            "model_boundary": "reuse_model_center_vlm_and_existing_knowledge_index",
            "keyframe_count": settings.keyframe_count,
            "keyframe_interval_seconds": settings.keyframe_interval_seconds
        },
        "caption": format!(
            "Camera DVR recording from {} between {} and {}. Video sidecar is ready for existing multimodal RAG/VLM indexing.",
            segment.device_id, segment.started_at, segment.ended_at
        ),
        "derived_text": "local continuous DVR segment; analyze via existing HarborBeacon knowledge index video keyframe VLM path",
        "source_video_path": segment.file_path,
        "camera": {
            "device_id": segment.device_id,
            "name": device.name,
            "room": device.room,
            "ip_address": device.ip_address
        },
        "labels": ["video", "recording", "dvr", "analysis_pending"]
    });
    fs::write(
        &sidecar_path,
        serde_json::to_vec_pretty(&payload)
            .map_err(|error| format!("failed to serialize DVR sidecar: {error}"))?,
    )
    .map_err(|error| {
        format!(
            "failed to write DVR sidecar {}: {error}",
            sidecar_path.display()
        )
    })
}

fn write_snapshot_sidecar(
    segment: &DvrTimelineSegment,
    device: &CameraDevice,
) -> Result<(), String> {
    let sidecar_path = Path::new(&segment.file_path).with_extension("json");
    let payload = json!({
        "media_asset": {
            "kind": "snapshot",
            "source": "camera_dvr",
            "device_id": segment.device_id,
            "device_name": device.name,
            "room": device.room,
            "vendor": device.vendor,
            "model": device.model,
            "created_at": segment.created_at,
            "retention_expires_at": segment.retention_expires_at,
            "source_image_path": segment.file_path,
            "labels": ["image", "snapshot", "dvr", "analysis_pending"],
            "analysis_pipeline": "multimodal_rag_vlm",
            "model_boundary": "reuse_model_center_vlm_and_existing_knowledge_index"
        },
        "caption": format!(
            "Camera DVR snapshot from {} at {}. Image sidecar is ready for existing multimodal RAG/VLM indexing.",
            segment.device_id, segment.created_at
        ),
        "derived_text": "local camera DVR snapshot; analyze via existing HarborBeacon knowledge index image OCR/VLM path",
        "source_image_path": segment.file_path,
        "camera": {
            "device_id": segment.device_id,
            "name": device.name,
            "room": device.room,
            "ip_address": device.ip_address
        },
        "labels": ["image", "snapshot", "dvr", "analysis_pending"]
    });
    fs::write(
        &sidecar_path,
        serde_json::to_vec_pretty(&payload)
            .map_err(|error| format!("failed to serialize DVR snapshot sidecar: {error}"))?,
    )
    .map_err(|error| {
        format!(
            "failed to write DVR snapshot sidecar {}: {error}",
            sidecar_path.display()
        )
    })
}

fn collect_mp4_paths(directory: &Path, paths: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(directory).map_err(|error| {
        format!(
            "failed to read DVR directory {}: {error}",
            directory.display()
        )
    })? {
        let entry =
            entry.map_err(|error| format!("failed to read DVR directory entry: {error}"))?;
        let path = entry.path();
        if path.is_dir() {
            collect_mp4_paths(&path, paths)?;
        } else if path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("mp4"))
        {
            paths.push(path);
        }
    }
    Ok(())
}

fn collect_snapshot_paths(directory: &Path, paths: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in fs::read_dir(directory).map_err(|error| {
        format!(
            "failed to read DVR snapshot directory {}: {error}",
            directory.display()
        )
    })? {
        let entry = entry
            .map_err(|error| format!("failed to read DVR snapshot directory entry: {error}"))?;
        let path = entry.path();
        if path.is_dir() {
            collect_snapshot_paths(&path, paths)?;
        } else if is_snapshot_path(&path) {
            paths.push(path);
        }
    }
    Ok(())
}

fn latest_segment_path(settings: &DvrRecordingSettings, device_id: &str) -> Option<String> {
    let root = media_library_root_path(settings)
        .join("recordings")
        .join(device_path_component(device_id));
    if !root.exists() {
        return None;
    }
    let mut paths = Vec::new();
    collect_mp4_paths(&root, &mut paths).ok()?;
    paths
        .sort_by_key(|path| segment_timestamp_from_path(path).or_else(|| file_modified_secs(path)));
    paths.last().map(|path| path.to_string_lossy().into_owned())
}

fn latest_segment_path_for_pattern(pattern: &Path) -> Option<String> {
    let device_root = pattern
        .parent()?
        .parent()?
        .parent()?
        .parent()?
        .to_path_buf();
    if !device_root.exists() {
        return None;
    }
    let mut paths = Vec::new();
    collect_mp4_paths(&device_root, &mut paths).ok()?;
    paths.retain(|path| {
        let size_bytes = fs::metadata(path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        recording_playback_info(path, size_bytes, 1).playable
    });
    paths
        .sort_by_key(|path| segment_timestamp_from_path(path).or_else(|| file_modified_secs(path)));
    paths.last().map(|path| path.to_string_lossy().into_owned())
}

fn segment_output_pattern(settings: &DvrRecordingSettings, device_id: &str) -> PathBuf {
    media_library_root_path(settings)
        .join("recordings")
        .join(device_path_component(device_id))
        .join("%Y")
        .join("%m")
        .join("%d")
        .join("%H%M%S.mp4")
}

pub fn media_library_root_path(settings: &DvrRecordingSettings) -> PathBuf {
    PathBuf::from(settings.media_library_root.trim())
}

pub fn dvr_media_preview_path(
    settings: &DvrRecordingSettings,
    requested_path: &str,
) -> Result<PathBuf, String> {
    let settings = sanitize_dvr_recording_settings(settings.clone());
    let requested = PathBuf::from(requested_path.trim());
    if !requested.is_absolute() {
        return Err("DVR media preview path must be absolute".to_string());
    }
    let requested = requested
        .canonicalize()
        .map_err(|_| format!("DVR media preview file not found: {}", requested.display()))?;
    let root = media_library_root_path(&settings)
        .canonicalize()
        .map_err(|_| "DVR media library root is not available".to_string())?;
    if !path_is_same_or_inside_path(&requested, &root) {
        return Err(format!(
            "DVR media preview path is outside the media library: {}",
            requested.display()
        ));
    }
    if requested.is_dir() || !requested.is_file() {
        return Err(format!(
            "DVR media preview file not found: {}",
            requested.display()
        ));
    }
    if is_recording_path(&requested) {
        let size_bytes = fs::metadata(&requested)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if !recording_playback_info(&requested, size_bytes, 1).playable {
            return Err(format!(
                "DVR recording is not playable: {}",
                requested.display()
            ));
        }
        return Ok(requested);
    }
    if is_snapshot_path(&requested) {
        return Ok(requested);
    }
    Err(format!(
        "DVR media preview supports recordings and snapshots only: {}",
        requested.display()
    ))
}

fn prepare_segment_calendar_dirs(
    recording_root: &Path,
    device_id: &str,
    start_secs: u64,
    settings: &DvrRecordingSettings,
) -> std::io::Result<()> {
    let days_to_prepare = settings.retention_days.min(365).saturating_add(1);
    for day_offset in 0..=days_to_prepare {
        let segment_path = recording_segment_path(
            recording_root,
            device_id,
            start_secs.saturating_add(day_offset as u64 * 86_400),
        );
        if let Some(parent) = segment_path.parent() {
            fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}

fn snapshot_media_path(media_root: &Path, device_id: &str, unix_secs: u64) -> PathBuf {
    let (year, month, day, hour, minute, second) = unix_to_utc_parts(unix_secs);
    media_root
        .join("snapshots")
        .join(device_path_component(device_id))
        .join(format!("{year:04}"))
        .join(format!("{month:02}"))
        .join(format!("{day:02}"))
        .join(format!("{hour:02}{minute:02}{second:02}.jpg"))
}

fn device_id_from_media_path(
    media_root: &Path,
    media_kind_dir: &str,
    path: &Path,
) -> Option<String> {
    let root = media_root.join(media_kind_dir);
    let relative = path.strip_prefix(root).ok()?;
    relative
        .components()
        .next()
        .and_then(|component| component.as_os_str().to_str())
        .map(|value| value.to_string())
}

fn is_recording_path(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("mp4"))
}

fn is_snapshot_path(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "jpg" | "jpeg" | "png" | "webp"
            )
        })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlaybackInfo {
    playable: bool,
    duration_seconds: Option<u32>,
}

fn recording_playback_info(
    path: &Path,
    size_bytes: u64,
    fallback_duration_seconds: u32,
) -> PlaybackInfo {
    if size_bytes < MIN_PLAYABLE_MP4_BYTES {
        return PlaybackInfo {
            playable: false,
            duration_seconds: None,
        };
    }
    if let Some(duration_seconds) = ffprobe_video_duration_seconds(path) {
        return PlaybackInfo {
            playable: true,
            duration_seconds: Some(duration_seconds.max(1)),
        };
    }
    PlaybackInfo {
        playable: true,
        duration_seconds: Some(fallback_duration_seconds.max(1)),
    }
}

fn ffprobe_video_duration_seconds(path: &Path) -> Option<u32> {
    let ffprobe = resolve_ffprobe_bin()?;
    let output = Command::new(ffprobe)
        .arg("-v")
        .arg("error")
        .arg("-select_streams")
        .arg("v:0")
        .arg("-show_entries")
        .arg("format=duration")
        .arg("-of")
        .arg("default=nokey=1:noprint_wrappers=1")
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let seconds = text
        .lines()
        .find_map(|line| line.trim().parse::<f64>().ok())?;
    if !seconds.is_finite() || seconds <= 0.0 {
        return None;
    }
    Some(seconds.ceil() as u32)
}

fn gracefully_stop_ffmpeg(mut child: Child) -> std::io::Result<()> {
    if let Some(stdin) = child.stdin.as_mut() {
        let _ = stdin.write_all(b"q\n");
        let _ = stdin.flush();
    }
    let started = SystemTime::now();
    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        let elapsed = started.elapsed().unwrap_or_default().as_millis() as u64;
        if elapsed >= FFMPEG_GRACEFUL_STOP_TIMEOUT_MS {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn process_status_label(status: std::process::ExitStatus) -> String {
    status
        .code()
        .map(|value| value.to_string())
        .unwrap_or_else(|| "signal".to_string())
}

fn path_is_same_or_inside_path(candidate: &Path, root: &Path) -> bool {
    candidate == root || candidate.starts_with(root)
}

fn segment_timestamp_from_path(path: &Path) -> Option<u64> {
    let file_stem = path.file_stem()?.to_str()?;
    if file_stem.len() != 6 || !file_stem.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let second = file_stem[4..6].parse::<u32>().ok()?;
    let minute = file_stem[2..4].parse::<u32>().ok()?;
    let hour = file_stem[0..2].parse::<u32>().ok()?;
    let day = path.parent()?.file_name()?.to_str()?.parse::<u32>().ok()?;
    let month = path
        .parent()?
        .parent()?
        .file_name()?
        .to_str()?
        .parse::<u32>()
        .ok()?;
    let year = path
        .parent()?
        .parent()?
        .parent()?
        .file_name()?
        .to_str()?
        .parse::<i32>()
        .ok()?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let days = days_from_civil(year, month, day);
    if days < 0 {
        return None;
    }
    Some(days as u64 * 86_400 + hour as u64 * 3_600 + minute as u64 * 60 + second as u64)
}

fn file_modified_secs(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

fn sidecar_candidates(video_path: &Path) -> Vec<PathBuf> {
    ["json", "txt", "md", "csv", "yaml", "yml"]
        .iter()
        .map(|extension| video_path.with_extension(extension))
        .collect()
}

fn remove_file_if_exists(path: &Path, removed: &mut usize) -> Result<(), String> {
    if path.exists() {
        fs::remove_file(path)
            .map_err(|error| format!("failed to remove DVR file {}: {error}", path.display()))?;
        *removed += 1;
    }
    Ok(())
}

fn remove_dir_if_exists(path: &Path, removed: &mut usize) -> Result<(), String> {
    if path.exists() {
        fs::remove_dir_all(path).map_err(|error| {
            format!(
                "failed to remove DVR derived directory {}: {error}",
                path.display()
            )
        })?;
        *removed += 1;
    }
    Ok(())
}

fn path_can_accept_write(path: &Path) -> bool {
    if fs::create_dir_all(path).is_err() {
        return false;
    }
    let probe = path.join(".harborbeacon-dvr-write-probe");
    match fs::write(&probe, b"probe") {
        Ok(()) => {
            let _ = fs::remove_file(probe);
            true
        }
        Err(_) => false,
    }
}

fn replace_rtsp_path(stream_url: &str, path_hint: &str) -> Option<String> {
    let mut url = Url::parse(stream_url).ok()?;
    if !matches!(url.scheme(), "rtsp" | "rtsps") {
        return None;
    }
    url.set_path(path_hint);
    Some(url.to_string())
}

fn rtsp_url_uses_low_bitrate_stream(stream_url: &str) -> bool {
    let Ok(url) = Url::parse(stream_url) else {
        return false;
    };
    rtsp_path_uses_low_bitrate_stream(url.path())
}

fn rtsp_path_uses_low_bitrate_stream(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains("sub")
        || lower.contains("stream2")
        || lower.contains("minor")
        || lower.contains("low")
        || lower.contains("secondary")
}

fn normalize_rtsp_path_hint(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return fallback.to_string();
    }
    if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

fn dedupe_non_empty(values: Vec<String>) -> Vec<String> {
    let mut output = Vec::new();
    let mut seen = HashSet::new();
    for value in values {
        let trimmed = value.trim().to_string();
        if !trimmed.is_empty() && seen.insert(trimmed.clone()) {
            output.push(trimmed);
        }
    }
    output
}

fn device_path_component(device_id: &str) -> String {
    let mut output = String::new();
    for ch in device_id.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            output.push(ch);
        } else {
            output.push('_');
        }
    }
    if output.is_empty() {
        "camera".to_string()
    } else {
        output
    }
}

fn bytes_to_decimal_gb(bytes: u64) -> u64 {
    (bytes + 999_999_999) / 1_000_000_000
}

fn url_encode_path_segment(value: &str) -> String {
    url_encode_query_component(value)
}

fn url_encode_query_component(value: &str) -> String {
    let mut output = String::new();
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            output.push(*byte as char);
        } else {
            output.push_str(&format!("%{byte:02X}"));
        }
    }
    output
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn unix_to_utc_parts(unix_secs: u64) -> (i32, u32, u32, u32, u32, u32) {
    let days = (unix_secs / 86_400) as i64;
    let day_secs = unix_secs % 86_400;
    let (year, month, day) = civil_from_days(days);
    let hour = (day_secs / 3_600) as u32;
    let minute = ((day_secs % 3_600) / 60) as u32;
    let second = (day_secs % 60) as u32;
    (year, month, day, hour, minute, second)
}

fn civil_from_days(days_since_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = y + i64::from(month <= 2);
    (year as i32, month as u32, day as u32)
}

fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let mut y = year as i64;
    let m = month as i64;
    y -= i64::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
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

    fn unique_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "{label}-{}-{}",
            std::process::id(),
            now_unix_secs()
        ))
    }

    #[test]
    fn recording_path_uses_device_and_utc_calendar_segments() {
        let root = PathBuf::from("/tmp/harbor-dvr");
        let path = recording_segment_path(&root, "camera/main", 1_704_067_205);
        assert!(path.ends_with(
            Path::new("recordings")
                .join("camera_main")
                .join("2024")
                .join("01")
                .join("01")
                .join("000005.mp4")
        ));
        assert_eq!(segment_timestamp_from_path(&path), Some(1_704_067_205));
    }

    #[test]
    fn capacity_estimate_matches_low_bitrate_weekly_budget() {
        let settings = DvrRecordingSettings {
            continuous_bitrate_mbps: 2,
            enabled_device_ids: vec!["camera-main".to_string()],
            ..Default::default()
        };
        let estimate = dvr_capacity_estimate(&settings, 1);
        assert_eq!(estimate.retention_days, 7);
        assert_eq!(
            bytes_to_decimal_gb(estimate.estimated_bytes_per_camera),
            152
        );
    }

    #[test]
    fn prepare_segment_calendar_dirs_creates_retention_window_parents() {
        let root = unique_dir("harborbeacon-dvr-calendar-dirs");
        let settings = DvrRecordingSettings {
            recording_root: root.to_string_lossy().into_owned(),
            retention_days: 2,
            ..Default::default()
        };
        let start = 1_704_067_205;

        prepare_segment_calendar_dirs(&root, "camera-main", start, &settings)
            .expect("prepare dirs");

        for offset in 0..=3 {
            let path = recording_segment_path(&root, "camera-main", start + offset * 86_400);
            assert!(path.parent().expect("parent").is_dir());
        }

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn sanitize_settings_clamps_retention_segments_and_dedupes_devices() {
        let settings = sanitize_dvr_recording_settings(DvrRecordingSettings {
            recording_root: " ".to_string(),
            media_library_root: " ".to_string(),
            retention_days: 0,
            segment_seconds: 12,
            continuous_bitrate_mbps: 0,
            continuous_stream_path_hint: "stream-low".to_string(),
            enabled_device_ids: vec![
                " camera-main ".to_string(),
                "camera-main".to_string(),
                " ".to_string(),
            ],
            ..Default::default()
        });
        assert!(!settings.recording_root.trim().is_empty());
        assert!(Path::new(&settings.media_library_root)
            .ends_with(Path::new("camera-dvr").join("library")));
        assert_eq!(settings.retention_days, 1);
        assert_eq!(settings.segment_seconds, 30);
        assert_eq!(settings.continuous_bitrate_mbps, 1);
        assert_eq!(settings.continuous_stream_path_hint, "/stream-low");
        assert_eq!(settings.enabled_device_ids, vec!["camera-main"]);
    }

    #[test]
    fn timeline_scan_writes_video_sidecar_for_rag() {
        let root = unique_dir("harborbeacon-dvr-timeline");
        let media_root = root.join("library");
        let settings = DvrRecordingSettings {
            recording_root: root.to_string_lossy().into_owned(),
            media_library_root: media_root.to_string_lossy().into_owned(),
            segment_seconds: 300,
            ..Default::default()
        };
        let video_path = recording_segment_path(&media_root, "camera-main", 1_704_067_205);
        fs::create_dir_all(video_path.parent().expect("parent")).expect("create dir");
        let mut fake_video = b"ftyp".to_vec();
        fake_video.resize((MIN_PLAYABLE_MP4_BYTES + 1) as usize, 0);
        fs::write(&video_path, fake_video).expect("write video");
        let mut device = CameraDevice::new("camera-main", "Front Door", "rtsp://host/stream1");
        device.room = Some("Door".to_string());

        let response = scan_timeline(&settings, &[device], None, None, None, None).expect("scan");

        assert_eq!(response.segments.len(), 1);
        assert_eq!(response.segments[0].device_id, "camera-main");
        assert_eq!(response.media_library_root, media_root.to_string_lossy());
        assert_eq!(response.segments[0].media_kind, "recording");
        assert!(response.segments[0].playable);
        let sidecar = video_path.with_extension("json");
        let text = fs::read_to_string(sidecar).expect("sidecar");
        assert!(text.contains("multimodal_rag_vlm"));
        assert!(text.contains("analysis_pending"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn timeline_scan_hides_unplayable_tiny_recordings_and_mixes_snapshots() {
        let root = unique_dir("harborbeacon-dvr-media-library");
        let media_root = root.join("library");
        let settings = DvrRecordingSettings {
            recording_root: root.to_string_lossy().into_owned(),
            media_library_root: media_root.to_string_lossy().into_owned(),
            segment_seconds: 30,
            ..Default::default()
        };
        let bad_video_path = recording_segment_path(&media_root, "camera-main", 1_704_067_205);
        fs::create_dir_all(bad_video_path.parent().expect("parent")).expect("create video dir");
        fs::write(&bad_video_path, vec![0_u8; 48]).expect("write bad video");
        let snapshot_path = snapshot_media_path(&media_root, "camera-main", 1_704_067_206);
        fs::create_dir_all(snapshot_path.parent().expect("parent")).expect("create snapshot dir");
        fs::write(&snapshot_path, [0xFF, 0xD8, 0xFF, 0xD9]).expect("write snapshot");
        let device = CameraDevice::new("camera-main", "Front Door", "rtsp://host/stream1");

        let response = scan_timeline(&settings, &[device], None, None, None, None).expect("scan");

        assert_eq!(response.segments.len(), 1);
        assert_eq!(response.segments[0].media_kind, "snapshot");
        assert_eq!(
            response.segments[0].file_path,
            snapshot_path.to_string_lossy()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn recording_stream_url_prefers_substream_path_without_losing_auth() {
        let device = CameraDevice::new(
            "camera-main",
            "Main",
            "rtsp://user:pass@192.168.3.231:554/stream1",
        );
        let settings = DvrRecordingSettings::default();
        assert_eq!(
            recording_stream_url(&device, &settings),
            "rtsp://user:pass@192.168.3.231:554/stream2"
        );
    }

    #[test]
    fn recording_stream_url_keeps_selected_substream_path() {
        let device = CameraDevice::new(
            "camera-main",
            "Main",
            "rtsp://user:pass@192.168.3.252:554/h264/ch1/sub/av_stream",
        );
        let settings = DvrRecordingSettings::default();

        assert_eq!(
            recording_stream_url(&device, &settings),
            "rtsp://user:pass@192.168.3.252:554/h264/ch1/sub/av_stream"
        );
    }
}
