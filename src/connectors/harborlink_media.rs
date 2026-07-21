use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::Read;
use std::time::Duration;
use uuid::Uuid;

use crate::runtime::registry::CameraCapabilities;

const DEFAULT_HARBORLINK_MEDIA_API_URL: &str = "http://127.0.0.1:8790";
const DEFAULT_HARBORLINK_LOCAL_API_TOKEN_FILE: &str = "/etc/harborlink/local-api.token";
const HARBORLINK_CONTRACT_VERSION: &str = "1.0";

#[derive(Debug, Clone)]
pub struct HarborLinkMediaClient {
    base_url: String,
    local_api_token: Option<String>,
    http: Client,
}

#[derive(Debug, Clone, Serialize)]
struct StartLiveSessionRequest<'a> {
    stream_profile: &'a str,
    ttl_seconds: u64,
}

#[derive(Debug, Clone, Serialize)]
struct StartRecordingRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_profile: Option<&'a str>,
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
pub struct HarborLinkClipCapture {
    pub camera_id: String,
    pub clip_path: String,
    pub keyframe_paths: Vec<String>,
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
            .send()
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
            .send()
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
            .send()
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
            .send()
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
            .send()
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
            .send()
            .map_err(unavailable_error)?;
        decode_session_response(response)
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
            .send()
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
            .send()
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
            .send()
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
        let response = request.send().map_err(unavailable_error)?;
        if !response.status().is_success() {
            return Err(redacted_media_error(response.status(), "camera recording"));
        }
        response
            .json::<HarborLinkRecordingStatus>()
            .map_err(|error| {
                format!("HarborLink returned an invalid camera recording response: {error}")
            })
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
            .send()
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
            .send()
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
            .send()
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
            .send()
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
            .send()
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
            .send()
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
            .send()
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
            .send()
            .map_err(unavailable_error)?;
        decode_json_response(response, "DVR settings update")
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
            .send()
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
            .send()
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
            .send()
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
            .send()
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
            .send()
            .map_err(unavailable_error)?;
        decode_json_response(response, "Home Assistant configuration")
    }

    fn request(&self, method: reqwest::Method, url: String, mutation: bool) -> RequestBuilder {
        let mut request = self
            .http
            .request(method, url)
            .header("X-HarborLink-Contract-Version", HARBORLINK_CONTRACT_VERSION);
        if let Some(token) = self.local_api_token.as_deref() {
            request = request.bearer_auth(token);
        }
        if mutation {
            request = request.header(
                "X-Request-Id",
                format!("beacon-{}", Uuid::new_v4().simple()),
            );
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

fn unavailable_error(error: reqwest::Error) -> String {
    format!("HarborLink southbound media service is unavailable: {error}")
}

fn decode_session_response(response: Response) -> Result<HarborLinkLiveSession, String> {
    let status = response.status();
    if status.is_success() {
        return response
            .json::<HarborLinkLiveSession>()
            .map_err(|error| format!("HarborLink returned an invalid media response: {error}"));
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
        .map_err(|error| format!("HarborLink returned an invalid {operation} response: {error}"))
}

fn redacted_media_response_error(response: Response, operation: &str) -> String {
    let status = response.status();
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
            return format!(
                "HarborLink {operation} failed: {code} from {dependency}; retryable={retryable} (HTTP {})",
                status.as_u16()
            );
        }
    }
    redacted_media_error(status, operation)
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
        encode_path_segment, HarborLinkMediaClient, StartLiveSessionRequest, StartRecordingRequest,
    };

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
    fn rejects_non_http_media_url() {
        let error = HarborLinkMediaClient::new("file:///tmp/harborlink.sock")
            .expect_err("file URL must be rejected");
        assert!(error.contains("http or https"));
    }
}
