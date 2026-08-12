//! Outbound notification contract for HarborGate delivery and local UI alerts.

use std::env;
use std::time::Duration;

use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

pub const CONTRACT_VERSION: &str = "2.0";
const IM_GATEWAY_BASE_URL_ENV: &str = "HARBORGATE_BASE_URL";
const IM_GATEWAY_BEARER_TOKEN_ENV: &str = "HARBORGATE_BEARER_TOKEN";
const LEGACY_IM_GATEWAY_BASE_URL_ENV: &str = "HARBOR_IM_GATEWAY_BASE_URL";
const LEGACY_IM_GATEWAY_BEARER_TOKEN_ENV: &str = "HARBOR_IM_GATEWAY_BEARER_TOKEN";
const SHARED_IM_AGENT_SERVICE_TOKEN_ENV: &str = "IM_AGENT_SERVICE_TOKEN";
const DEFAULT_LOCAL_IM_GATEWAY_BASE_URL: &str = "http://127.0.0.1:8787";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationPayloadFormat {
    PlainText,
    Markdown,
    LarkCard,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationAttachmentKind {
    Image,
    Video,
    Link,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationDestinationKind {
    Conversation,
    Recipient,
    LocalUi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationDeliveryMode {
    Send,
    Reply,
    Update,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationRecipientIdType {
    ChatId,
    OpenId,
    Email,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NotificationDeliveryStatus {
    Sent,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationRecipient {
    pub recipient_id: String,
    pub recipient_type: NotificationRecipientIdType,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotificationAttachment {
    pub kind: NotificationAttachmentKind,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_id: Option<String>,
    #[serde(default)]
    pub mime_type: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationSource {
    pub service: String,
    pub module: String,
    pub event_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationDestination {
    pub kind: NotificationDestinationKind,
    #[serde(default)]
    pub route_key: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub recipient: Option<NotificationRecipient>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotificationContent {
    pub title: String,
    pub body: String,
    #[serde(default = "default_payload_format")]
    pub payload_format: NotificationPayloadFormat,
    #[serde(default)]
    pub structured_payload: Value,
    #[serde(default)]
    pub attachments: Vec<NotificationAttachment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub delivery_hints: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationDelivery {
    pub mode: NotificationDeliveryMode,
    #[serde(default)]
    pub reply_to_message_id: String,
    #[serde(default)]
    pub update_message_id: String,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct NotificationMetadata {
    #[serde(default)]
    pub correlation_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotificationRequest {
    pub notification_id: String,
    pub trace_id: String,
    pub source: NotificationSource,
    pub destination: NotificationDestination,
    pub content: NotificationContent,
    pub delivery: NotificationDelivery,
    #[serde(default)]
    pub metadata: NotificationMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationErrorDetail {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotificationDeliveryRecord {
    pub delivery_id: String,
    pub notification_id: String,
    pub trace_id: String,
    pub ok: bool,
    pub status: NotificationDeliveryStatus,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub provider_message_id: Option<String>,
    #[serde(default)]
    pub retryable: bool,
    #[serde(default)]
    pub error: Option<NotificationErrorDetail>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedHttpErrorDetail {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedHttpErrorEnvelope {
    pub ok: bool,
    pub error: SharedHttpErrorDetail,
    #[serde(default)]
    pub trace_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationGatewayConfig {
    pub base_url: String,
    pub bearer_token: String,
}

impl NotificationGatewayConfig {
    pub fn from_env() -> Result<Self, String> {
        let base_url =
            env_var_with_legacy_alias(IM_GATEWAY_BASE_URL_ENV, LEGACY_IM_GATEWAY_BASE_URL_ENV)
                .unwrap_or_else(|| DEFAULT_LOCAL_IM_GATEWAY_BASE_URL.to_string());
        let bearer_token = env_var_with_legacy_alias(
            IM_GATEWAY_BEARER_TOKEN_ENV,
            LEGACY_IM_GATEWAY_BEARER_TOKEN_ENV,
        )
        .or_else(|| env_var(SHARED_IM_AGENT_SERVICE_TOKEN_ENV))
        .ok_or_else(|| format!("missing required env var {IM_GATEWAY_BEARER_TOKEN_ENV}"))?;
        Self::new(base_url, bearer_token)
    }

    pub fn new(
        base_url: impl Into<String>,
        bearer_token: impl Into<String>,
    ) -> Result<Self, String> {
        let base_url = base_url.into().trim().to_string();
        if base_url.is_empty() {
            return Err("HarborGate base URL cannot be empty".to_string());
        }
        let bearer_token = bearer_token.into().trim().to_string();
        if bearer_token.is_empty() {
            return Err("HarborGate bearer token cannot be empty".to_string());
        }
        Ok(Self {
            base_url,
            bearer_token,
        })
    }
}

fn env_var_with_legacy_alias(primary: &str, legacy: &str) -> Option<String> {
    if let Some(value) = env_var(primary) {
        return Some(value);
    }

    env_var(legacy).inspect(|_| {
        eprintln!("warning: {legacy} is deprecated; prefer {primary}");
    })
}

fn env_var(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[derive(Debug, Clone, PartialEq)]
pub enum NotificationDeliveryError {
    MissingConfiguration(String),
    Transport(String),
    InvalidResponse(String),
    RequestRejected {
        status_code: u16,
        envelope: SharedHttpErrorEnvelope,
    },
}

impl std::fmt::Display for NotificationDeliveryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingConfiguration(message)
            | Self::Transport(message)
            | Self::InvalidResponse(message) => f.write_str(message),
            Self::RequestRejected {
                status_code,
                envelope,
            } => write!(
                f,
                "notification request rejected with HTTP {status_code}: {} ({})",
                envelope.error.message, envelope.error.code
            ),
        }
    }
}

impl std::error::Error for NotificationDeliveryError {}

pub struct NotificationDeliveryService {
    client: Client,
    config: NotificationGatewayConfig,
}

impl NotificationDeliveryService {
    pub fn new() -> Result<Self, String> {
        let config = NotificationGatewayConfig::from_env()
            .map_err(|error| format!("notification delivery configuration error: {error}"))?;
        Self::from_config(config)
    }

    pub fn from_config(config: NotificationGatewayConfig) -> Result<Self, String> {
        let client = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|error| format!("failed to build notification delivery client: {error}"))?;
        Ok(Self { client, config })
    }

    pub fn deliver(
        &self,
        request: &NotificationRequest,
    ) -> Result<NotificationDeliveryRecord, NotificationDeliveryError> {
        if request.destination.kind == NotificationDestinationKind::LocalUi {
            return Ok(NotificationDeliveryRecord {
                delivery_id: new_delivery_id(),
                notification_id: request.notification_id.clone(),
                trace_id: request.trace_id.clone(),
                ok: true,
                status: NotificationDeliveryStatus::Sent,
                platform: "local_ui".to_string(),
                provider_message_id: None,
                retryable: false,
                error: None,
            });
        }

        let endpoint = notification_endpoint(&self.config.base_url);
        let response = self
            .client
            .post(endpoint)
            .header(
                "Authorization",
                format!("Bearer {}", self.config.bearer_token),
            )
            .header("X-Contract-Version", CONTRACT_VERSION)
            .json(request)
            .send()
            .map_err(|error| {
                NotificationDeliveryError::Transport(format!(
                    "notification delivery request failed: {error}"
                ))
            })?;
        let status_code = response.status().as_u16();
        let response_body = response.text().map_err(|error| {
            NotificationDeliveryError::InvalidResponse(format!(
                "failed to read notification delivery response: {error}"
            ))
        })?;

        if (200..300).contains(&status_code) {
            return serde_json::from_str::<NotificationDeliveryRecord>(&response_body).map_err(
                |error| {
                    NotificationDeliveryError::InvalidResponse(format!(
                        "invalid notification delivery response body: {error}"
                    ))
                },
            );
        }

        match serde_json::from_str::<SharedHttpErrorEnvelope>(&response_body) {
            Ok(envelope) => Err(NotificationDeliveryError::RequestRejected {
                status_code,
                envelope,
            }),
            Err(error) => Err(NotificationDeliveryError::InvalidResponse(format!(
                "notification delivery rejected with HTTP {status_code}, but body was not a shared error envelope: {error}"
            ))),
        }
    }
}

fn default_payload_format() -> NotificationPayloadFormat {
    NotificationPayloadFormat::PlainText
}

fn notification_endpoint(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.ends_with("/api/notifications/deliveries") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/api/notifications/deliveries")
    }
}

fn new_delivery_id() -> String {
    format!("delivery_{}", Uuid::new_v4().as_simple())
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Mutex, MutexGuard};
    use std::thread;

    use serde_json::{json, Value};

    use super::{
        NotificationAttachment, NotificationAttachmentKind, NotificationContent,
        NotificationDelivery, NotificationDeliveryError, NotificationDeliveryMode,
        NotificationDeliveryService, NotificationDeliveryStatus, NotificationDestination,
        NotificationDestinationKind, NotificationErrorDetail, NotificationGatewayConfig,
        NotificationMetadata, NotificationPayloadFormat, NotificationRequest, NotificationSource,
        SharedHttpErrorDetail, SharedHttpErrorEnvelope, CONTRACT_VERSION,
        DEFAULT_LOCAL_IM_GATEWAY_BASE_URL, IM_GATEWAY_BASE_URL_ENV, IM_GATEWAY_BEARER_TOKEN_ENV,
        LEGACY_IM_GATEWAY_BASE_URL_ENV, LEGACY_IM_GATEWAY_BEARER_TOKEN_ENV,
        SHARED_IM_AGENT_SERVICE_TOKEN_ENV,
    };

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        previous: Vec<(&'static str, Option<String>)>,
        _lock: MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn scoped(values: &[(&'static str, Option<&str>)]) -> Self {
            let lock = ENV_LOCK.lock().expect("env lock");
            let previous = values
                .iter()
                .map(|(key, _)| (*key, env::var(key).ok()))
                .collect::<Vec<_>>();

            for (key, value) in values {
                if let Some(value) = value {
                    env::set_var(key, value);
                } else {
                    env::remove_var(key);
                }
            }

            Self {
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, previous) in self.previous.drain(..) {
                if let Some(value) = previous {
                    env::set_var(key, value);
                } else {
                    env::remove_var(key);
                }
            }
        }
    }

    fn sample_request() -> NotificationRequest {
        NotificationRequest {
            notification_id: "notif_01JABC".to_string(),
            trace_id: "trace_01JABC".to_string(),
            source: NotificationSource {
                service: "harborbeacon".to_string(),
                module: "task_api".to_string(),
                event_type: "task.completed".to_string(),
            },
            destination: NotificationDestination {
                kind: NotificationDestinationKind::Conversation,
                route_key: "gw_route_01JABC".to_string(),
                id: String::new(),
                platform: String::new(),
                recipient: None,
            },
            content: NotificationContent {
                title: "Front Door AI Analysis".to_string(),
                body: "1 person detected.".to_string(),
                payload_format: NotificationPayloadFormat::LarkCard,
                structured_payload: json!({
                    "header": {"title": {"content": "Front Door AI Analysis"}}
                }),
                attachments: vec![NotificationAttachment {
                    kind: NotificationAttachmentKind::Image,
                    label: "snapshot".to_string(),
                    artifact_id: None,
                    mime_type: "image/jpeg".to_string(),
                    path: Some(".harborbeacon/vision/snapshot.jpg".to_string()),
                    url: None,
                    metadata: Value::Null,
                }],
                delivery_hints: Vec::new(),
            },
            delivery: NotificationDelivery {
                mode: NotificationDeliveryMode::Send,
                reply_to_message_id: String::new(),
                update_message_id: String::new(),
                idempotency_key: "idem_01JABC".to_string(),
            },
            metadata: NotificationMetadata {
                correlation_id: "trace_01JABC".to_string(),
            },
        }
    }

    #[test]
    fn legacy_notification_content_and_attachment_default_stable_artifact_fields() {
        let attachment = serde_json::from_value::<NotificationAttachment>(json!({
            "kind": "video",
            "label": "legacy video",
            "mime_type": "video/mp4",
            "url": "/legacy.mp4"
        }))
        .expect("legacy notification attachment");
        let content = serde_json::from_value::<NotificationContent>(json!({
            "title": "legacy",
            "body": "legacy body",
            "attachments": []
        }))
        .expect("legacy notification content");

        assert!(attachment.artifact_id.is_none());
        assert!(content.delivery_hints.is_empty());
    }

    #[test]
    fn notification_gateway_config_accepts_shared_gate_defaults() {
        let _env = EnvGuard::scoped(&[
            (IM_GATEWAY_BASE_URL_ENV, None),
            (LEGACY_IM_GATEWAY_BASE_URL_ENV, None),
            (IM_GATEWAY_BEARER_TOKEN_ENV, None),
            (LEGACY_IM_GATEWAY_BEARER_TOKEN_ENV, None),
            (SHARED_IM_AGENT_SERVICE_TOKEN_ENV, Some("shared-gate-token")),
        ]);

        let config = NotificationGatewayConfig::from_env().expect("config");

        assert_eq!(config.base_url, DEFAULT_LOCAL_IM_GATEWAY_BASE_URL);
        assert_eq!(config.bearer_token, "shared-gate-token");
    }

    #[test]
    fn request_round_trips_contract_shape() {
        let request = sample_request();

        let encoded = serde_json::to_value(&request).expect("encode");

        assert_eq!(encoded["notification_id"], "notif_01JABC");
        assert_eq!(encoded["destination"]["route_key"], "gw_route_01JABC");
        assert_eq!(encoded["content"]["payload_format"], "lark_card");
        assert_eq!(encoded["delivery"]["mode"], "send");
        assert_eq!(encoded["metadata"]["correlation_id"], "trace_01JABC");
    }

    #[test]
    fn local_ui_delivery_returns_sent_record_without_network() {
        let service = NotificationDeliveryService::from_config(
            NotificationGatewayConfig::new("http://127.0.0.1:9", "test-token").expect("config"),
        )
        .expect("service");
        let mut request = sample_request();
        request.destination.kind = NotificationDestinationKind::LocalUi;
        request.destination.route_key.clear();

        let record = service.deliver(&request).expect("delivery");

        assert_eq!(record.notification_id, request.notification_id);
        assert!(record.ok);
        assert_eq!(record.status, NotificationDeliveryStatus::Sent);
        assert_eq!(record.platform, "local_ui");
        assert!(record.provider_message_id.is_none());
    }

    #[test]
    fn accepted_delivery_request_posts_contract_headers_and_body() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let address = listener.local_addr().expect("listener address");
        let server = thread::spawn(move || -> (String, String) {
            let (mut stream, _) = listener.accept().expect("accept request");
            let (request_head, body) = read_http_request(&mut stream);
            let response_body = json!({
                "delivery_id": "delivery_01JABC",
                "notification_id": "notif_01JABC",
                "trace_id": "trace_01JABC",
                "ok": true,
                "status": "sent",
                "platform": "feishu",
                "provider_message_id": "om_01JABC",
                "retryable": false,
                "error": null
            })
            .to_string();
            write_http_response(&mut stream, 200, "OK", &response_body);
            (request_head, body)
        });

        let service = NotificationDeliveryService::from_config(
            NotificationGatewayConfig::new(format!("http://{address}"), "test-token")
                .expect("config"),
        )
        .expect("service");
        let request = sample_request();

        let record = service.deliver(&request).expect("delivery");
        let (request_head, body) = server.join().expect("server thread");
        let request_head_lower = request_head.to_lowercase();
        let body_json: Value = serde_json::from_str(&body).expect("body json");

        assert!(request_head.starts_with("POST /api/notifications/deliveries HTTP/1.1"));
        assert!(request_head_lower.contains("authorization: bearer test-token"));
        assert!(request_head_lower.contains(&format!(
            "x-contract-version: {}",
            CONTRACT_VERSION.to_lowercase()
        )));
        assert_eq!(body_json["destination"]["route_key"], "gw_route_01JABC");
        assert_eq!(record.delivery_id, "delivery_01JABC");
        assert_eq!(record.provider_message_id.as_deref(), Some("om_01JABC"));
        assert!(record.ok);
    }

    #[test]
    fn rejected_delivery_request_returns_shared_error_envelope() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind listener");
        let address = listener.local_addr().expect("listener address");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept request");
            let _ = read_http_request(&mut stream);
            let response_body = serde_json::to_string(&SharedHttpErrorEnvelope {
                ok: false,
                error: SharedHttpErrorDetail {
                    code: "ROUTE_NOT_FOUND".to_string(),
                    message: "route key not found".to_string(),
                },
                trace_id: Some("trace_01JABC".to_string()),
            })
            .expect("encode shared error");
            write_http_response(&mut stream, 404, "Not Found", &response_body);
        });

        let service = NotificationDeliveryService::from_config(
            NotificationGatewayConfig::new(format!("http://{address}"), "test-token")
                .expect("config"),
        )
        .expect("service");

        let result = service.deliver(&sample_request());
        server.join().expect("server thread");

        match result {
            Err(NotificationDeliveryError::RequestRejected {
                status_code,
                envelope,
            }) => {
                assert_eq!(status_code, 404);
                assert_eq!(envelope.error.code, "ROUTE_NOT_FOUND");
                assert_eq!(envelope.trace_id.as_deref(), Some("trace_01JABC"));
            }
            other => panic!("expected request rejection, got {other:?}"),
        }
    }

    #[test]
    fn gateway_config_requires_non_empty_values() {
        assert!(NotificationGatewayConfig::new("", "token").is_err());
        assert!(NotificationGatewayConfig::new("http://127.0.0.1:4176", "").is_err());
    }

    #[test]
    fn delivery_record_round_trips_failure_payload() {
        let record = serde_json::from_value::<super::NotificationDeliveryRecord>(json!({
            "delivery_id": "delivery_01JABC",
            "notification_id": "notif_01JABC",
            "trace_id": "trace_01JABC",
            "ok": false,
            "status": "failed",
            "platform": "feishu",
            "provider_message_id": null,
            "retryable": true,
            "error": {
                "code": "PLATFORM_UNAVAILABLE",
                "message": "upstream timeout"
            }
        }))
        .expect("record");

        assert_eq!(record.status, NotificationDeliveryStatus::Failed);
        assert_eq!(
            record.error,
            Some(NotificationErrorDetail {
                code: "PLATFORM_UNAVAILABLE".to_string(),
                message: "upstream timeout".to_string(),
            })
        );
        assert!(record.retryable);
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> (String, String) {
        let mut buffer = [0_u8; 4096];
        let mut request_bytes = Vec::new();
        let mut content_length = 0_usize;
        let mut header_end = None;

        loop {
            let bytes_read = stream.read(&mut buffer).expect("read request");
            if bytes_read == 0 {
                break;
            }
            request_bytes.extend_from_slice(&buffer[..bytes_read]);
            if header_end.is_none() {
                if let Some(end) = request_bytes
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                {
                    header_end = Some(end + 4);
                    let headers = String::from_utf8_lossy(&request_bytes[..end + 4]).to_string();
                    content_length = content_length_from_headers(&headers);
                }
            }
            if let Some(end) = header_end {
                if request_bytes.len() >= end + content_length {
                    break;
                }
            }
        }

        let header_end = header_end.expect("header end");
        let head = String::from_utf8_lossy(&request_bytes[..header_end]).to_string();
        let body = String::from_utf8_lossy(&request_bytes[header_end..header_end + content_length])
            .to_string();
        (head, body)
    }

    fn content_length_from_headers(headers: &str) -> usize {
        headers
            .lines()
            .find_map(|line| {
                let mut parts = line.splitn(2, ':');
                let name = parts.next()?.trim();
                let value = parts.next()?.trim();
                name.eq_ignore_ascii_case("Content-Length")
                    .then(|| value.parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0)
    }

    fn write_http_response(
        stream: &mut std::net::TcpStream,
        status_code: u16,
        reason: &str,
        body: &str,
    ) {
        let response = format!(
            "HTTP/1.1 {status_code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write response");
        stream.flush().expect("flush response");
    }
}
