//! K3 EVT readiness and redacted evidence helpers.

use std::env;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::blocking::Client;
use serde_json::{json, Value};

use crate::control_plane::models::{ModelEndpoint, ModelKind};
use crate::control_plane::models::{ModelEndpointKind, ModelEndpointStatus, PrivacyLevel};
use crate::runtime::admin_console::{
    AdminConsoleState, AdminConsoleStore, AdminModelCenterState, NotificationTargetRecord,
};
use crate::runtime::model_center::wire_semantic_router_resident_endpoint;
use crate::runtime::vision_event::{
    list_recent_local_vision_events_default, local_vision_event_store_stats_default,
};

pub const EVT_READINESS_PROFILE: &str = "k3-direct-72h-readiness";
pub const EVT_READINESS_EVIDENCE_KIND: &str = "evt_readiness_v1";
const SEMANTIC_ROUTER_SERVICE: &str = "semantic-router.service";
const SEMANTIC_ROUTER_HEALTHZ_URL_ENV: &str = "HARBOR_SEMANTIC_ROUTER_HEALTHZ_URL";
const DEFAULT_SEMANTIC_ROUTER_HEALTHZ_URL: &str = "http://127.0.0.1:4176/healthz";
const EVT_SERVICES: [&str; 5] = [
    "harboros-beacon.service",
    "harboros-im-gate.service",
    "nginx.service",
    "harborlink-dev-k3.service",
    SEMANTIC_ROUTER_SERVICE,
];

pub fn build_evt_readiness_report(
    store: &AdminConsoleStore,
    gateway_status: Option<&Value>,
) -> Result<Value, String> {
    let state = store.load_state()?;
    let camera_count = store
        .registry_store()
        .load_devices()
        .map(|devices| devices.len())
        .unwrap_or(0);
    let mut readiness = build_evt_readiness_from_state(&state, camera_count, gateway_status);
    if let Some(object) = readiness.as_object_mut() {
        object.insert(
            "admin_state".to_string(),
            serde_json::to_value(store.state_stats(Some(&state))).unwrap_or_else(|_| json!({})),
        );
    }
    Ok(readiness)
}

