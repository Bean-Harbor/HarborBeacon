use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Read};
use std::path::Path;
use std::time::Duration;
use uuid::Uuid;

use crate::runtime::registry::CameraCapabilities;

const DEFAULT_HARBORLINK_MEDIA_API_URL: &str = "http://127.0.0.1:8790";
const DEFAULT_HARBORLINK_LOCAL_API_TOKEN_FILE: &str =
    "/run/credentials/harboros-beacon.service/harborlink-local-api-token";
const HARBORLINK_CONTRACT_VERSION: &str = "1.0";
const HARBORLINK_CUTOVER_MODE: &str = "harborlink";
const DETECTION_LEASE_START_TIMEOUT_SECONDS: u64 = 45;

fn detection_lease_start_timeout() -> Duration {
    Duration::from_secs(DETECTION_LEASE_START_TIMEOUT_SECONDS)
}

thread_local! {
    static HARBORLINK_BUSINESS_REQUEST_ID: RefCell<Option<String>> = const { RefCell::new(None) };
}

pub struct HarborLinkRequestScope {
    previous: Option<String>,
}

impl Drop for HarborLinkRequestScope {
    fn drop(&mut self) {
        HARBORLINK_BUSINESS_REQUEST_ID.with(|request_id| {
            *request_id.borrow_mut() = self.previous.take();
        });
    }
}

