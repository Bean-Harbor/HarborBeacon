//! Home Assistant REST connector.

use std::fs;
use std::time::Duration;

use reqwest::blocking::{Client, Response};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::runtime::startup::StartupProfile;

const DEFAULT_TIMEOUT_SECONDS: u64 = 8;
const DEFAULT_HARBORLINK_MEDIA_API_URL: &str = "http://127.0.0.1:8790";
const DEFAULT_HARBORLINK_LOCAL_API_TOKEN_FILE: &str =
    "/run/credentials/harboros-beacon.service/harborlink-local-api-token";
const HARBORLINK_CONTRACT_VERSION: &str = "1.0";
const HARBORLINK_CUTOVER_MODE: &str = "harborlink";
pub const HOME_ASSISTANT_TOKEN_REDACTION: &str = "__harbor_redacted__";

#[derive(Debug, Clone)]
pub struct HomeAssistantClient {
    base_url: Url,
    local_api_token: Option<String>,
    http: Client,
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
#[serde(deny_unknown_fields)]
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

#[derive(Debug)]
pub struct HomeAssistantActionOutcome {
    pub status: &'static str,
    pub allowed: bool,
    pub executed: bool,
    pub message: String,
    pub result: Option<HomeAssistantServiceCallResponse>,
}

impl HomeAssistantActionOutcome {
    pub fn blocked(message: String) -> Self {
        Self {
            status: "blocked",
            allowed: false,
            executed: false,
            message,
            result: None,
        }
    }
}

impl HomeAssistantClient {
    pub fn from_harborlink_env() -> Result<Self, String> {
        match StartupProfile::from_env()? {
            StartupProfile::N1 => Self::from_harborlink_env_with_auth(false),
            StartupProfile::N2 => Self::from_authenticated_harborlink_env(),
        }
    }

    pub fn from_authenticated_harborlink_env() -> Result<Self, String> {
        Self::from_harborlink_env_with_auth(true)
    }