pub fn run_evt_preflight_report(
    store: &AdminConsoleStore,
    gateway_status: Option<&Value>,
) -> Result<Value, String> {
    let started_at = now_unix_string();
    let start = std::time::Instant::now();
    let readiness = build_evt_readiness_report(store, gateway_status)?;
    let status = readiness
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("degraded")
        .to_string();
    let blockers = readiness
        .get("blockers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let warnings = readiness
        .get("warnings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let service_checks = readiness
        .get("services")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|service| {
            json!({
                "check": format!(
                    "service:{}",
                    service
                        .get("service")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown")
                ),
                "status": if service
                    .get("active")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
                {
                    "passed"
                } else {
                    "warn"
                },
                "metadata": service,
            })
        })
        .collect::<Vec<_>>();
    let mut checks = vec![
        json!({
            "check": "long_run_guard",
            "status": "passed",
            "long_run_started": false,
            "short_run_started": false,
            "operator_supervisor_required": true,
        }),
        json!({
            "check": "gateway_default_target",
            "status": readiness
                .pointer("/gateway/status")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            "metadata": readiness.get("gateway").cloned().unwrap_or_else(|| json!({})),
        }),
        json!({
            "check": "home_assistant",
            "status": readiness
                .pointer("/home_assistant/status")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            "metadata": readiness
                .get("home_assistant")
                .cloned()
                .unwrap_or_else(|| json!({})),
        }),
        json!({
            "check": "camera_event_availability",
            "status": readiness
                .pointer("/camera/status")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            "metadata": readiness.get("camera").cloned().unwrap_or_else(|| json!({})),
        }),
        json!({
            "check": "semantic_router_local_only",
            "status": readiness
                .pointer("/models/semantic_router/local_only")
                .and_then(Value::as_bool)
                .map(|ready| if ready { "passed" } else { "blocked" })
                .unwrap_or("unknown"),
            "metadata": readiness
                .pointer("/models/semantic_router")
                .cloned()
                .unwrap_or_else(|| json!({})),
        }),
        json!({
            "check": "semantic_router_service",
            "status": readiness
                .pointer("/models/semantic_router/service/status")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            "metadata": readiness
                .pointer("/models/semantic_router/service")
                .cloned()
                .unwrap_or_else(|| json!({})),
        }),
        json!({
            "check": "semantic_router_endpoint",
            "status": readiness
                .pointer("/models/semantic_router/endpoint/status")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            "metadata": readiness
                .pointer("/models/semantic_router/endpoint")
                .cloned()
                .unwrap_or_else(|| json!({})),
        }),
        json!({
            "check": "secret_scan",
            "status": readiness
                .pointer("/security/secret_scan/status")
                .and_then(Value::as_str)
                .unwrap_or("unknown"),
            "metadata": readiness
                .pointer("/security/secret_scan")
                .cloned()
                .unwrap_or_else(|| json!({})),
        }),
    ];
    checks.extend(service_checks);
    Ok(json!({
        "kind": "evt_preflight_v1",
        "profile": EVT_READINESS_PROFILE,
        "status": status,
        "started_at": started_at,
        "completed_at": now_unix_string(),
        "duration_ms": start.elapsed().as_millis() as u64,
        "long_run_started": false,
        "short_run_started": false,
        "operator_supervisor_required": true,
        "blockers": blockers,
        "warnings": warnings,
        "checks": checks,
        "readiness": readiness,
        "redacted": true,
    }))
}

pub fn build_evt_evidence_bundle(
    readiness: Value,
    latest_preflight: Option<Value>,
    diagnostics_workflow: Value,
) -> Value {
    let preflight = latest_preflight.unwrap_or_else(|| {
        json!({
            "kind": "evt_preflight_v1",
            "profile": EVT_READINESS_PROFILE,
            "status": "not_run",
            "message": "No EVT preflight has been run in this Beacon API process.",
            "long_run_started": false,
            "short_run_started": false,
            "redacted": true,
        })
    });
    json!({
        "kind": EVT_READINESS_EVIDENCE_KIND,
        "generated_at": now_unix_string(),
        "profile": EVT_READINESS_PROFILE,
        "status": readiness
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("degraded")
            .to_string(),
        "readiness": readiness,
        "preflight": preflight,
        "diagnostics_workflow": diagnostics_workflow,
        "security": {
            "redacted": true,
            "secret_scan": "counts_only",
            "raw_media_included": false,
            "local_paths_included": false,
            "tokens_included": false,
        },
        "redacted": true,
    })
}

pub fn evt_readiness_workflow_summary(readiness: &Value) -> Value {
    json!({
        "kind": "evt_readiness",
        "profile": readiness
            .get("profile")
            .and_then(Value::as_str)
            .unwrap_or(EVT_READINESS_PROFILE),
        "status": readiness
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("degraded"),
        "blockers": readiness.get("blockers").cloned().unwrap_or_else(|| json!([])),
        "warnings": readiness.get("warnings").cloned().unwrap_or_else(|| json!([])),
        "semantic_router": evt_semantic_router_workflow_summary(readiness),
        "generated_at": readiness
            .get("generated_at")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        "redacted": true,
    })
}

pub fn evt_preflight_workflow_summary(preflight: Option<Value>) -> Value {
    preflight.unwrap_or_else(|| {
        json!({
            "kind": "evt_preflight",
            "profile": EVT_READINESS_PROFILE,
            "status": "not_run",
            "message": "No EVT preflight has been run in this Beacon API process.",
            "long_run_started": false,
            "short_run_started": false,
            "redacted": true,
        })
    })
}

pub fn evt_long_run_request_boundary(raw_text: &str) -> bool {
    let normalized = raw_text
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>()
        .to_ascii_lowercase();
    let asks_stress = ["压测", "stress", "burnin", "burn-in", "soak"]
        .iter()
        .any(|keyword| normalized.contains(keyword));
    let asks_start = ["开始", "启动", "运行", "run", "start", "执行"]
        .iter()
        .any(|keyword| normalized.contains(keyword));
    let asks_long = ["72小时", "72h", "4小时", "4h", "长跑", "长期"]
        .iter()
        .any(|keyword| normalized.contains(keyword));
    asks_stress && asks_start && asks_long
}

pub fn evt_status_reply_summary(readiness: &Value, preflight: Option<&Value>) -> String {
    let status = readiness
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("degraded");
    let blockers = readiness
        .get("blockers")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .take(3)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let preflight_time = preflight
        .and_then(|value| value.get("completed_at").and_then(Value::as_str))
        .or_else(|| preflight.and_then(|value| value.get("started_at").and_then(Value::as_str)))
        .unwrap_or("未运行");
    if blockers.is_empty() {
        format!("EVT 就绪状态：{status}；最近预检：{preflight_time}；证据包可生成。")
    } else {
        format!(
            "EVT 就绪状态：{status}；主要 blocker：{}；最近预检：{preflight_time}。",
            blockers.join("、")
        )
    }
}

pub fn evt_preflight_reply_summary(preflight: &Value) -> String {
    let status = preflight
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("degraded");
    let blocker_count = preflight
        .get("blockers")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let duration = preflight
        .get("duration_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    format!(
        "EVT 预检完成：{status}，blocker {blocker_count} 个，用时 {duration}ms；未启动 4h/72h 长压测。"
    )
}

pub fn evt_evidence_reply_summary(bundle: &Value) -> String {
    let status = bundle
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("degraded");
    format!(
        "EVT 脱敏证据包已生成：{status}；只包含摘要和计数，不含 token、RTSP、本地路径或原始图像。"
    )
}

fn build_evt_readiness_from_state(
    state: &AdminConsoleState,
    camera_count: usize,
    gateway_status: Option<&Value>,
) -> Value {
    let services = EVT_SERVICES
        .iter()
        .map(|service| systemd_service_summary(service))
        .collect::<Vec<_>>();
    let latest_events = list_recent_local_vision_events_default(3).unwrap_or_default();
    let event_store_stats = local_vision_event_store_stats_default()
        .map(|stats| serde_json::to_value(stats).unwrap_or_else(|_| json!({})))
        .unwrap_or_else(|error| {
            json!({
                "status": "unavailable",
                "error": redact_evt_text(&error),
                "metadata_only": true,
            })
        });
    let latest_event = latest_events.first();
    let package = package_summary("harboros-beacon");
    let gateway = evt_gateway_readiness(&state.notification_targets, gateway_status);
    let home_assistant = evt_home_assistant_readiness(state);
    let camera = json!({
        "status": if camera_count > 0 { "available" } else { "blocked" },
        "configured_count": camera_count,
        "latest_event_available": latest_event.is_some(),
        "latest_event_id": latest_event.map(|event| event.event.event_id.clone()),
        "event_store": event_store_stats,
        "metadata_only": true,
        "rtsp_urls_redacted": true,
        "local_paths_redacted": true,
    });
    let models = evt_model_policy_readiness(state);
    let resources = resource_summary();
    let security = secret_scan_summary();
    let mut blockers = Vec::new();
    let mut warnings = Vec::new();

    if !gateway
        .get("default_target_ready")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        blockers.push("default_notification_target_or_gate_unavailable".to_string());
    }
    if !home_assistant
        .get("configured")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        warnings.push("home_assistant_not_configured".to_string());
    } else if !home_assistant
        .get("enabled")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        warnings.push("home_assistant_disabled".to_string());
    }
    if camera_count == 0 {
        blockers.push("no_camera_configured".to_string());
    } else if latest_event.is_none() {
        warnings.push("no_recent_camera_event".to_string());
    }
    if !models
        .pointer("/semantic_router/local_only")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        blockers.push("semantic_router_not_local_only".to_string());
    }
    if !models
        .pointer("/semantic_router/available")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        warnings.push("semantic_router_not_active".to_string());
    }
    if !models
        .pointer("/semantic_router/endpoint_ready")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        warnings.push("semantic_router_endpoint_not_ready".to_string());
    }
    if security
        .pointer("/secret_scan/total_count")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        > 0
    {
        blockers.push("secret_scan_detected_sensitive_pattern".to_string());
    }
    if resources
        .pointer("/disk/root/use_percent")
        .and_then(Value::as_u64)
        .is_some_and(|percent| percent >= 90)
    {
        blockers.push("root_disk_critical".to_string());
    }
    if resources
        .pointer("/memory/available_percent")
        .and_then(Value::as_u64)
        .is_some_and(|percent| percent < 10)
    {
        blockers.push("memory_available_critical".to_string());
    }
    if resources
        .pointer("/thermal/max_celsius")
        .and_then(Value::as_f64)
        .is_some_and(|temp| temp >= 80.0)
    {
        blockers.push("temperature_critical".to_string());
    }
    if resources
        .pointer("/dmesg_risk/count")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        > 0
    {
        warnings.push("recent_kernel_risk_signals_detected".to_string());
    }
    for service in &services {
        if service
            .get("active")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        let name = service
            .get("service")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        match name {
            "harboros-beacon.service" | "nginx.service" => {
                blockers.push(format!("service_inactive:{name}"));
            }
            _ => warnings.push(format!("service_inactive:{name}")),
        }
    }
    let status = if !blockers.is_empty() {
        "blocked"
    } else if !warnings.is_empty() {
        "degraded"
    } else {
        "ready"
    };
    json!({
        "kind": "evt_readiness_v1",
        "profile": EVT_READINESS_PROFILE,
        "generated_at": now_unix_string(),
        "status": status,
        "summary": format!("EVT readiness is {status}."),
        "blockers": blockers,
        "warnings": warnings,
        "package": package,
        "services": services,
        "gateway": gateway,
        "default_notification_target": evt_default_target_readiness(&state.notification_targets, gateway_status),
        "home_assistant": home_assistant,
        "camera": camera,
        "models": models,
        "resources": resources,
        "security": security,
        "guards": {
            "long_run_start_allowed": false,
            "operator_supervisor_required": true,
            "preflight_max_seconds": 90,
            "starts_4h_or_72h": false,
        },
        "redacted": true,
    })
}

