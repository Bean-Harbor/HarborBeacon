//! Home Assistant REST connector.

use std::fs;
use std::time::Duration;

use reqwest::blocking::{Client, RequestBuilder, Response};
use reqwest::{StatusCode, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

const DEFAULT_TIMEOUT_SECONDS: u64 = 8;
const DEFAULT_HARBORLINK_MEDIA_API_URL: &str = "http://127.0.0.1:8790";
const DEFAULT_HARBORLINK_LOCAL_API_TOKEN_FILE: &str = "/etc/harborlink/local-api.token";
const HARBORLINK_CONTRACT_VERSION: &str = "1.0";
pub const HOME_ASSISTANT_TOKEN_REDACTION: &str = "__harbor_redacted__";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeAssistantClientConfig {
    pub base_url: String,
    pub access_token: String,
    pub timeout_seconds: u64,
}

#[derive(Debug, Clone)]
pub struct HomeAssistantClient {
    base_url: Url,
    access_token: Option<String>,
    local_api_token: Option<String>,
    backend: HomeAssistantBackend,
    http: Client,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HomeAssistantBackend {
    Direct,
    HarborLink,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HomeAssistantConfigSummary {
    pub base_url: String,
    pub configured: bool,
    pub token_configured: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HomeAssistantCoreConfig {
    #[serde(default)]
    pub location_name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub unit_system: Value,
    #[serde(default)]
    pub time_zone: Option<String>,
    #[serde(flatten)]
    pub extra: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HomeAssistantConnectionTest {
    pub ok: bool,
    pub status: String,
    #[serde(default)]
    pub location_name: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HomeAssistantEntity {
    pub entity_id: String,
    pub domain: String,
    pub state: String,
    pub display_name: String,
    #[serde(default)]
    pub area_id: Option<String>,
    #[serde(default)]
    pub device_class: Option<String>,
    #[serde(default)]
    pub last_changed: Option<String>,
    #[serde(default)]
    pub last_updated: Option<String>,
    #[serde(default)]
    pub attributes: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HomeAssistantServiceDomain {
    pub domain: String,
    #[serde(default)]
    pub services: Vec<HomeAssistantService>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HomeAssistantService {
    pub service: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub fields: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HomeAssistantServiceCallResponse {
    pub domain: String,
    pub service: String,
    pub entity_id: String,
    pub ok: bool,
    #[serde(default)]
    pub changed_entity_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct HomeAssistantServiceActionRequest {
    #[serde(default)]
    pub entity_id: String,
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub service: String,
    #[serde(default)]
    pub fields: Value,
}

#[derive(Debug, Clone, Deserialize)]
struct RawHomeAssistantEntity {
    entity_id: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    attributes: Value,
    #[serde(default)]
    last_changed: Option<String>,
    #[serde(default)]
    last_updated: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RawHomeAssistantServiceDomain {
    domain: String,
    #[serde(default)]
    services: Value,
}

impl HomeAssistantClientConfig {
    pub fn new(base_url: impl Into<String>, access_token: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            access_token: access_token.into(),
            timeout_seconds: DEFAULT_TIMEOUT_SECONDS,
        }
    }

    pub fn configured(&self) -> bool {
        !self.base_url.trim().is_empty() && !self.access_token.trim().is_empty()
    }

    pub fn redacted_summary(&self) -> HomeAssistantConfigSummary {
        HomeAssistantConfigSummary {
            base_url: self.base_url.trim().trim_end_matches('/').to_string(),
            configured: self.configured(),
            token_configured: !self.access_token.trim().is_empty(),
        }
    }
}

impl HomeAssistantClient {
    pub fn new(config: HomeAssistantClientConfig) -> Result<Self, String> {
        let base_url = normalize_base_url(&config.base_url)?;
        let access_token = config.access_token.trim().to_string();
        if access_token.is_empty() {
            return Err("Home Assistant access token is required".to_string());
        }
        let timeout = Duration::from_secs(config.timeout_seconds.max(1));
        let http = Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|error| format!("failed to build Home Assistant client: {error}"))?;

        Ok(Self {
            base_url,
            access_token: Some(access_token),
            local_api_token: None,
            backend: HomeAssistantBackend::Direct,
            http,
        })
    }

    pub fn from_harborlink_env() -> Result<Self, String> {
        let base_url = std::env::var("HARBORLINK_MEDIA_API_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_HARBORLINK_MEDIA_API_URL.to_string());
        let base_url = normalize_base_url(&base_url)?;
        let http = Client::builder()
            .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECONDS))
            .build()
            .map_err(|error| format!("failed to build HarborLink client: {error}"))?;
        Ok(Self {
            base_url,
            access_token: None,
            local_api_token: read_local_api_token_from_env()?,
            backend: HomeAssistantBackend::HarborLink,
            http,
        })
    }

    pub fn test_connection(&self) -> HomeAssistantConnectionTest {
        if self.backend == HomeAssistantBackend::HarborLink {
            return match self.post_json("/v1/home-assistant/test", &json!({})) {
                Ok(test) => test,
                Err(error) => HomeAssistantConnectionTest {
                    ok: false,
                    status: "error".to_string(),
                    location_name: None,
                    version: None,
                    error: Some(error),
                },
            };
        }
        match self.fetch_core_config() {
            Ok(config) => HomeAssistantConnectionTest {
                ok: true,
                status: "connected".to_string(),
                location_name: config.location_name,
                version: config.version,
                error: None,
            },
            Err(error) => HomeAssistantConnectionTest {
                ok: false,
                status: "error".to_string(),
                location_name: None,
                version: None,
                error: Some(error),
            },
        }
    }

    pub fn fetch_core_config(&self) -> Result<HomeAssistantCoreConfig, String> {
        self.get_json("/api/config")
    }

    pub fn fetch_entities(&self) -> Result<Vec<HomeAssistantEntity>, String> {
        if self.backend == HomeAssistantBackend::HarborLink {
            return self.get_json("/v1/home-assistant/entities");
        }
        let raw: Vec<RawHomeAssistantEntity> = self.get_json("/api/states")?;
        Ok(raw.into_iter().map(normalize_entity).collect())
    }

    pub fn fetch_services(&self) -> Result<Vec<HomeAssistantServiceDomain>, String> {
        if self.backend == HomeAssistantBackend::HarborLink {
            return self.get_json("/v1/home-assistant/services");
        }
        let raw: Vec<RawHomeAssistantServiceDomain> = self.get_json("/api/services")?;
        Ok(raw.into_iter().map(normalize_service_domain).collect())
    }

    pub fn call_service(
        &self,
        domain: &str,
        service: &str,
        entity_id: &str,
        fields: Option<&Value>,
    ) -> Result<HomeAssistantServiceCallResponse, String> {
        let domain = sanitize_service_path_component(domain, "domain")?;
        let service = sanitize_service_path_component(service, "service")?;
        let entity_id = entity_id.trim();
        if entity_id.is_empty() {
            return Err("Home Assistant entity id is required".to_string());
        }
        let path = if self.backend == HomeAssistantBackend::HarborLink {
            format!("/v1/home-assistant/services/{domain}/{service}")
        } else {
            format!("/api/services/{domain}/{service}")
        };
        let mut body = fields
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if self.backend == HomeAssistantBackend::HarborLink {
            let value: HomeAssistantServiceCallResponse = self.post_json(
                &path,
                &json!({
                    "entity_id": entity_id,
                    "fields": Value::Object(body),
                }),
            )?;
            return Ok(value);
        }
        body.insert("entity_id".to_string(), json!(entity_id));
        let body = Value::Object(body);
        let value: Value = self.post_json(&path, &body)?;
        let changed_entity_count = value.as_array().map(Vec::len).unwrap_or(0);
        Ok(HomeAssistantServiceCallResponse {
            domain,
            service,
            entity_id: entity_id.to_string(),
            ok: true,
            changed_entity_count,
        })
    }

    fn get_json<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T, String> {
        let url = self
            .base_url
            .join(path.trim_start_matches('/'))
            .map_err(|error| format!("invalid Home Assistant endpoint {path}: {error}"))?;
        let mut request = self.http.get(url);
        if let Some(access_token) = self.access_token.as_deref() {
            request = request.bearer_auth(access_token);
        }
        if self.backend == HomeAssistantBackend::HarborLink {
            request = self.harborlink_request(request, false);
        }
        let response = request
            .send()
            .map_err(|error| format!("Home Assistant request failed: {error}"))?;
        let status = response.status();
        if !status.is_success() {
            if self.backend == HomeAssistantBackend::HarborLink {
                return Err(format_harborlink_status_error(
                    response,
                    "Home Assistant request",
                ));
            }
            return Err(format_home_assistant_status_error(status));
        }
        response
            .json::<T>()
            .map_err(|error| format!("failed to parse Home Assistant response: {error}"))
    }

    fn post_json<T: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        body: &Value,
    ) -> Result<T, String> {
        let url = self
            .base_url
            .join(path.trim_start_matches('/'))
            .map_err(|error| format!("invalid Home Assistant endpoint {path}: {error}"))?;
        let mut request = self.http.post(url).json(body);
        if let Some(access_token) = self.access_token.as_deref() {
            request = request.bearer_auth(access_token);
        }
        if self.backend == HomeAssistantBackend::HarborLink {
            request = self.harborlink_request(request, true);
        }
        let response = request
            .send()
            .map_err(|error| format!("Home Assistant request failed: {error}"))?;
        let status = response.status();
        if !status.is_success() {
            if self.backend == HomeAssistantBackend::HarborLink {
                return Err(format_harborlink_status_error(
                    response,
                    "Home Assistant request",
                ));
            }
            return Err(format_home_assistant_status_error(status));
        }
        response
            .json::<T>()
            .map_err(|error| format!("failed to parse Home Assistant response: {error}"))
    }

    fn harborlink_request(&self, request: RequestBuilder, mutation: bool) -> RequestBuilder {
        let mut request =
            request.header("X-HarborLink-Contract-Version", HARBORLINK_CONTRACT_VERSION);
        if let Some(token) = self.local_api_token.as_deref() {
            request = request.bearer_auth(token);
        }
        if mutation {
            request = request.header(
                "X-Request-Id",
                format!("beacon-{}", uuid::Uuid::new_v4().simple()),
            );
        }
        request
    }
}

pub fn token_is_redacted(value: &str) -> bool {
    value.trim().is_empty() || value.trim() == HOME_ASSISTANT_TOKEN_REDACTION
}

pub fn redact_home_assistant_token(value: &str) -> String {
    if value.trim().is_empty() {
        String::new()
    } else {
        HOME_ASSISTANT_TOKEN_REDACTION.to_string()
    }
}

pub fn normalize_home_assistant_service_action_request(
    request: &HomeAssistantServiceActionRequest,
) -> HomeAssistantServiceActionRequest {
    HomeAssistantServiceActionRequest {
        entity_id: request.entity_id.trim().to_lowercase(),
        domain: request.domain.trim().to_lowercase(),
        service: request.service.trim().to_lowercase(),
        fields: normalize_home_assistant_service_fields(&request.fields),
    }
}

pub fn validate_home_assistant_service_action_request(
    request: &HomeAssistantServiceActionRequest,
    enabled: bool,
    exposed_domains: &[String],
) -> Result<(), String> {
    if !enabled {
        return Err("Home Assistant integration is disabled".to_string());
    }
    if request.entity_id.is_empty() || request.domain.is_empty() || request.service.is_empty() {
        return Err("Home Assistant entity, domain, and service are required".to_string());
    }
    if !request
        .entity_id
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '_' | '.'))
    {
        return Err("Home Assistant entity id is outside the safe identifier shape".to_string());
    }
    if !request
        .entity_id
        .starts_with(&format!("{}.", request.domain))
    {
        return Err("Home Assistant entity id must match the requested domain".to_string());
    }
    if !exposed_domains.is_empty()
        && !exposed_domains
            .iter()
            .any(|domain| domain.trim().eq_ignore_ascii_case(&request.domain))
    {
        return Err("Home Assistant domain is not in the allowlisted sync scope".to_string());
    }
    if !home_assistant_service_action_is_allowlisted(&request.domain, &request.service) {
        return Err("Home Assistant service is not allowlisted for safe smoke control".to_string());
    }
    validate_home_assistant_service_fields(&request.fields)?;
    Ok(())
}

pub fn home_assistant_service_action_is_allowlisted(domain: &str, service: &str) -> bool {
    match (
        domain.trim().to_lowercase().as_str(),
        service.trim().to_lowercase().as_str(),
    ) {
        ("light" | "switch" | "input_boolean", "turn_on" | "turn_off" | "toggle") => true,
        ("scene", "turn_on") => true,
        _ => false,
    }
}

pub fn normalize_home_assistant_service_fields(fields: &Value) -> Value {
    if fields.is_null() {
        return json!({});
    }
    fields.clone()
}

pub fn validate_home_assistant_service_fields(fields: &Value) -> Result<(), String> {
    if fields.is_null() {
        return Ok(());
    }
    let Some(object) = fields.as_object() else {
        return Err("Home Assistant service fields must be a JSON object".to_string());
    };
    for (key, value) in object {
        if home_assistant_secret_like_key(key) {
            return Err(
                "Home Assistant service fields cannot include secret-like keys".to_string(),
            );
        }
        let serialized = serde_json::to_string(value).unwrap_or_default();
        if home_assistant_secret_like_value(&serialized) {
            return Err(
                "Home Assistant service fields cannot include secret-like values".to_string(),
            );
        }
    }
    Ok(())
}

pub fn normalize_base_url(value: &str) -> Result<Url, String> {
    let trimmed = value.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("Home Assistant base URL is required".to_string());
    }
    let url =
        Url::parse(trimmed).map_err(|error| format!("invalid Home Assistant base URL: {error}"))?;
    match url.scheme() {
        "http" | "https" => Ok(url),
        scheme => Err(format!(
            "unsupported Home Assistant URL scheme {scheme}; expected http or https"
        )),
    }
}

fn normalize_entity(raw: RawHomeAssistantEntity) -> HomeAssistantEntity {
    let domain = raw
        .entity_id
        .split_once('.')
        .map(|(domain, _)| domain.to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let display_name = raw
        .attributes
        .get("friendly_name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| raw.entity_id.clone());
    let area_id = raw
        .attributes
        .get("area_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let device_class = raw
        .attributes
        .get("device_class")
        .and_then(Value::as_str)
        .map(str::to_string);

    HomeAssistantEntity {
        entity_id: raw.entity_id,
        domain,
        state: raw.state,
        display_name,
        area_id,
        device_class,
        last_changed: raw.last_changed,
        last_updated: raw.last_updated,
        attributes: raw.attributes,
    }
}

fn normalize_service_domain(raw: RawHomeAssistantServiceDomain) -> HomeAssistantServiceDomain {
    let mut services = Vec::new();
    if let Some(map) = raw.services.as_object() {
        for (service, value) in map {
            services.push(HomeAssistantService {
                service: service.clone(),
                name: value
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                description: value
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                fields: value.get("fields").cloned().unwrap_or_else(|| json!({})),
            });
        }
    }
    services.sort_by(|left, right| left.service.cmp(&right.service));
    HomeAssistantServiceDomain {
        domain: raw.domain,
        services,
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

fn format_harborlink_status_error(response: Response, operation: &str) -> String {
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
                "{operation} failed via HarborLink: {code} from {dependency}; retryable={retryable} (HTTP {})",
                status.as_u16()
            );
        }
    }
    format!(
        "{operation} failed via HarborLink (HTTP {})",
        status.as_u16()
    )
}

fn format_home_assistant_status_error(status: StatusCode) -> String {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            "Home Assistant token was rejected".to_string()
        }
        StatusCode::NOT_FOUND => "Home Assistant API endpoint was not found".to_string(),
        _ => format!("Home Assistant returned HTTP {status}"),
    }
}

fn sanitize_service_path_component(value: &str, label: &str) -> Result<String, String> {
    let trimmed = value.trim().to_lowercase();
    if trimmed.is_empty() {
        return Err(format!("Home Assistant {label} is required"));
    }
    if !trimmed
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
    {
        return Err(format!(
            "Home Assistant {label} contains unsupported characters"
        ));
    }
    Ok(trimmed)
}

fn home_assistant_secret_like_key(key: &str) -> bool {
    let normalized = key.trim().to_lowercase();
    [
        "token",
        "secret",
        "password",
        "passwd",
        "api_key",
        "apikey",
        "authorization",
        "bearer",
        "private_key",
        "access_key",
        "credential",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

fn home_assistant_secret_like_value(value: &str) -> bool {
    let normalized = value.trim().to_lowercase();
    normalized.contains("bearer ")
        || normalized.contains("authorization")
        || normalized.contains("api_key")
        || normalized.contains("apikey")
        || normalized.contains("access_token")
        || normalized.contains("private_key")
        || normalized.contains("-----begin ")
        || normalized.contains("rtsp://")
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::{
        home_assistant_service_action_is_allowlisted, normalize_base_url,
        normalize_home_assistant_service_action_request, redact_home_assistant_token,
        sanitize_service_path_component, token_is_redacted,
        validate_home_assistant_service_action_request, validate_home_assistant_service_fields,
        HomeAssistantClientConfig, HomeAssistantServiceActionRequest,
        HOME_ASSISTANT_TOKEN_REDACTION,
    };

    #[test]
    fn config_summary_redacts_token_material() {
        let config = HomeAssistantClientConfig::new("http://ha.local:8123/", "secret-token");

        let summary = config.redacted_summary();

        assert_eq!(summary.base_url, "http://ha.local:8123");
        assert!(summary.configured);
        assert!(summary.token_configured);
        assert_eq!(
            redact_home_assistant_token("secret-token"),
            HOME_ASSISTANT_TOKEN_REDACTION
        );
    }

    #[test]
    fn token_redaction_marker_is_secret_preserving() {
        assert!(token_is_redacted(""));
        assert!(token_is_redacted(HOME_ASSISTANT_TOKEN_REDACTION));
        assert!(!token_is_redacted("new-token"));
    }

    #[test]
    fn base_url_requires_http_scheme() {
        assert!(normalize_base_url("http://127.0.0.1:8123").is_ok());
        assert!(normalize_base_url("https://ha.example.test").is_ok());
        assert!(normalize_base_url("ws://ha.example.test").is_err());
    }

    #[test]
    fn raw_entity_shape_normalizes_friendly_name() {
        let raw = super::RawHomeAssistantEntity {
            entity_id: "light.kitchen".to_string(),
            state: "on".to_string(),
            attributes: json!({"friendly_name": "Kitchen", "device_class": "light"}),
            last_changed: Some("2026-05-09T01:02:03Z".to_string()),
            last_updated: None,
        };

        let entity = super::normalize_entity(raw);

        assert_eq!(entity.domain, "light");
        assert_eq!(entity.display_name, "Kitchen");
        assert_eq!(entity.device_class.as_deref(), Some("light"));
    }

    #[test]
    fn service_path_components_are_narrowly_validated() {
        assert_eq!(
            sanitize_service_path_component("Light", "domain").expect("valid"),
            "light"
        );
        assert!(sanitize_service_path_component("../config", "domain").is_err());
        assert!(sanitize_service_path_component("turn-on", "service").is_err());
    }

    #[test]
    fn service_action_allowlist_is_low_risk_only() {
        assert!(home_assistant_service_action_is_allowlisted(
            "light", "turn_on"
        ));
        assert!(home_assistant_service_action_is_allowlisted(
            "switch", "toggle"
        ));
        assert!(home_assistant_service_action_is_allowlisted(
            "input_boolean",
            "turn_off"
        ));
        assert!(home_assistant_service_action_is_allowlisted(
            "scene", "turn_on"
        ));
        assert!(!home_assistant_service_action_is_allowlisted(
            "lock", "unlock"
        ));
        assert!(!home_assistant_service_action_is_allowlisted(
            "climate",
            "set_temperature"
        ));
    }

    #[test]
    fn service_action_validation_rejects_scope_and_secret_fields() {
        let request = HomeAssistantServiceActionRequest {
            entity_id: "Light.Kitchen".to_string(),
            domain: "LIGHT".to_string(),
            service: "TURN_ON".to_string(),
            fields: Value::Null,
        };
        let normalized = normalize_home_assistant_service_action_request(&request);
        assert_eq!(normalized.entity_id, "light.kitchen");
        assert_eq!(normalized.fields, json!({}));
        validate_home_assistant_service_action_request(&normalized, true, &["light".to_string()])
            .expect("low risk action allowed");

        let unsafe_request = HomeAssistantServiceActionRequest {
            entity_id: "lock.front_door".to_string(),
            domain: "lock".to_string(),
            service: "unlock".to_string(),
            fields: json!({}),
        };
        assert!(validate_home_assistant_service_action_request(
            &unsafe_request,
            true,
            &["light".to_string(), "lock".to_string()],
        )
        .is_err());

        assert!(validate_home_assistant_service_fields(&json!({
            "api_token": "secret"
        }))
        .is_err());
        assert!(validate_home_assistant_service_fields(&json!({
            "message": "Bearer abcdef"
        }))
        .is_err());
    }
}