    fn from_harborlink_env_with_auth(require_token: bool) -> Result<Self, String> {
        require_harborlink_cutover()?;
        let local_api_token = read_local_api_token_from_env()?;
        if require_token && local_api_token.is_none() {
            return Err("HarborLink local API credential is unavailable".to_string());
        }
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
            local_api_token,
            http,
        })
    }

    pub fn test_connection(&self) -> HomeAssistantConnectionTest {
        match self.post_json("/v1/home-assistant/test", &json!({})) {
            Ok(test) => test,
            Err(error) => HomeAssistantConnectionTest {
                ok: false,
                status: "error".to_string(),
                location_name: None,
                version: None,
                error: Some(error),
            },
        }
    }

    pub fn fetch_entities(&self) -> Result<Vec<HomeAssistantEntity>, String> {
        self.get_json("/v1/home-assistant/entities")
    }

    pub fn fetch_services(&self) -> Result<Vec<HomeAssistantServiceDomain>, String> {
        self.get_json("/v1/home-assistant/services")
    }

    pub fn check_service_action(
        &self,
        request: &HomeAssistantServiceActionRequest,
    ) -> Result<(), String> {
        let entities = self.fetch_entities().map_err(|_| {
            "Home Assistant entities are unavailable; no action was sent".to_string()
        })?;
        let services = self.fetch_services().map_err(|_| {
            "Home Assistant actions are unavailable; no action was sent".to_string()
        })?;
        validate_home_assistant_action_capability(request, &entities, &services)
    }

    pub fn execute_checked_action(
        &self,
        request: &HomeAssistantServiceActionRequest,
    ) -> HomeAssistantActionOutcome {
        if let Err(message) = self.check_service_action(request) {
            return HomeAssistantActionOutcome::blocked(message);
        }
        // Once dispatched, an unreadable response cannot prove that no action occurred.
        let result = self.call_service(
            &request.domain,
            &request.service,
            &request.entity_id,
            Some(&request.fields),
        );
        let (status, message, result) = match result {
            Ok(result)
                if result.domain == request.domain
                    && result.service == request.service
                    && result.entity_id == request.entity_id =>
            {
                if result.ok {
                    (
                        "succeeded",
                        "Home Assistant accepted the action; device state is not yet confirmed",
                        Some(result),
                    )
                } else {
                    (
                        "failed",
                        "Home Assistant did not accept the action",
                        Some(result),
                    )
                }
            }
            _ => (
                "unknown",
                "Home Assistant action outcome is unknown; check the device before trying again",
                None,
            ),
        };
        HomeAssistantActionOutcome {
            status,
            allowed: true,
            executed: true,
            message: message.into(),
            result,
        }
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
        let path = format!("/v1/home-assistant/services/{domain}/{service}");
        let body = fields
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        self.post_json(
            &path,
            &json!({
                "entity_id": entity_id,
                "fields": Value::Object(body),
            }),
        )
    }

    fn get_json<T: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<T, String> {
        let url = self
            .base_url
            .join(path.trim_start_matches('/'))
            .map_err(|error| format!("invalid Home Assistant endpoint {path}: {error}"))?;
        let request = self.harborlink_request(self.http.get(url), None);
        let response = request
            .send()
            .map_err(|error| format!("Home Assistant request failed: {error}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format_harborlink_status_error(
                response,
                "Home Assistant request",
            ));
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
        let request_id = format!("beacon-{}", uuid::Uuid::new_v4().simple());
        let request = self.harborlink_request(self.http.post(url).json(body), Some(&request_id));
        let response = request
            .send()
            .map_err(|error| format!("Home Assistant request failed: {error}"))?;
        let status = response.status();
        if !status.is_success() {
            return Err(format_harborlink_status_error(
                response,
                "Home Assistant request",
            ));
        }
        response
            .json::<T>()
            .map_err(|error| format!("failed to parse Home Assistant response: {error}"))
    }

    fn harborlink_request(
        &self,
        request: reqwest::blocking::RequestBuilder,
        request_id: Option<&str>,
    ) -> reqwest::blocking::RequestBuilder {
        let mut request =
            request.header("X-HarborLink-Contract-Version", HARBORLINK_CONTRACT_VERSION);
        if let Some(token) = self.local_api_token.as_deref() {
            request = request.bearer_auth(token);
        }
        if let Some(request_id) = request_id {
            request = request.header("X-Request-Id", request_id);
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

pub fn validate_home_assistant_action_capability(
    request: &HomeAssistantServiceActionRequest,
    entities: &[HomeAssistantEntity],
    services: &[HomeAssistantServiceDomain],
) -> Result<(), String> {
    validate_home_assistant_service_action_request(request, true, &[])?;
    let entity = entities
        .iter()
        .find(|entity| entity.entity_id == request.entity_id)
        .ok_or_else(|| {
            "Home Assistant entity no longer exists or is outside the allowed scope".to_string()
        })?;
    if entity.domain != request.domain || matches!(entity.state.as_str(), "unavailable" | "unknown")
    {
        return Err("Home Assistant entity is unavailable or its capability changed".into());
    }
    let service = services
        .iter()
        .filter(|domain| domain.domain == request.domain)
        .flat_map(|domain| &domain.services)
        .find(|service| service.service == request.service)
        .ok_or_else(|| {
            "Home Assistant action is no longer available; review the device action".to_string()
        })?;
    if let Some(fields) = request.fields.as_object() {
        for key in fields.keys() {
            if !service
                .fields
                .as_object()
                .is_some_and(|schema| schema.contains_key(key))
            {
                return Err("Home Assistant action contains an unsupported parameter".into());
            }
        }
    }
    Ok(())
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
        if matches!(
            key.as_str(),
            "entity_id" | "device_id" | "area_id" | "target" | "floor_id" | "label_id"
        ) {
            return Err(
                "Home Assistant action parameters cannot override the selected entity".into(),
            );
        }
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
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use serde_json::{json, Value};
    use tiny_http::{Response, Server};

    use super::{
        home_assistant_service_action_is_allowlisted, normalize_base_url,
        normalize_home_assistant_service_action_request, sanitize_service_path_component,
        token_is_redacted, validate_home_assistant_service_action_request,
        validate_home_assistant_service_fields, HomeAssistantClient,
        HomeAssistantServiceActionRequest, HOME_ASSISTANT_TOKEN_REDACTION,
    };

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

    #[test]
    fn action_preflight_uses_current_inventory_and_service_schema() {
        let request = HomeAssistantServiceActionRequest {
            entity_id: "light.desk".into(),
            domain: "light".into(),
            service: "turn_on".into(),
            fields: json!({}),
        };
        let mut entities: Vec<super::HomeAssistantEntity> = serde_json::from_value(json!([{
            "entity_id":"light.desk", "domain":"light", "state":"off", "display_name":"Desk"
        }]))
        .unwrap();
        let services: Vec<super::HomeAssistantServiceDomain> = serde_json::from_value(json!([{
            "domain":"light", "services":[{"service":"turn_on", "fields":{}}]
        }]))
        .unwrap();
        assert!(
            super::validate_home_assistant_action_capability(&request, &entities, &services)
                .is_ok()
        );
        assert!(
            super::validate_home_assistant_action_capability(&request, &[], &services).is_err()
        );
        assert!(
            super::validate_home_assistant_action_capability(&request, &entities, &[]).is_err()
        );
        entities[0].state = "unavailable".into();
        assert!(
            super::validate_home_assistant_action_capability(&request, &entities, &services)
                .is_err()
        );
        entities[0].state = "off".into();
        let mut unsupported = request;
        unsupported.fields = json!({"invented_parameter": true});
        assert!(super::validate_home_assistant_action_capability(
            &unsupported,
            &entities,
            &services
        )
        .is_err());
    }

    #[test]
    fn action_parameters_cannot_widen_the_selected_target() {
        for key in [
            "entity_id",
            "device_id",
            "area_id",
            "target",
            "floor_id",
            "label_id",
        ] {
            let fields = json!({key: "all"});
            assert!(
                validate_home_assistant_service_fields(&fields).is_err(),
                "{key}"
            );
        }
        assert!(serde_json::from_value::<HomeAssistantServiceActionRequest>(json!({
            "entity_id":"light.desk", "domain":"light", "service":"turn_on", "target":{"area_id":"all"}
        })).is_err());
    }

    const FACTORY_CHILD: &str = "HARBOR_HA_FACTORY_TEST_CHILD";
    const LINK_TOKEN: &str = "synthetic_link_token_0123456789abcdef0123456789abcdef";

    struct FactoryFixture(PathBuf);

    impl FactoryFixture {
        fn new() -> Self {
            let path = env::temp_dir().join(format!("harbor-ha-auth-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for FactoryFixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn assert_factory_scenario(scenario: &str, rejection: Option<&str>) {
        let fixture = FactoryFixture::new();
        let server = Server::http("127.0.0.1:0").unwrap();
        let address = server.server_addr().to_ip().unwrap();
        let (stop, stopped) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut requests = Vec::new();
            while stopped.try_recv().is_err() {
                let Some(request) = server.recv_timeout(Duration::from_millis(20)).unwrap() else {
                    continue;
                };
                let header = |name: &str| {
                    request
                        .headers()
                        .iter()
                        .find(|header| header.field.to_string().eq_ignore_ascii_case(name))
                        .map(|header| header.value.to_string())
                };
                requests.push((
                    request.method().as_str().to_string(),
                    request.url().to_string(),
                    header("Authorization"),
                    header("X-HarborLink-Contract-Version"),
                    header("X-Request-Id"),
                ));
                let body = if request.method().as_str() == "GET" {
                    json!([])
                } else {
                    json!({"domain":"light", "service":"turn_on", "entity_id":"light.fixture", "ok":true})
                };
                request
                    .respond(Response::from_string(body.to_string()))
                    .unwrap();
            }
            requests
        });
        // Each child receives synthetic configuration without mutating the test process env.
        let mut command = Command::new(env::current_exe().unwrap());
        command.args([
            "--exact",
            "connectors::home_assistant::tests::harborlink_factory_child",
            "--nocapture",
        ]);
        for key in [
            "HARBOR_BEACON_STARTUP_PROFILE",
            "HARBORBEACON_SOUTHBOUND_MODE",
            "HARBORLINK_MEDIA_API_URL",
            "HARBORLINK_LOCAL_API_TOKEN",
            "HARBORLINK_LOCAL_API_TOKEN_FILE",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            command.env_remove(key);
        }
        command
            .env(FACTORY_CHILD, rejection.unwrap_or(""))
            .env("NO_PROXY", "127.0.0.1,localhost")
            .env("HARBORBEACON_SOUTHBOUND_MODE", "harborlink")
            .env("HARBORLINK_MEDIA_API_URL", format!("http://{address}"))
            .env("HARBORLINK_LOCAL_API_TOKEN_FILE", fixture.0.join("token"));
        match scenario {
            "missing-token" => {}
            "env-token" => {
                command.env("HARBORLINK_LOCAL_API_TOKEN", LINK_TOKEN);
            }
            "file-token" => {
                fs::write(fixture.0.join("token"), LINK_TOKEN).unwrap();
            }
            "empty-token-file" => {
                fs::write(fixture.0.join("token"), "\n").unwrap();
            }
            "invalid-profile" => {
                command.env("HARBOR_BEACON_STARTUP_PROFILE", "invalid");
            }
            "wrong-product-profile" => {
                command.env(
                    "HARBOR_BEACON_STARTUP_PROFILE",
                    if cfg!(feature = "external-model-runtime") {
                        "n1"
                    } else {
                        "n2"
                    },
                );
            }
            "invalid-cutover" => {
                command.env("HARBORBEACON_SOUTHBOUND_MODE", "direct");
            }
            _ => panic!("unknown factory scenario"),
        }
        let output = command.output().unwrap();
        stop.send(()).unwrap();
        let requests = worker.join().unwrap();
        assert!(
            output.status.success(),
            "{scenario}: child failed, {} outbound requests:\n{}\n{}",
            requests.len(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        if rejection.is_some() {
            assert!(
                requests.is_empty(),
                "rejected factory sent an outbound request"
            );
        } else {
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[0].0, "GET");
            assert_eq!(requests[0].1, "/v1/home-assistant/entities");
            assert_eq!(requests[1].0, "POST");
            assert_eq!(requests[1].1, "/v1/home-assistant/services/light/turn_on");
            for request in &requests {
                assert_eq!(request.3.as_deref(), Some("1.0"));
                let expected =
                    (scenario != "missing-token").then(|| format!("Bearer {LINK_TOKEN}"));
                assert_eq!(request.2, expected);
            }
            assert!(requests[1]
                .4
                .as_deref()
                .is_some_and(|value| value.starts_with("beacon-")));
        }
    }

    #[test]
    fn harborlink_factory_child() {
        let Ok(rejection) = env::var(FACTORY_CHILD) else {
            return;
        };
        let result = HomeAssistantClient::from_harborlink_env();
        if rejection.is_empty() {
            let client = result.unwrap();
            assert!(client.fetch_entities().unwrap().is_empty());
            assert!(
                client
                    .call_service("light", "turn_on", "light.fixture", None)
                    .unwrap()
                    .ok
            );
        } else {
            if let Ok(client) = &result {
                // A permissive mock exposes an incorrectly enabled client, without relying on 401.
                let _ = client.fetch_entities();
            }
            let error = result.expect_err("factory must reject before network access");
            assert!(
                error.contains(&rejection),
                "unexpected factory error: {error}"
            );
            assert!(!error.contains(LINK_TOKEN));
        }
    }

    #[test]
    fn harborlink_factory_requires_token_for_n2_and_preserves_n1_compatibility() {
        let rejection = cfg!(feature = "external-model-runtime")
            .then_some("HarborLink local API credential is unavailable");
        assert_factory_scenario("missing-token", rejection);
    }

    #[test]
    fn harborlink_factory_sends_bearer_for_env_and_file_tokens() {
        assert_factory_scenario("env-token", None);
        assert_factory_scenario("file-token", None);
    }

    #[test]
    fn harborlink_factory_rejects_invalid_profile_before_network() {
        assert_factory_scenario("invalid-profile", Some("HARBOR_BEACON_STARTUP_PROFILE"));
        assert_factory_scenario(
            "wrong-product-profile",
            Some("HARBOR_BEACON_STARTUP_PROFILE"),
        );
    }

    #[test]
    fn harborlink_factory_rejects_invalid_cutover_and_empty_credentials_before_network() {
        assert_factory_scenario("invalid-cutover", Some("HARBORBEACON_SOUTHBOUND_MODE"));
        assert_factory_scenario("empty-token-file", Some("empty"));
    }
}