fn evt_gateway_readiness(
    targets: &[NotificationTargetRecord],
    gateway_status: Option<&Value>,
) -> Value {
    let connected = gateway_connected(gateway_status);
    let configured = gateway_configured(gateway_status);
    let default_target = targets.iter().find(|target| target.is_default);
    let status = if connected && default_target.is_some() {
        "available"
    } else if configured || default_target.is_some() {
        "degraded"
    } else {
        "blocked"
    };
    json!({
        "status": status,
        "configured": configured,
        "connected": connected,
        "default_target_ready": connected && default_target.is_some(),
        "default_target_configured": default_target.is_some(),
        "default_target_label": default_target.map(|target| target.label.clone()),
        "platform_hint": default_target.and_then(|target| non_empty_string(&target.platform_hint)),
        "route_key_redacted": default_target.is_some(),
        "context_token_redacted": gateway_status.is_some(),
        "redacted": true,
    })
}

fn evt_default_target_readiness(
    targets: &[NotificationTargetRecord],
    gateway_status: Option<&Value>,
) -> Value {
    let default_target = targets.iter().find(|target| target.is_default);
    let connected = gateway_connected(gateway_status);
    json!({
        "status": if default_target.is_some() && connected {
            "available"
        } else if default_target.is_some() {
            "degraded"
        } else {
            "not_configured"
        },
        "target_configured": default_target.is_some(),
        "target_label": default_target.map(|target| target.label.clone()),
        "gateway_connected": connected,
        "route_key_redacted": default_target.is_some(),
        "redacted": true,
    })
}