pub fn harborlink_request_scope(request_id: Option<&str>) -> HarborLinkRequestScope {
    let request_id = normalize_business_request_id(request_id);
    let previous =
        HARBORLINK_BUSINESS_REQUEST_ID.with(|current| current.borrow_mut().replace(request_id));
    HarborLinkRequestScope { previous }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HarborLinkContractError {
    pub status_code: u16,
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub dependency: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct HarborLinkMediaClient {
    base_url: String,
    local_api_token: Option<String>,
    http: Client,
}

trait HarborLinkRequestBuilderExt {
    fn send_harborlink(self) -> Result<Response, reqwest::Error>;
}

impl HarborLinkRequestBuilderExt for RequestBuilder {
    fn send_harborlink(self) -> Result<Response, reqwest::Error> {
        let mutation = self
            .try_clone()
            .and_then(|request| request.build().ok())
            .is_some_and(|request| request.headers().contains_key("X-Request-Id"));
        let retry = mutation.then(|| self.try_clone()).flatten();
        match self.send() {
            Err(error) if error.is_connect() || error.is_timeout() => {
                if let Some(retry) = retry {
                    retry.send()
                } else {
                    Err(error)
                }
            }
            result => result,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct StartLiveSessionRequest<'a> {
    stream_profile: &'a str,
    ttl_seconds: u64,
}

#[derive(Debug, Clone, Serialize)]
struct StartDetectionLeaseRequest<'a> {
    stream_profile: &'a str,
    ttl_seconds: u64,
    pre_roll_seconds: u32,
}

#[derive(Debug, Clone, Serialize)]
struct StartRecordingRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_profile: Option<&'a str>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct StartEventRecordingRequest<'a> {
    event_id: &'a str,
    owner: &'a str,
    labels: &'a [&'a str],
    stream_profile: &'a str,
    ttl_seconds: u64,
    pre_roll_seconds: u32,
    trigger_epoch_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    required_end_epoch_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HarborLinkLiveSession {
    pub camera_id: String,
    pub session_id: Option<String>,
    pub status: String,
    pub transport: String,
    pub stream_profile: String,
    pub webrtc_url: Option<String>,
    pub hls_url: Option<String>,
    pub started_at: Option<String>,
    pub updated_at: String,
    pub expires_at: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HarborLinkDetectionLease {
    pub camera_id: String,
    pub lease_id: String,
    pub status: String,
    pub stream_profile: String,
    pub local_rtsp_url: Option<String>,
    pub started_at: String,
    pub updated_at: String,
    pub expires_at: String,
    #[serde(default)]
    pub pre_roll_seconds: u32,
    #[serde(default)]
    pub pre_roll_ready: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HarborLinkRecordingStatus {
    pub device_id: String,
    pub status: String,
    pub started_at: Option<String>,
    pub updated_at: Option<String>,
    pub stream_kind: String,
    pub last_segment_path: Option<String>,
    pub live_mjpeg_url: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarborLinkEventRecordingLease {
    pub camera_id: String,
    pub lease_id: String,
    pub event_id: String,
    pub owner: String,
    pub status: String,
    pub stream_profile: String,
    #[serde(default)]
    pub labels: Vec<String>,
    pub started_at: String,
    pub updated_at: String,
    pub expires_at: String,
    #[serde(default)]
    pub pre_roll_seconds: u32,
    #[serde(default)]
    pub trigger_epoch_ms: u64,
    #[serde(default)]
    pub artifacts: Vec<HarborLinkRecordingArtifact>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarborLinkClipCapture {
    pub camera_id: String,
    #[serde(default)]
    pub clip_path: String,
    #[serde(default)]
    pub keyframe_paths: Vec<String>,
    #[serde(default)]
    pub clip_artifact: Option<HarborLinkRecordingArtifact>,
    #[serde(default)]
    pub keyframe_artifacts: Vec<HarborLinkRecordingArtifact>,
    pub mime_type: String,
    pub byte_size: u64,
    pub captured_at_epoch_ms: u128,
    pub started_at_epoch_ms: u128,
    pub ended_at_epoch_ms: u128,
    pub clip_length_seconds: u32,
    pub keyframe_count: u32,
    pub keyframe_interval_seconds: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarborLinkRecordingArtifact {
    pub media_contract_version: String,
    pub artifact_id: String,
    pub camera_id: String,
    pub kind: String,
    pub mime_type: String,
    pub byte_size: u64,
    #[serde(default)]
    pub started_at_epoch_ms: u128,
    #[serde(default)]
    pub ended_at_epoch_ms: u128,
    #[serde(default)]
    pub duration_seconds: u32,
    #[serde(default)]
    pub stream_kind: String,
    pub modified_at_epoch_ms: u128,
    pub preview_url: String,
    #[serde(default)]
    pub event_id: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub coverage_start_epoch_ms: Option<u64>,
    #[serde(default)]
    pub coverage_end_epoch_ms: Option<u64>,
    #[serde(default)]
    pub coverage_verified: bool,
    #[serde(default)]
    pub gap_free: bool,
    #[serde(default)]
    pub coverage_segment_duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarborLinkCredentialStatus {
    pub camera_id: String,
    pub configured: bool,
    pub username_configured: bool,
    pub password_configured: bool,
    pub rtsp_port_configured: bool,
    pub rtsp_path_count: usize,
    pub last_verified_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarborLinkHomeAssistantStatus {
    pub enabled: bool,
    pub configured: bool,
    pub base_url_configured: bool,
    pub token_configured: bool,
    pub allowed_entity_count: usize,
    pub allowed_camera_count: usize,
    pub allowed_entities: Vec<String>,
    pub allowed_cameras: Vec<String>,
    pub camera_entity_bindings: BTreeMap<String, String>,
    pub exposed_domains: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarborLinkCameraRegistration {
    pub registered: bool,
    pub camera: Option<Value>,
    pub check: HarborLinkRtspCheck,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarborLinkDiscoverySettings {
    pub network_cidr: String,
    pub protocol: String,
    pub rtsp_port: Option<u16>,
    pub rtsp_paths: Vec<String>,
    pub rtsp_path_count: usize,
    pub username_configured: bool,
    pub password_configured: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarborLinkRtspCheck {
    pub camera_id: String,
    pub reachable: bool,
    pub transport: String,
    pub requires_auth: bool,
    pub capabilities: CameraCapabilities,
    pub error_message: Option<String>,
    pub checked_at: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarborLinkDiscoveredCamera {
    pub candidate_id: String,
    pub camera_id: String,
    pub display_name: String,
    pub ip_address: String,
    pub port: u16,
    pub protocol: String,
    pub reachable: bool,
    pub registered: bool,
    pub requires_auth: bool,
    pub vendor: Option<String>,
    pub model: Option<String>,
    pub rtsp_paths: Vec<String>,
    pub rtsp_path_count: usize,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HarborLinkDiscoveryResponse {
    pub scanned_hosts: usize,
    pub cameras: Vec<HarborLinkDiscoveredCamera>,
}

pub struct HarborLinkMjpegStream {
    response: Response,
}

impl Read for HarborLinkMjpegStream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.response.read(buffer)
    }
}

impl HarborLinkMediaClient {
    pub fn from_env() -> Result<Self, String> {
        require_harborlink_cutover()?;
        let base_url = std::env::var("HARBORLINK_MEDIA_API_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_HARBORLINK_MEDIA_API_URL.to_string());
        let mut client = Self::new(base_url)?;
        client.local_api_token = read_local_api_token_from_env()?;
        Ok(client)
    }

    pub fn new(base_url: impl Into<String>) -> Result<Self, String> {
        let base_url = base_url.into().trim().trim_end_matches('/').to_string();
        if !(base_url.starts_with("http://") || base_url.starts_with("https://")) {
            return Err("HARBORLINK_MEDIA_API_URL must use http or https".to_string());
        }

        let http = Client::builder()
            .connect_timeout(Duration::from_secs(1))
            .build()
            .map_err(|error| format!("failed to create HarborLink media client: {error}"))?;
        Ok(Self {
            base_url,
            local_api_token: None,
            http,
        })
    }

    pub fn readyz(&self) -> Result<Value, String> {
        let response = self
            .request(
                reqwest::Method::GET,
                format!("{}/readyz", self.base_url),
                false,
            )
            .timeout(Duration::from_secs(3))
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_json_response(response, "HarborLink readiness")
    }

    pub fn capabilities(&self) -> Result<Value, String> {
        let response = self
            .request(
                reqwest::Method::GET,
                format!("{}/v1/capabilities", self.base_url),
                false,
            )
            .timeout(Duration::from_secs(3))
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_json_response(response, "HarborLink capabilities")
    }

    pub fn start_live_session(
        &self,
        camera_id: &str,
        stream_profile: &str,
        ttl_seconds: u64,
    ) -> Result<HarborLinkLiveSession, String> {
        let endpoint = self.live_session_collection_endpoint(camera_id);
        let response = self
            .request(reqwest::Method::POST, endpoint, true)
            .timeout(Duration::from_secs(4))
            .json(&StartLiveSessionRequest {
                stream_profile,
                ttl_seconds,
            })
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_session_response(response)
    }

    pub fn live_session_status(
        &self,
        camera_id: &str,
        session_id: &str,
    ) -> Result<HarborLinkLiveSession, String> {
        let response = self
            .request(
                reqwest::Method::GET,
                self.live_session_endpoint(camera_id, session_id),
                false,
            )
            .timeout(Duration::from_secs(4))
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_session_response(response)
    }

    pub fn stop_live_session(
        &self,
        camera_id: &str,
        session_id: Option<&str>,
    ) -> Result<HarborLinkLiveSession, String> {
        let session_id = session_id
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("current");
        let response = self
            .request(
                reqwest::Method::DELETE,
                self.live_session_endpoint(camera_id, session_id),
                true,
            )
            .timeout(Duration::from_secs(4))
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_session_response(response)
    }

    pub fn renew_live_session(
        &self,
        camera_id: &str,
        session_id: &str,
        ttl_seconds: u64,
    ) -> Result<HarborLinkLiveSession, String> {
        let response = self
            .request(
                reqwest::Method::POST,
                format!(
                    "{}/{}/renew",
                    self.live_session_collection_endpoint(camera_id),
                    encode_path_segment(session_id)
                ),
                true,
            )
            .timeout(Duration::from_secs(4))
            .json(&json!({ "ttl_seconds": ttl_seconds }))
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_session_response(response)
    }

    pub fn start_detection_lease(
        &self,
        camera_id: &str,
        stream_profile: &str,
        ttl_seconds: u64,
        pre_roll_seconds: u32,
    ) -> Result<HarborLinkDetectionLease, String> {
        let response = self
            .request(
                reqwest::Method::POST,
                self.detection_lease_collection_endpoint(camera_id),
                true,
            )
            .timeout(detection_lease_start_timeout())
            .json(&StartDetectionLeaseRequest {
                stream_profile,
                ttl_seconds,
                pre_roll_seconds,
            })
            .send_harborlink()
            .map_err(unavailable_error)?;
        let lease: HarborLinkDetectionLease = decode_json_response(response, "detection lease")?;
        if pre_roll_seconds > 0
            && (!lease.pre_roll_ready || lease.pre_roll_seconds < pre_roll_seconds)
        {
            return Err("HarborLink did not prepare the requested detection pre-roll".to_string());
        }
        Ok(lease)
    }

    pub fn detection_lease_status(
        &self,
        camera_id: &str,
        lease_id: &str,
    ) -> Result<HarborLinkDetectionLease, String> {
        let response = self
            .request(
                reqwest::Method::GET,
                self.detection_lease_endpoint(camera_id, lease_id),
                false,
            )
            .timeout(Duration::from_secs(4))
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_json_response(response, "detection lease")
    }

    pub fn renew_detection_lease(
        &self,
        camera_id: &str,
        lease_id: &str,
        ttl_seconds: u64,
    ) -> Result<HarborLinkDetectionLease, String> {
        let response = self
            .request(
                reqwest::Method::POST,
                format!(
                    "{}/renew",
                    self.detection_lease_endpoint(camera_id, lease_id)
                ),
                true,
            )
            .timeout(Duration::from_secs(4))
            .json(&json!({ "ttl_seconds": ttl_seconds }))
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_json_response(response, "detection lease")
    }

    pub fn stop_detection_lease(
        &self,
        camera_id: &str,
        lease_id: &str,
    ) -> Result<HarborLinkDetectionLease, String> {
        let response = self
            .request(
                reqwest::Method::DELETE,
                self.detection_lease_endpoint(camera_id, lease_id),
                true,
            )
            .timeout(Duration::from_secs(4))
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_json_response(response, "detection lease")
    }

    pub fn capture_snapshot(&self, camera_id: &str) -> Result<Vec<u8>, String> {
        let response = self
            .request(
                reqwest::Method::GET,
                format!(
                    "{}/v1/cameras/{}/snapshot.jpg",
                    self.base_url,
                    encode_path_segment(camera_id)
                ),
                false,
            )
            .timeout(Duration::from_secs(20))
            .send_harborlink()
            .map_err(unavailable_error)?;
        if !response.status().is_success() {
            return Err(redacted_media_error(response.status(), "camera snapshot"));
        }
        let bytes = response
            .bytes()
            .map_err(|error| format!("HarborLink returned an invalid snapshot: {error}"))?;
        if bytes.len() < 3 || bytes[..3] != [0xff, 0xd8, 0xff] {
            return Err("HarborLink returned an invalid JPEG snapshot".to_string());
        }
        Ok(bytes.to_vec())
    }

    pub fn archive_snapshot(&self, camera_id: &str) -> Result<HarborLinkRecordingArtifact, String> {
        let response = self
            .request(
                reqwest::Method::POST,
                format!(
                    "{}/v1/cameras/{}/snapshot.jpg",
                    self.base_url,
                    encode_path_segment(camera_id)
                ),
                true,
            )
            .timeout(Duration::from_secs(20))
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_json_response(response, "camera snapshot archive")
    }

    pub fn open_mjpeg(&self, camera_id: &str) -> Result<HarborLinkMjpegStream, String> {
        let response = self
            .request(
                reqwest::Method::GET,
                format!(
                    "{}/v1/cameras/{}/live.mjpeg",
                    self.base_url,
                    encode_path_segment(camera_id)
                ),
                false,
            )
            .send_harborlink()
            .map_err(unavailable_error)?;
        if !response.status().is_success() {
            return Err(redacted_media_error(
                response.status(),
                "camera MJPEG stream",
            ));
        }
        Ok(HarborLinkMjpegStream { response })
    }

    pub fn start_recording(
        &self,
        camera_id: &str,
        stream_profile: Option<&str>,
    ) -> Result<HarborLinkRecordingStatus, String> {
        self.recording_request(camera_id, reqwest::Method::POST, stream_profile)
    }

    pub fn recording_status(&self, camera_id: &str) -> Result<HarborLinkRecordingStatus, String> {
        self.recording_request(camera_id, reqwest::Method::GET, None)
    }

    pub fn stop_recording(&self, camera_id: &str) -> Result<HarborLinkRecordingStatus, String> {
        self.recording_request(camera_id, reqwest::Method::DELETE, None)
    }

    pub fn start_event_recording(
        &self,
        camera_id: &str,
        event_id: &str,
        stream_profile: &str,
        ttl_seconds: u64,
        pre_roll_seconds: u32,
        trigger_epoch_ms: u64,
    ) -> Result<HarborLinkEventRecordingLease, String> {
        self.start_event_recording_with_labels(
            camera_id,
            event_id,
            &["cat"],
            stream_profile,
            ttl_seconds,
            pre_roll_seconds,
            trigger_epoch_ms,
            None,
        )
    }

    pub fn start_package_event_recording(
        &self,
        camera_id: &str,
        event_id: &str,
        stream_profile: &str,
        ttl_seconds: u64,
        pre_roll_seconds: u32,
        trigger_epoch_ms: u64,
    ) -> Result<HarborLinkEventRecordingLease, String> {
        self.start_event_recording_with_labels(
            camera_id,
            event_id,
            &["package", "package_no_longer_visible"],
            stream_profile,
            ttl_seconds,
            pre_roll_seconds,
            trigger_epoch_ms,
            Some(trigger_epoch_ms.saturating_add(15_000)),
        )
    }

    pub fn start_package_appearance_event_recording(
        &self,
        camera_id: &str,
        event_id: &str,
        stream_profile: &str,
        ttl_seconds: u64,
        pre_roll_seconds: u32,
        trigger_epoch_ms: u64,
    ) -> Result<HarborLinkEventRecordingLease, String> {
        self.start_event_recording_with_labels(
            camera_id,
            event_id,
            &["package", "package_appeared"],
            stream_profile,
            ttl_seconds,
            pre_roll_seconds,
            trigger_epoch_ms,
            Some(trigger_epoch_ms.saturating_add(10_000)),
        )
    }

    fn start_event_recording_with_labels(
        &self,
        camera_id: &str,
        event_id: &str,
        labels: &[&str],
        stream_profile: &str,
        ttl_seconds: u64,
        pre_roll_seconds: u32,
        trigger_epoch_ms: u64,
        required_end_epoch_ms: Option<u64>,
    ) -> Result<HarborLinkEventRecordingLease, String> {
        let response = self
            .request(
                reqwest::Method::POST,
                self.event_recording_endpoint(camera_id, "current"),
                true,
            )
            .timeout(Duration::from_secs(10))
            .json(&StartEventRecordingRequest {
                event_id,
                owner: "harborbeacon",
                labels,
                stream_profile,
                ttl_seconds,
                pre_roll_seconds,
                trigger_epoch_ms,
                required_end_epoch_ms,
            })
            .send_harborlink()
            .map_err(unavailable_error)?;
        let lease: HarborLinkEventRecordingLease =
            decode_json_response(response, "event recording")?;
        if pre_roll_seconds > 0
            && (lease.pre_roll_seconds < pre_roll_seconds
                || lease.trigger_epoch_ms != trigger_epoch_ms)
        {
            return Err("HarborLink did not honor the requested event pre-roll".to_string());
        }
        Ok(lease)
    }

    pub fn event_recording_status(
        &self,
        camera_id: &str,
    ) -> Result<HarborLinkEventRecordingLease, String> {
        let response = self
            .request(
                reqwest::Method::GET,
                self.event_recording_endpoint(camera_id, "current"),
                false,
            )
            .timeout(Duration::from_secs(4))
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_json_response(response, "event recording")
    }

    pub fn event_recording_lease_status(
        &self,
        camera_id: &str,
        lease_id: &str,
    ) -> Result<HarborLinkEventRecordingLease, String> {
        let response = self
            .request(
                reqwest::Method::GET,
                self.event_recording_endpoint(camera_id, lease_id),
                false,
            )
            .timeout(Duration::from_secs(4))
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_json_response(response, "event recording")
    }

    pub fn renew_event_recording(
        &self,
        camera_id: &str,
        lease_id: &str,
        ttl_seconds: u64,
    ) -> Result<HarborLinkEventRecordingLease, String> {
        let response = self
            .request(
                reqwest::Method::POST,
                format!(
                    "{}/renew",
                    self.event_recording_endpoint(camera_id, lease_id)
                ),
                true,
            )
            .timeout(Duration::from_secs(4))
            .json(&json!({ "ttl_seconds": ttl_seconds }))
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_json_response(response, "event recording")
    }

    pub fn stop_event_recording(
        &self,
        camera_id: &str,
        lease_id: &str,
    ) -> Result<HarborLinkEventRecordingLease, String> {
        let response = self
            .request(
                reqwest::Method::DELETE,
                self.event_recording_endpoint(camera_id, lease_id),
                true,
            )
            .timeout(Duration::from_secs(10))
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_json_response(response, "event recording")
    }

    pub fn capture_clip(
        &self,
        camera_id: &str,
        clip_length_seconds: u32,
        keyframe_count: Option<u32>,
        keyframe_interval_seconds: Option<u32>,
    ) -> Result<HarborLinkClipCapture, String> {
        let response = self
            .request(
                reqwest::Method::POST,
                format!(
                    "{}/v1/cameras/{}/clips",
                    self.base_url,
                    encode_path_segment(camera_id)
                ),
                true,
            )
            .timeout(Duration::from_secs(
                u64::from(clip_length_seconds.clamp(3, 300)) + 45,
            ))
            .json(&json!({
                "clipLengthSeconds": clip_length_seconds,
                "keyframeCount": keyframe_count,
                "keyframeIntervalSeconds": keyframe_interval_seconds,
            }))
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_json_response(response, "camera clip capture")
    }

    fn recording_request(
        &self,
        camera_id: &str,
        method: reqwest::Method,
        stream_profile: Option<&str>,
    ) -> Result<HarborLinkRecordingStatus, String> {
        let request = self
            .request(
                method.clone(),
                format!(
                    "{}/v1/cameras/{}/recordings/current",
                    self.base_url,
                    encode_path_segment(camera_id)
                ),
                method != reqwest::Method::GET,
            )
            .timeout(Duration::from_secs(10));
        let request = if method == reqwest::Method::POST {
            request.json(&StartRecordingRequest { stream_profile })
        } else {
            request
        };
        let response = request.send_harborlink().map_err(unavailable_error)?;
        if !response.status().is_success() {
            return Err(redacted_media_error(response.status(), "camera recording"));
        }
        response
            .json::<HarborLinkRecordingStatus>()
            .map_err(|error| {
                format!("HarborLink returned an invalid camera recording response: {error}")
            })
    }

    fn event_recording_endpoint(&self, camera_id: &str, lease_id: &str) -> String {
        format!(
            "{}/v1/cameras/{}/event-recordings/{}",
            self.base_url,
            encode_path_segment(camera_id),
            encode_path_segment(lease_id)
        )
    }

    pub fn credential_status(&self, camera_id: &str) -> Result<HarborLinkCredentialStatus, String> {
        let response = self
            .request(
                reqwest::Method::GET,
                format!(
                    "{}/v1/cameras/{}/credentials",
                    self.base_url,
                    encode_path_segment(camera_id)
                ),
                false,
            )
            .timeout(Duration::from_secs(4))
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_json_response(response, "camera credential status")
    }

    pub fn save_camera_credential(
        &self,
        camera_id: &str,
        username: Option<&str>,
        password: Option<&str>,
        rtsp_port: Option<u16>,
        rtsp_paths: Option<&[String]>,
    ) -> Result<HarborLinkCredentialStatus, String> {
        let response = self
            .request(
                reqwest::Method::PUT,
                format!(
                    "{}/v1/cameras/{}/credentials",
                    self.base_url,
                    encode_path_segment(camera_id)
                ),
                true,
            )
            .timeout(Duration::from_secs(4))
            .json(&json!({
                "username": username,
                "password": password,
                "rtspPort": rtsp_port,
                "rtspPaths": rtsp_paths,
            }))
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_json_response(response, "camera credential update")
    }

    pub fn check_rtsp(
        &self,
        camera_id: &str,
        username: Option<&str>,
        password: Option<&str>,
        rtsp_port: Option<u16>,
        rtsp_paths: Option<&[String]>,
    ) -> Result<HarborLinkRtspCheck, String> {
        let response = self
            .request(
                reqwest::Method::POST,
                format!(
                    "{}/v1/cameras/{}/rtsp-check",
                    self.base_url,
                    encode_path_segment(camera_id)
                ),
                true,
            )
            .timeout(Duration::from_secs(10))
            .json(&json!({
                "username": username,
                "password": password,
                "rtspPort": rtsp_port,
                "rtspPaths": rtsp_paths,
            }))
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_json_response(response, "camera RTSP check")
    }

    pub fn register_camera(
        &self,
        camera_id: &str,
        registration: &Value,
    ) -> Result<HarborLinkCameraRegistration, String> {
        let response = self
            .request(
                reqwest::Method::POST,
                format!(
                    "{}/v1/cameras/{}/register",
                    self.base_url,
                    encode_path_segment(camera_id)
                ),
                true,
            )
            .timeout(Duration::from_secs(20))
            .json(registration)
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_json_response(response, "camera registration")
    }

    pub fn discover_cameras(
        &self,
        network_cidr: &str,
        protocol: Option<&str>,
        rtsp_port: Option<u16>,
        rtsp_username: Option<&str>,
        rtsp_password: Option<&str>,
        rtsp_paths: &[String],
    ) -> Result<HarborLinkDiscoveryResponse, String> {
        let response = self
            .request(
                reqwest::Method::POST,
                format!("{}/v1/cameras/discover", self.base_url),
                true,
            )
            .timeout(Duration::from_secs(90))
            .json(&json!({
                "networkCidr": network_cidr,
                "protocol": protocol,
                "rtspPort": rtsp_port,
                "rtspUsername": rtsp_username,
                "rtspPassword": rtsp_password,
                "rtspPaths": rtsp_paths,
            }))
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_json_response(response, "camera discovery")
    }

    pub fn discovery_settings(&self) -> Result<HarborLinkDiscoverySettings, String> {
        let response = self
            .request(
                reqwest::Method::GET,
                format!("{}/v1/discovery-settings", self.base_url),
                false,
            )
            .timeout(Duration::from_secs(4))
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_json_response(response, "camera discovery settings")
    }

    pub fn save_discovery_settings(
        &self,
        network_cidr: &str,
        protocol: &str,
        rtsp_port: Option<u16>,
        rtsp_paths: &[String],
        rtsp_username: Option<&str>,
        rtsp_password: Option<&str>,
    ) -> Result<HarborLinkDiscoverySettings, String> {
        let response = self
            .request(
                reqwest::Method::PUT,
                format!("{}/v1/discovery-settings", self.base_url),
                true,
            )
            .timeout(Duration::from_secs(4))
            .json(&json!({
                "networkCidr": network_cidr,
                "protocol": protocol,
                "rtspPort": rtsp_port,
                "rtspPaths": rtsp_paths,
                "rtspUsername": rtsp_username,
                "rtspPassword": rtsp_password,
            }))
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_json_response(response, "camera discovery settings")
    }

    pub fn save_dvr_settings(&self, settings: &Value) -> Result<Value, String> {
        let response = self
            .request(
                reqwest::Method::PUT,
                format!("{}/v1/dvr-settings", self.base_url),
                true,
            )
            .timeout(Duration::from_secs(4))
            .json(settings)
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_json_response(response, "DVR settings update")
    }

    pub fn dvr_settings(&self) -> Result<Value, String> {
        let response = self
            .request(
                reqwest::Method::GET,
                format!("{}/v1/dvr-settings", self.base_url),
                false,
            )
            .timeout(Duration::from_secs(4))
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_json_response(response, "DVR settings")
    }

    pub fn recording_timeline(
        &self,
        camera_id: &str,
    ) -> Result<Vec<HarborLinkRecordingArtifact>, String> {
        let response = self
            .request(
                reqwest::Method::GET,
                format!(
                    "{}/v1/cameras/{}/artifacts",
                    self.base_url,
                    encode_path_segment(camera_id)
                ),
                false,
            )
            .timeout(Duration::from_secs(10))
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_json_response(response, "DVR timeline")
    }

    pub fn delete_dvr_artifact(&self, artifact_id: &str) -> Result<Value, String> {
        let response = self
            .request(
                reqwest::Method::DELETE,
                format!(
                    "{}/v1/dvr/artifacts/{}",
                    self.base_url,
                    encode_path_segment(artifact_id)
                ),
                true,
            )
            .timeout(Duration::from_secs(10))
            .send_harborlink()
            .map_err(unavailable_error)?;
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(json!({
                "artifactId": artifact_id,
                "deleted": true,
                "alreadyAbsent": true
            }));
        }
        decode_json_response(response, "DVR artifact deletion")
    }

    pub fn open_dvr_artifact(
        &self,
        artifact_id: &str,
        range: Option<&str>,
    ) -> Result<Response, String> {
        let mut request = self
            .request(
                reqwest::Method::GET,
                format!(
                    "{}/v1/dvr/artifacts/{}",
                    self.base_url,
                    encode_path_segment(artifact_id)
                ),
                false,
            )
            .timeout(Duration::from_secs(30));
        if let Some(range) = range {
            request = request.header(reqwest::header::RANGE, range);
        }
        let response = request.send_harborlink().map_err(unavailable_error)?;
        if response.status().is_success() {
            Ok(response)
        } else {
            Err(redacted_media_response_error(response, "DVR artifact"))
        }
    }

    pub fn download_dvr_artifact_to(
        &self,
        artifact_id: &str,
        destination: &Path,
        max_bytes: u64,
    ) -> Result<u64, String> {
        if max_bytes == 0 {
            return Err("DVR artifact download limit must be greater than zero".to_string());
        }
        let response = self.open_dvr_artifact(artifact_id, None)?;
        if response
            .content_length()
            .is_some_and(|content_length| content_length > max_bytes)
        {
            return Err(format!(
                "DVR artifact exceeds the validation download limit of {max_bytes} bytes"
            ));
        }
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "failed to create DVR artifact download directory {}: {error}",
                    parent.display()
                )
            })?;
        }
        let temp_path =
            destination.with_extension(format!("download-{}.part", Uuid::new_v4().simple()));
        let result = (|| {
            let mut output = fs::File::create(&temp_path).map_err(|error| {
                format!(
                    "failed to create DVR artifact download {}: {error}",
                    temp_path.display()
                )
            })?;
            let copied = io::copy(&mut response.take(max_bytes.saturating_add(1)), &mut output)
                .map_err(|error| format!("failed to download DVR artifact: {error}"))?;
            if copied > max_bytes {
                return Err(format!(
                    "DVR artifact exceeds the validation download limit of {max_bytes} bytes"
                ));
            }
            output
                .sync_data()
                .map_err(|error| format!("failed to persist DVR artifact download: {error}"))?;
            fs::rename(&temp_path, destination).map_err(|error| {
                format!(
                    "failed to finalize DVR artifact download {}: {error}",
                    destination.display()
                )
            })?;
            Ok(copied)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        result
    }

    pub fn upsert_camera(&self, camera_id: &str, camera: &Value) -> Result<Value, String> {
        let response = self
            .request(
                reqwest::Method::PUT,
                format!(
                    "{}/v1/cameras/{}",
                    self.base_url,
                    encode_path_segment(camera_id)
                ),
                true,
            )
            .timeout(Duration::from_secs(4))
            .json(camera)
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_json_response(response, "camera registry update")
    }

    pub fn update_camera_metadata(&self, camera_id: &str, update: &Value) -> Result<Value, String> {
        let response = self
            .request(
                reqwest::Method::PATCH,
                format!(
                    "{}/v1/cameras/{}/metadata",
                    self.base_url,
                    encode_path_segment(camera_id)
                ),
                true,
            )
            .timeout(Duration::from_secs(4))
            .json(update)
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_json_response(response, "camera metadata update")
    }

    pub fn remove_camera(&self, camera_id: &str) -> Result<Value, String> {
        let response = self
            .request(
                reqwest::Method::DELETE,
                format!(
                    "{}/v1/cameras/{}",
                    self.base_url,
                    encode_path_segment(camera_id)
                ),
                true,
            )
            .timeout(Duration::from_secs(4))
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_json_response(response, "camera registry removal")
    }

    pub fn home_assistant_status(&self) -> Result<HarborLinkHomeAssistantStatus, String> {
        let response = self
            .request(
                reqwest::Method::GET,
                format!("{}/v1/home-assistant", self.base_url),
                false,
            )
            .timeout(Duration::from_secs(4))
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_json_response(response, "Home Assistant status")
    }

    pub fn save_home_assistant(
        &self,
        enabled: bool,
        base_url: Option<&str>,
        access_token: Option<&str>,
        exposed_domains: Option<&[String]>,
        allowed_entities: Option<&[String]>,
        allowed_cameras: Option<&[String]>,
        camera_entity_bindings: Option<&BTreeMap<String, String>>,
        clear_access_token: bool,
    ) -> Result<HarborLinkHomeAssistantStatus, String> {
        let response = self
            .request(
                reqwest::Method::PUT,
                format!("{}/v1/home-assistant", self.base_url),
                true,
            )
            .timeout(Duration::from_secs(4))
            .json(&json!({
                "enabled": enabled,
                "baseUrl": base_url,
                "accessToken": access_token,
                "exposedDomains": exposed_domains,
                "allowedEntities": allowed_entities,
                "allowedCameras": allowed_cameras,
                "cameraEntityBindings": camera_entity_bindings,
                "clearAccessToken": clear_access_token,
            }))
            .send_harborlink()
            .map_err(unavailable_error)?;
        decode_json_response(response, "Home Assistant configuration")
    }

    fn request(&self, method: reqwest::Method, url: String, mutation: bool) -> RequestBuilder {
        let request_id = mutation.then(|| operation_request_id(&method, &url));
        let mut request = self
            .http
            .request(method, url)
            .header("X-HarborLink-Contract-Version", HARBORLINK_CONTRACT_VERSION);
        if let Some(token) = self.local_api_token.as_deref() {
            request = request.bearer_auth(token);
        }
        if let Some(request_id) = request_id {
            request = request.header("X-Request-Id", request_id);
        }
        request
    }

    fn live_session_collection_endpoint(&self, camera_id: &str) -> String {
        format!(
            "{}/v1/cameras/{}/live-sessions",
            self.base_url,
            encode_path_segment(camera_id)
        )
    }

    fn live_session_endpoint(&self, camera_id: &str, session_id: &str) -> String {
        format!(
            "{}/{}",
            self.live_session_collection_endpoint(camera_id),
            encode_path_segment(session_id)
        )
    }

    fn detection_lease_collection_endpoint(&self, camera_id: &str) -> String {
        format!(
            "{}/v1/cameras/{}/detection-leases",
            self.base_url,
            encode_path_segment(camera_id)
        )
    }

    fn detection_lease_endpoint(&self, camera_id: &str, lease_id: &str) -> String {
        format!(
            "{}/{}",
            self.detection_lease_collection_endpoint(camera_id),
            encode_path_segment(lease_id)
        )
    }
}

fn normalize_business_request_id(request_id: Option<&str>) -> String {
    let value = request_id.map(str::trim).unwrap_or_default();
    if !value.is_empty()
        && value.len() <= 80
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        value.to_string()
    } else if value.is_empty() {
        format!("beacon-{}", Uuid::new_v4().simple())
    } else {
        format!("beacon-{}", short_request_hash(value.as_bytes()))
    }
}

fn operation_request_id(method: &reqwest::Method, url: &str) -> String {
    let business_request_id = HARBORLINK_BUSINESS_REQUEST_ID.with(|request_id| {
        request_id
            .borrow()
            .clone()
            .unwrap_or_else(|| normalize_business_request_id(None))
    });
    let operation = format!("{}\n{url}", method.as_str());
    format!(
        "{business_request_id}-{}",
        short_request_hash(operation.as_bytes())
    )
}

fn short_request_hash(value: &[u8]) -> String {
    Sha256::digest(value)[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn read_local_api_token_from_env() -> Result<Option<String>, String> {
    if let Ok(value) = std::env::var("HARBORLINK_LOCAL_API_TOKEN") {
        let value = value.trim().to_string();
        if !value.is_empty() {
            return Ok(Some(value));
        }
    }
    let token_file = std::env::var("HARBORLINK_LOCAL_API_TOKEN_FILE")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_HARBORLINK_LOCAL_API_TOKEN_FILE.to_string());
    match fs::read_to_string(&token_file) {
        Ok(value) => {
            let value = value.trim().to_string();
            if value.is_empty() {
                Err(format!(
                    "HarborLink local API token file {token_file} is empty"
                ))
            } else {
                Ok(Some(value))
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "failed to read HarborLink local API token file {token_file}: {error}"
        )),
    }
}

fn require_harborlink_cutover() -> Result<(), String> {
    let mode = std::env::var("HARBORBEACON_SOUTHBOUND_MODE").unwrap_or_default();
    if mode.trim().eq_ignore_ascii_case(HARBORLINK_CUTOVER_MODE) {
        Ok(())
    } else {
        Err("HARBORBEACON_SOUTHBOUND_MODE must explicitly select harborlink".to_string())
    }
}

fn unavailable_error(error: reqwest::Error) -> String {
    let status = error
        .status()
        .map(|status| status.as_u16())
        .unwrap_or(StatusCode::SERVICE_UNAVAILABLE.as_u16());
    encode_contract_error(HarborLinkContractError {
        status_code: status,
        code: "HARBORLINK_UNAVAILABLE".to_string(),
        message: "HarborLink southbound service is unavailable".to_string(),
        retryable: true,
        dependency: "harborlink".to_string(),
        request_id: None,
    })
}

fn decode_session_response(response: Response) -> Result<HarborLinkLiveSession, String> {
    let status = response.status();
    if status.is_success() {
        return response
            .json::<HarborLinkLiveSession>()
            .map_err(|_| invalid_response_error("live session"));
    }
    Err(redacted_media_response_error(response, "live session"))
}

fn decode_json_response<T: for<'de> Deserialize<'de>>(
    response: Response,
    operation: &str,
) -> Result<T, String> {
    if !response.status().is_success() {
        return Err(redacted_media_response_error(response, operation));
    }
    response
        .json::<T>()
        .map_err(|_| invalid_response_error(operation))
}

fn redacted_media_response_error(response: Response, operation: &str) -> String {
    let status = response.status();
    let response_request_id = response
        .headers()
        .get("X-Request-Id")
        .and_then(|value| value.to_str().ok())
        .map(ToString::to_string);
    if let Ok(value) = response.json::<Value>() {
        if let Some(error) = value.get("error") {
            let code = error
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("HARBORLINK_ERROR");
            let dependency = error
                .get("dependency")
                .and_then(Value::as_str)
                .unwrap_or("harborlink");
            let retryable = error
                .get("retryable")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .map(redact_contract_message)
                .unwrap_or_else(|| format!("HarborLink {operation} failed"));
            let request_id = value
                .get("requestId")
                .and_then(Value::as_str)
                .map(ToString::to_string)
                .or(response_request_id);
            return encode_contract_error(HarborLinkContractError {
                status_code: status.as_u16(),
                code: code.to_string(),
                message,
                retryable,
                dependency: dependency.to_string(),
                request_id,
            });
        }
    }
    encode_contract_error(HarborLinkContractError {
        status_code: status.as_u16(),
        code: "HARBORLINK_ERROR".to_string(),
        message: redacted_media_error(status, operation),
        retryable: status.is_server_error(),
        dependency: "harborlink".to_string(),
        request_id: response_request_id,
    })
}

fn invalid_response_error(operation: &str) -> String {
    encode_contract_error(HarborLinkContractError {
        status_code: StatusCode::BAD_GATEWAY.as_u16(),
        code: "HARBORLINK_INVALID_RESPONSE".to_string(),
        message: format!("HarborLink returned an invalid {operation} response"),
        retryable: false,
        dependency: "harborlink".to_string(),
        request_id: None,
    })
}

fn encode_contract_error(error: HarborLinkContractError) -> String {
    serde_json::to_string(&error).expect("HarborLink contract error serialization must succeed")
}

fn redact_contract_message(value: &str) -> String {
    value.replace(['\r', '\n'], " ").chars().take(512).collect()
}

fn redacted_media_error(status: StatusCode, operation: &str) -> String {
    match status {
        StatusCode::NOT_FOUND => format!("HarborLink {operation} target was not found"),
        StatusCode::SERVICE_UNAVAILABLE => {
            format!("HarborLink southbound media service is unavailable for {operation}")
        }
        _ => format!("HarborLink {operation} failed (HTTP {})", status.as_u16()),
    }
}

fn encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(*byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::{
        detection_lease_start_timeout, encode_path_segment, harborlink_request_scope,
        HarborLinkContractError, HarborLinkMediaClient, HarborLinkRecordingArtifact,
        StartDetectionLeaseRequest, StartEventRecordingRequest, StartLiveSessionRequest,
        StartRecordingRequest,
    };
    use std::time::Duration;

    #[test]
    fn endpoint_encodes_camera_and_session_identity() {
        let client = HarborLinkMediaClient::new("http://127.0.0.1:8790/").expect("client");
        assert_eq!(
            client.live_session_endpoint("camera 1/left", "live/current"),
            "http://127.0.0.1:8790/v1/cameras/camera%201%2Fleft/live-sessions/live%2Fcurrent"
        );
        assert_eq!(
            encode_path_segment("cam-1_main~preview"),
            "cam-1_main~preview"
        );
    }

    #[test]
    fn start_request_contains_only_profile_and_ttl() {
        let body = serde_json::to_value(StartLiveSessionRequest {
            stream_profile: "sub",
            ttl_seconds: 300,
        })
        .expect("serialize request");

        assert_eq!(body["stream_profile"], "sub");
        assert_eq!(body["ttl_seconds"], 300);
        assert!(body.get("rtsp_url").is_none());
        assert!(body.get("username").is_none());
        assert!(body.get("password").is_none());
    }

    #[test]
    fn detection_lease_request_and_endpoint_never_contain_camera_credentials() {
        let client = HarborLinkMediaClient::new("http://127.0.0.1:8790").expect("client");
        let body = serde_json::to_value(StartDetectionLeaseRequest {
            stream_profile: "sub",
            ttl_seconds: 60,
            pre_roll_seconds: 3,
        })
        .expect("serialize request");

        assert_eq!(body["stream_profile"], "sub");
        assert_eq!(body["ttl_seconds"], 60);
        assert_eq!(body["pre_roll_seconds"], 3);
        assert_eq!(body.as_object().map(|value| value.len()), Some(3));
        assert_eq!(
            client.detection_lease_endpoint("camera 1/left", "detect/lease"),
            "http://127.0.0.1:8790/v1/cameras/camera%201%2Fleft/detection-leases/detect%2Flease"
        );
    }

    #[test]
    fn recording_request_contains_only_explicit_stream_profile() {
        let body = serde_json::to_value(StartRecordingRequest {
            stream_profile: Some("main"),
        })
        .expect("serialize request");

        assert_eq!(body["stream_profile"], "main");
        assert_eq!(body.as_object().map(|value| value.len()), Some(1));
    }

    #[test]
    fn recording_request_omits_stream_profile_for_legacy_default() {
        let body = serde_json::to_value(StartRecordingRequest {
            stream_profile: None,
        })
        .expect("serialize request");

        assert!(body.get("stream_profile").is_none());
    }

    #[test]
    fn event_recording_request_uses_selected_stream_profile() {
        for stream_profile in ["main", "sub"] {
            let body = serde_json::to_value(StartEventRecordingRequest {
                event_id: "cat-activity-test",
                owner: "harborbeacon",
                labels: &["cat"],
                stream_profile,
                ttl_seconds: 45,
                pre_roll_seconds: 3,
                trigger_epoch_ms: 1_786_060_800_123,
                required_end_epoch_ms: None,
            })
            .expect("serialize request");

            assert_eq!(body["streamProfile"], stream_profile);
            assert_eq!(body["eventId"], "cat-activity-test");
            assert_eq!(body["owner"], "harborbeacon");
            assert_eq!(body["preRollSeconds"], 3);
            assert_eq!(body["triggerEpochMs"], 1_786_060_800_123_u64);
        }
    }

    #[test]
    fn package_event_recording_request_carries_package_lifecycle_labels() {
        let body = serde_json::to_value(StartEventRecordingRequest {
            event_id: "package-removed-test",
            owner: "harborbeacon",
            labels: &["package", "package_no_longer_visible"],
            stream_profile: "sub",
            ttl_seconds: 15,
            pre_roll_seconds: 3,
            trigger_epoch_ms: 1_786_060_800_123,
            required_end_epoch_ms: Some(1_786_060_815_123),
        })
        .expect("serialize package event recording request");

        assert_eq!(
            body["labels"],
            serde_json::json!(["package", "package_no_longer_visible"])
        );
        assert_eq!(body["eventId"], "package-removed-test");
        assert_eq!(body["owner"], "harborbeacon");
        assert_eq!(body["requiredEndEpochMs"], 1_786_060_815_123_u64);
    }

    #[test]
    fn recording_artifact_deserializes_verified_timeline_coverage() {
        let artifact: HarborLinkRecordingArtifact = serde_json::from_value(serde_json::json!({
            "mediaContractVersion": "1.0",
            "artifactId": "recording-package",
            "cameraId": "camera.252",
            "kind": "recording",
            "mimeType": "video/mp4",
            "byteSize": 1024,
            "startedAtEpochMs": 95_000,
            "endedAtEpochMs": 115_000,
            "durationSeconds": 20,
            "streamKind": "substream",
            "modifiedAtEpochMs": 115_000,
            "previewUrl": "/v1/dvr/artifacts/recording-package",
            "eventId": "package-removed-test",
            "labels": ["package", "package_no_longer_visible"],
            "source": "yolo_package_lifecycle",
            "coverageStartEpochMs": 95_000,
            "coverageEndEpochMs": 115_000,
            "coverageVerified": true,
            "gapFree": true,
            "coverageSegmentDurationMs": 2_000
        }))
        .expect("deserialize verified package recording artifact");

        assert_eq!(artifact.coverage_start_epoch_ms, Some(95_000));
        assert_eq!(artifact.coverage_end_epoch_ms, Some(115_000));
        assert!(artifact.coverage_verified);
        assert!(artifact.gap_free);
        assert_eq!(artifact.coverage_segment_duration_ms, Some(2_000));
    }

    #[test]
    fn mutation_retry_clones_reuse_the_same_request_id() {
        let client = HarborLinkMediaClient::new("http://127.0.0.1:8790").expect("client");
        let request = client.request(
            reqwest::Method::POST,
            "http://127.0.0.1:8790/v1/cameras/cam-1/recordings/current".to_string(),
            true,
        );
        let first = request
            .try_clone()
            .expect("clone first attempt")
            .build()
            .expect("build first attempt");
        let retry = request
            .try_clone()
            .expect("clone retry")
            .build()
            .expect("build retry");
        assert_eq!(
            first.headers().get("X-Request-Id"),
            retry.headers().get("X-Request-Id")
        );
        assert!(first.headers().get("X-Request-Id").is_some());
    }

    #[test]
    fn business_request_id_is_stable_per_operation_and_distinct_between_operations() {
        let client = HarborLinkMediaClient::new("http://127.0.0.1:8790").expect("client");
        let _scope = harborlink_request_scope(Some("webui-business-operation-1"));
        let build_request_id = |method, url: &str| {
            client
                .request(method, url.to_string(), true)
                .build()
                .expect("build request")
                .headers()
                .get("X-Request-Id")
                .expect("request id")
                .to_str()
                .expect("request id text")
                .to_string()
        };
        let start_url = "http://127.0.0.1:8790/v1/cameras/cam-1/recordings/current";
        let first = build_request_id(reqwest::Method::POST, start_url);
        let retry = build_request_id(reqwest::Method::POST, start_url);
        let stop = build_request_id(reqwest::Method::DELETE, start_url);

        assert_eq!(first, retry);
        assert_ne!(first, stop);
        assert!(first.starts_with("webui-business-operation-1-"));
    }

    #[test]
    fn structured_contract_errors_preserve_review_fields() {
        let error = HarborLinkContractError {
            status_code: 503,
            code: "MEDIAMTX_UNAVAILABLE".to_string(),
            message: "Media relay is unavailable".to_string(),
            retryable: true,
            dependency: "mediamtx".to_string(),
            request_id: Some("beacon-operation-1".to_string()),
        };
        let value = serde_json::to_value(error).expect("serialize contract error");
        assert_eq!(value["code"], "MEDIAMTX_UNAVAILABLE");
        assert_eq!(value["retryable"], true);
        assert_eq!(value["dependency"], "mediamtx");
        assert_eq!(value["requestId"], "beacon-operation-1");
    }

    #[test]
    fn rejects_non_http_media_url() {
        let error = HarborLinkMediaClient::new("file:///tmp/harborlink.sock")
            .expect_err("file URL must be rejected");
        assert!(error.contains("http or https"));
    }

    #[test]
    fn detection_lease_start_timeout_covers_harborlink_pre_roll_wait() {
        assert_eq!(detection_lease_start_timeout(), Duration::from_secs(45));
    }
}