fn evt_home_assistant_readiness(state: &AdminConsoleState) -> Value {
    let configured = !state.home_assistant.base_url.trim().is_empty()
        && !state.home_assistant.access_token.trim().is_empty();
    json!({
        "configured": configured,
        "enabled": state.home_assistant.enabled,
        "status": state.home_assistant.last_status,
        "last_sync_at": state.home_assistant.last_sync_at,
        "entity_count": state.home_assistant.entity_count,
        "service_count": state.home_assistant.service_count,
        "exposed_domains": state.home_assistant.exposed_domains,
        "token_configured": !state.home_assistant.access_token.trim().is_empty(),
        "token_redacted": !state.home_assistant.access_token.trim().is_empty(),
        "base_url_redacted": configured,
        "redacted": true,
    })
}

fn evt_model_policy_readiness(state: &AdminConsoleState) -> Value {
    let policy = state
        .models
        .route_policies
        .iter()
        .find(|policy| policy.route_policy_id == "semantic.router");
    let mut runtime_models = state.models.clone();
    wire_semantic_router_resident_endpoint(&mut runtime_models);
    let service = systemd_service_summary(SEMANTIC_ROUTER_SERVICE);
    let service_active = service
        .get("active")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let endpoint = semantic_router_endpoint_candidate(&runtime_models);
    let endpoint_health = semantic_router_endpoint_health(endpoint);
    let endpoint_ready = endpoint_health
        .get("ready")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let local_only = policy
        .map(|policy| {
            policy.privacy_level == PrivacyLevel::StrictLocal
                && policy
                    .fallback_order
                    .iter()
                    .all(|item| !item.to_ascii_lowercase().contains("cloud"))
        })
        .unwrap_or(true);
    let available = local_only && service_active && endpoint_ready;
    json!({
        "semantic_router": {
            "status": if available {
                "active"
            } else if endpoint_ready {
                "endpoint_ready"
            } else if endpoint.is_some() {
                "configured"
            } else if service_active {
                "service_active"
            } else if policy.is_some() {
                "policy_configured"
            } else {
                "unknown"
            },
            "available": available,
            "service_active": service_active,
            "endpoint_ready": endpoint_ready,
            "service": service,
            "endpoint": endpoint_health,
            "resident_service": {
                "service": SEMANTIC_ROUTER_SERVICE,
                "required_for_evt": true,
                "loopback_only": true,
                "healthz_url_redacted": true,
            },
            "local_only": local_only,
            "cloud_fallback_allowed": !local_only,
            "policy_configured": policy.is_some(),
            "endpoint_id": endpoint.map(|endpoint| endpoint.model_endpoint_id.clone()),
            "model_name": endpoint.map(|endpoint| endpoint.model_name.clone()),
            "provider_key": endpoint.map(|endpoint| endpoint.provider_key.clone()),
        },
        "model_policy": {
            "semantic_router_policy": "semantic.router",
            "local_only": local_only,
            "cloud_fallback_allowed": !local_only,
        },
        "redacted": true,
    })
}

fn semantic_router_endpoint_candidate(state: &AdminModelCenterState) -> Option<&ModelEndpoint> {
    let mut candidates = state
        .endpoints
        .iter()
        .filter(|endpoint| {
            endpoint.model_kind == ModelKind::Llm
                && endpoint.endpoint_kind == ModelEndpointKind::Local
                && endpoint.status != ModelEndpointStatus::Disabled
        })
        .filter(|endpoint| semantic_router_endpoint_rank(endpoint).0 < 3)
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        semantic_router_endpoint_rank(left).cmp(&semantic_router_endpoint_rank(right))
    });
    candidates.into_iter().next()
}

fn semantic_router_endpoint_rank(endpoint: &ModelEndpoint) -> (u8, u8, String) {
    let is_default = endpoint.model_endpoint_id == "llm-local-openai-compatible";
    let semantic = endpoint
        .capability_tags
        .iter()
        .any(|tag| matches_semantic_router_tag(tag))
        || endpoint
            .metadata
            .get("semantic_router")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let category = if semantic && !is_default {
        0
    } else if semantic {
        1
    } else if is_default {
        2
    } else {
        3
    };
    let status = match endpoint.status {
        ModelEndpointStatus::Active => 0,
        ModelEndpointStatus::Degraded => 1,
        ModelEndpointStatus::Disabled => 2,
    };
    (category, status, endpoint.model_endpoint_id.clone())
}

fn semantic_router_endpoint_health(endpoint: Option<&ModelEndpoint>) -> Value {
    let Some(endpoint) = endpoint else {
        return json!({
            "status": "no_endpoint",
            "ready": false,
            "model_name": Value::Null,
            "base_url_redacted": true,
            "healthz_url_redacted": true,
            "redacted": true,
        });
    };
    let healthz_url = endpoint
        .metadata
        .get("healthz_url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            env::var(SEMANTIC_ROUTER_HEALTHZ_URL_ENV)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| DEFAULT_SEMANTIC_ROUTER_HEALTHZ_URL.to_string());
    let client = match Client::builder().timeout(Duration::from_secs(3)).build() {
        Ok(client) => client,
        Err(error) => {
            return json!({
                "status": "unavailable",
                "ready": false,
                "model_name": endpoint.model_name.clone(),
                "error": redact_evt_text(&error.to_string()),
                "base_url_redacted": true,
                "healthz_url_redacted": true,
                "redacted": true,
            })
        }
    };
    let response = match client.get(healthz_url).send() {
        Ok(response) => response,
        Err(error) => {
            return json!({
                "status": "unavailable",
                "ready": false,
                "model_name": endpoint.model_name.clone(),
                "error": redact_evt_text(&error.to_string()),
                "base_url_redacted": true,
                "healthz_url_redacted": true,
                "redacted": true,
            })
        }
    };
    let http_status = response.status().as_u16();
    let body = match response.text() {
        Ok(body) => body,
        Err(error) => {
            return json!({
                "status": "unavailable",
                "ready": false,
                "http_status": http_status,
                "model_name": endpoint.model_name.clone(),
                "error": redact_evt_text(&error.to_string()),
                "base_url_redacted": true,
                "healthz_url_redacted": true,
                "redacted": true,
            })
        }
    };
    let payload = serde_json::from_str::<Value>(&body).unwrap_or_else(|_| json!({}));
    let service_ready = payload
        .get("ready")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let backend_ready = payload
        .pointer("/backend/ready")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let ready = (200..300).contains(&http_status) && service_ready && backend_ready;
    json!({
        "status": if ready {
            "ready"
        } else if (200..300).contains(&http_status) {
            "degraded"
        } else {
            "unavailable"
        },
        "ready": ready,
        "http_status": http_status,
        "service_ready": service_ready,
        "backend_ready": backend_ready,
        "backend_kind": payload.pointer("/backend/kind").cloned().unwrap_or(Value::Null),
        "model_loaded": payload.pointer("/backend/model_loaded").cloned().unwrap_or(Value::Null),
        "service_reported": payload.get("service").cloned().unwrap_or(Value::Null),
        "health_status": payload.get("status").cloned().unwrap_or(Value::Null),
        "model_name": endpoint.model_name.clone(),
        "base_url_redacted": true,
        "healthz_url_redacted": true,
        "bind_redacted": true,
        "redacted": true,
    })
}

fn evt_semantic_router_workflow_summary(readiness: &Value) -> Value {
    let router = readiness
        .pointer("/models/semantic_router")
        .cloned()
        .unwrap_or_else(|| json!({}));
    json!({
        "kind": "semantic_router_resident",
        "status": router.get("status").cloned().unwrap_or(Value::Null),
        "service_active": router.get("service_active").cloned().unwrap_or(Value::Null),
        "endpoint_ready": router.get("endpoint_ready").cloned().unwrap_or(Value::Null),
        "local_only": router.get("local_only").cloned().unwrap_or(Value::Null),
        "cloud_fallback_allowed": router
            .get("cloud_fallback_allowed")
            .cloned()
            .unwrap_or(Value::Null),
        "backend_kind": router.pointer("/endpoint/backend_kind").cloned().unwrap_or(Value::Null),
        "model_loaded": router.pointer("/endpoint/model_loaded").cloned().unwrap_or(Value::Null),
        "model_name": router.get("model_name").cloned().unwrap_or(Value::Null),
        "urls_redacted": true,
        "redacted": true,
    })
}

fn matches_semantic_router_tag(tag: &str) -> bool {
    matches!(
        tag.trim().to_ascii_lowercase().as_str(),
        "semantic_router" | "assistant_input_parser" | "k3_nsp"
    )
}

fn package_summary(package: &str) -> Value {
    let output = Command::new("dpkg-query")
        .args(["-W", "-f=${Version}", package])
        .output();
    match output {
        Ok(output) if output.status.success() => json!({
            "name": package,
            "status": "installed",
            "version": String::from_utf8_lossy(&output.stdout).trim().to_string(),
            "redacted": true,
        }),
        Ok(output) => json!({
            "name": package,
            "status": "unknown",
            "error": redact_evt_text(&String::from_utf8_lossy(&output.stderr)),
            "redacted": true,
        }),
        Err(error) => json!({
            "name": package,
            "status": "unavailable",
            "error": redact_evt_text(&error.to_string()),
            "redacted": true,
        }),
    }
}

fn systemd_service_summary(service: &str) -> Value {
    let output = Command::new("systemctl")
        .args(["is-active", service])
        .output();
    match output {
        Ok(output) => {
            let status = String::from_utf8_lossy(&output.stdout).trim().to_string();
            json!({
                "service": service,
                "status": if status.is_empty() { "unknown" } else { status.as_str() },
                "active": output.status.success(),
            })
        }
        Err(error) => json!({
            "service": service,
            "status": "unavailable",
            "active": false,
            "error": redact_evt_text(&error.to_string()),
        }),
    }
}

fn resource_summary() -> Value {
    json!({
        "memory": proc_meminfo_summary(),
        "disk": {
            "root": disk_summary("/")
        },
        "thermal": thermal_summary(),
        "dmesg_risk": dmesg_risk_summary(),
        "redacted": true,
    })
}

fn proc_meminfo_summary() -> Value {
    let text = match fs::read_to_string("/proc/meminfo") {
        Ok(text) => text,
        Err(error) => {
            return json!({
                "status": "unavailable",
                "error": redact_evt_text(&error.to_string()),
            })
        }
    };
    let mut total_kb = None;
    let mut available_kb = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            total_kb = first_number(rest);
        } else if let Some(rest) = line.strip_prefix("MemAvailable:") {
            available_kb = first_number(rest);
        }
    }
    let available_percent = total_kb
        .zip(available_kb)
        .and_then(|(total, available)| (total > 0).then_some((available * 100) / total));
    json!({
        "status": if total_kb.is_some() { "available" } else { "unknown" },
        "total_mb": total_kb.map(|value| value / 1024),
        "available_mb": available_kb.map(|value| value / 1024),
        "available_percent": available_percent,
    })
}

fn disk_summary(path: &str) -> Value {
    let output = Command::new("df").args(["-Pk", path]).output();
    let Ok(output) = output else {
        return json!({"status": "unavailable"});
    };
    if !output.status.success() {
        return json!({
            "status": "unknown",
            "error": redact_evt_text(&String::from_utf8_lossy(&output.stderr)),
        });
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().nth(1).unwrap_or_default();
    let parts = line.split_whitespace().collect::<Vec<_>>();
    if parts.len() < 5 {
        return json!({"status": "unknown"});
    }
    let use_percent = parts[4].trim_end_matches('%').parse::<u64>().ok();
    json!({
        "status": "available",
        "mount": "root",
        "use_percent": use_percent,
    })
}

fn thermal_summary() -> Value {
    let mut values = Vec::new();
    if let Ok(entries) = fs::read_dir("/sys/class/thermal") {
        for entry in entries.flatten() {
            let temp_path = entry.path().join("temp");
            let Ok(text) = fs::read_to_string(temp_path) else {
                continue;
            };
            let Ok(raw) = text.trim().parse::<f64>() else {
                continue;
            };
            let celsius = if raw > 1000.0 { raw / 1000.0 } else { raw };
            if celsius.is_finite() {
                values.push(celsius);
            }
        }
    }
    let max_celsius = values
        .iter()
        .copied()
        .fold(None, |max: Option<f64>, value| {
            Some(max.map(|current| current.max(value)).unwrap_or(value))
        });
    json!({
        "status": if max_celsius.is_some() {
            "available"
        } else {
            "unknown"
        },
        "max_celsius": max_celsius,
    })
}

fn dmesg_risk_summary() -> Value {
    let output = Command::new("dmesg").arg("-T").output();
    let Ok(output) = output else {
        return json!({"status": "unavailable", "count": 0});
    };
    if !output.status.success() {
        return json!({"status": "unknown", "count": 0});
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let count = text
        .lines()
        .rev()
        .take(200)
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            [
                "overheat",
                "thermal",
                "segfault",
                "oom",
                "i/o error",
                "undervoltage",
            ]
            .iter()
            .any(|keyword| lower.contains(keyword))
        })
        .count();
    json!({
        "status": if count == 0 { "clean" } else { "warn" },
        "count": count,
        "lines_redacted": true,
    })
}

fn secret_scan_summary() -> Value {
    let roots = evt_secret_scan_roots();
    let mut counts = serde_json::Map::new();
    for key in [
        "rtsp_url",
        "bearer_token",
        "api_key",
        "ha_token",
        "private_key",
        "data_image",
        "local_media_path",
    ] {
        counts.insert(key.to_string(), json!(0_u64));
    }
    for root in roots {
        scan_path_counts(&root, &mut counts, 0);
    }
    let total = counts
        .values()
        .filter_map(Value::as_u64)
        .fold(0_u64, |sum, value| sum.saturating_add(value));
    json!({
        "secret_scan": {
            "status": if total == 0 { "clean" } else { "blocked" },
            "total_count": total,
            "counts": counts,
            "matched_lines_included": false,
            "redacted": true,
        },
        "redacted": true,
    })
}

fn evt_secret_scan_roots() -> Vec<String> {
    if let Ok(value) = env::var("HARBORNAVI_EVT_SECRET_SCAN_ROOTS") {
        return value
            .split([';', ':'])
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
            .collect();
    }
    default_evt_secret_scan_roots()
        .iter()
        .filter(|path| Path::new(path).exists())
        .map(|path| path.to_string())
        .collect()
}

fn default_evt_secret_scan_roots() -> [&'static str; 7] {
    [
        "/var/log/nginx",
        "/var/log/harboros-beacon",
        "/var/log/harboros-im-gate",
        "/var/log/harborlink",
        "/var/log/semantic-router",
        "/run/harboros",
        "/var/lib/harboros-beacon/diagnostics",
    ]
}

fn scan_path_counts(path: &str, counts: &mut serde_json::Map<String, Value>, depth: usize) {
    if depth > 2 {
        return;
    }
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    if metadata.is_file() {
        if metadata.len() > 1_000_000 {
            return;
        }
        if let Ok(text) = fs::read_to_string(path) {
            update_secret_counts(&text, counts);
        }
        return;
    }
    if !metadata.is_dir() {
        return;
    }
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    for entry in entries.flatten().take(128) {
        let path = entry.path();
        if let Some(path) = path.to_str() {
            scan_path_counts(path, counts, depth + 1);
        }
    }
}

fn update_secret_counts(text: &str, counts: &mut serde_json::Map<String, Value>) {
    for line in text.lines().take(2000) {
        let lower = line.to_ascii_lowercase();
        increment_if(counts, "rtsp_url", lower.contains("rtsp://"));
        increment_if(counts, "bearer_token", lower.contains("bearer "));
        increment_if(
            counts,
            "api_key",
            lower.contains("api_key") || lower.contains("api-key") || lower.contains("apikey"),
        );
        increment_if(
            counts,
            "ha_token",
            lower.contains("home_assistant_token")
                || lower.contains("ha token")
                || lower.contains("ha_token"),
        );
        increment_if(counts, "private_key", lower.contains("private key"));
        increment_if(counts, "data_image", lower.contains("data:image"));
        increment_if(
            counts,
            "local_media_path",
            (lower.contains("/var/lib/harboros-beacon")
                || lower.contains("/mnt/")
                || lower.contains("/tmp/"))
                && (lower.contains("snapshot")
                    || lower.contains("camera")
                    || lower.contains("record")
                    || lower.contains(".mp4")
                    || lower.contains(".jpg")),
        );
    }
}

fn increment_if(counts: &mut serde_json::Map<String, Value>, key: &str, condition: bool) {
    if !condition {
        return;
    }
    let next = counts.get(key).and_then(Value::as_u64).unwrap_or(0) + 1;
    counts.insert(key.to_string(), json!(next));
}

fn gateway_connected(gateway_status: Option<&Value>) -> bool {
    gateway_status
        .and_then(|value| value.get("connected").and_then(Value::as_bool))
        .or_else(|| {
            gateway_status
                .and_then(|value| value.pointer("/bridge_provider/connected"))
                .and_then(Value::as_bool)
        })
        .or_else(|| {
            gateway_channel(gateway_status?, "weixin")
                .and_then(|value| value.get("connected"))
                .and_then(Value::as_bool)
        })
        .unwrap_or(false)
}

fn gateway_configured(gateway_status: Option<&Value>) -> bool {
    gateway_status
        .and_then(|value| value.get("configured").and_then(Value::as_bool))
        .or_else(|| {
            gateway_status
                .and_then(|value| value.pointer("/bridge_provider/configured"))
                .and_then(Value::as_bool)
        })
        .or_else(|| {
            gateway_channel(gateway_status?, "weixin")
                .and_then(|value| value.get("configured"))
                .and_then(Value::as_bool)
        })
        .unwrap_or(false)
}

fn gateway_channel<'a>(payload: &'a Value, platform: &str) -> Option<&'a Value> {
    payload
        .get("channels")
        .or_else(|| payload.pointer("/gateway_status/channels"))
        .and_then(Value::as_array)
        .and_then(|channels| {
            channels.iter().find(|channel| {
                channel
                    .get("platform")
                    .or_else(|| channel.get("name"))
                    .or_else(|| channel.get("channel"))
                    .and_then(Value::as_str)
                    .is_some_and(|value| value.eq_ignore_ascii_case(platform))
            })
        })
}

fn first_number(text: &str) -> Option<u64> {
    text.split_whitespace()
        .find_map(|part| part.parse::<u64>().ok())
}

fn non_empty_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn redact_evt_text(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if lower.contains("rtsp://")
        || lower.contains("bearer ")
        || lower.contains("api_key")
        || lower.contains("apikey")
        || lower.contains("private key")
        || lower.contains("data:image")
        || lower.contains("/var/lib/harboros-beacon")
        || lower.contains("/mnt/")
        || lower.contains("/tmp/")
    {
        "[redacted]".to_string()
    } else {
        value.trim().to_string()
    }
}

fn now_unix_string() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

#[allow(dead_code)]
fn command_exists(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};
    use std::thread;
    use std::time::Duration as StdDuration;

    use tiny_http::{Response, Server};

    use crate::control_plane::models::{
        ModelEndpoint, ModelEndpointKind, ModelEndpointStatus, ModelKind, PrivacyLevel,
    };
    use crate::runtime::admin_console::{AdminConsoleState, NotificationTargetRecord};

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = env::var(key).ok();
            env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.as_ref() {
                env::set_var(self.key, previous);
            } else {
                env::remove_var(self.key);
            }
        }
    }

    fn evt_test_env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn evt_long_run_requests_are_blocked() {
        assert!(evt_long_run_request_boundary("开始72小时压测"));
        assert!(evt_long_run_request_boundary("启动4h stress"));
        assert!(!evt_long_run_request_boundary("帮我做一下EVT预检"));
    }

    #[test]
    fn readiness_redacts_gateway_target_route_key() {
        let _env_lock = evt_test_env_lock().lock().expect("env lock");
        let mut state = AdminConsoleState::default();
        state.notification_targets.push(NotificationTargetRecord {
            target_id: "target-1".to_string(),
            label: "默认微信".to_string(),
            route_key: "secret-route-key".to_string(),
            platform_hint: "weixin".to_string(),
            is_default: true,
        });
        state.home_assistant.base_url = "http://homeassistant.local:8123".to_string();
        state.home_assistant.access_token = "secret-ha-token".to_string();
        let readiness = build_evt_readiness_from_state(
            &state,
            1,
            Some(&json!({"connected": true, "configured": true})),
        );
        let text = serde_json::to_string(&readiness).expect("readiness json");
        assert!(!text.contains("secret-route-key"));
        assert!(!text.contains("secret-ha-token"));
        assert!(!text.contains("homeassistant.local"));
        assert!(text.contains("\"route_key_redacted\":true"));
        assert!(text.contains("\"token_redacted\":true"));
    }

    #[test]
    fn readiness_records_semantic_router_endpoint_health_without_urls() {
        let _env_lock = evt_test_env_lock().lock().expect("env lock");
        let server = Server::http("127.0.0.1:0").expect("health server");
        let healthz_url = format!("http://{}/healthz", server.server_addr());
        let _healthz = EnvVarGuard::set(SEMANTIC_ROUTER_HEALTHZ_URL_ENV, &healthz_url);
        let server_thread = thread::spawn(move || {
            if let Ok(Some(request)) = server.recv_timeout(StdDuration::from_secs(3)) {
                let _ = request.respond(Response::from_string(
                    r#"{"service":"harbor-model-api","status":"ok","backend":{"kind":"candle","ready":true,"model_loaded":false},"chat_model":"Qwen/Qwen2.5-0.5B-Instruct","ready":true}"#,
                ));
            }
        });

        let mut state = AdminConsoleState::default();
        state.models.endpoints.push(ModelEndpoint {
            model_endpoint_id: "llm-local-semantic-router".to_string(),
            workspace_id: Some("home-1".to_string()),
            provider_account_id: None,
            model_kind: ModelKind::Llm,
            endpoint_kind: ModelEndpointKind::Local,
            provider_key: "openai_compatible".to_string(),
            model_name: "Qwen/Qwen2.5-0.5B-Instruct".to_string(),
            capability_tags: vec!["semantic_router".to_string(), "k3_nsp".to_string()],
            cost_policy: json!({}),
            status: ModelEndpointStatus::Active,
            metadata: json!({
                "healthz_url": healthz_url,
                "api_key": "secret-router-token",
                "semantic_router": true,
                "local_only": true,
            }),
        });

        let readiness = build_evt_readiness_from_state(&state, 1, None);
        server_thread.join().expect("health server joined");

        assert_eq!(
            readiness["models"]["semantic_router"]["endpoint_ready"],
            json!(true)
        );
        assert_eq!(
            readiness["models"]["semantic_router"]["endpoint"]["backend_kind"],
            json!("candle")
        );
        assert_eq!(
            readiness["models"]["semantic_router"]["endpoint"]["healthz_url_redacted"],
            json!(true)
        );
        let text = serde_json::to_string(&readiness).expect("readiness json");
        assert!(!text.contains("/healthz"));
        assert!(!text.contains("secret-router-token"));
        assert!(!text.contains("127.0.0.1"));
    }

    #[test]
    fn semantic_router_cloud_fallback_blocks_evt_readiness() {
        let _env_lock = evt_test_env_lock().lock().expect("env lock");
        let mut state = AdminConsoleState::default();
        let router = state
            .models
            .route_policies
            .iter_mut()
            .find(|policy| policy.route_policy_id == "semantic.router")
            .expect("router policy");
        router.privacy_level = PrivacyLevel::AllowRedactedCloud;
        router.fallback_order = vec!["local".to_string(), "cloud".to_string()];

        let readiness = build_evt_readiness_from_state(&state, 1, None);

        assert_eq!(
            readiness["models"]["semantic_router"]["local_only"],
            json!(false)
        );
        assert!(readiness["blockers"]
            .as_array()
            .expect("blockers")
            .iter()
            .any(|value| value == "semantic_router_not_local_only"));
    }

    #[test]
    fn default_secret_scan_roots_skip_auth_log_scope() {
        let roots = default_evt_secret_scan_roots();
        assert!(roots.contains(&"/var/log/nginx"));
        assert!(!roots.contains(&"/var/log"));
    }
}
