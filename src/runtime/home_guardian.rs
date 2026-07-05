//! Metadata-only Home Guardian rule evaluation helpers.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::runtime::admin_console::AutomationRuleReview;
use crate::runtime::vision_event::StoredLocalVisionEvent;

#[derive(Debug, Serialize)]
pub struct HomeGuardianActivityResponse {
    pub generated_at: String,
    pub rule_count: usize,
    pub active_count: usize,
    #[serde(default)]
    pub rules: Vec<Value>,
    #[serde(default)]
    pub activity: Vec<Value>,
    pub counters: Value,
    pub metadata_only: bool,
    pub secret_scan: String,
}

#[derive(Debug, Serialize, Clone)]
pub struct HomeGuardianEvaluationResponse {
    pub evaluation_id: String,
    pub status: String,
    pub event_id: String,
    pub evaluated_at: String,
    #[serde(default)]
    pub results: Vec<Value>,
    pub counters: Value,
    pub metadata_only: bool,
    pub secret_scan: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HomeGuardianHaServiceRequest {
    pub entity_id: String,
    pub domain: String,
    pub service: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeGuardianActionExecutionKind {
    NotifyDefaultTarget,
    HomeAssistantService,
    Unsupported,
}

#[derive(Debug, Clone)]
pub struct HomeGuardianActionExecutionPlan {
    pub action: Value,
    pub action_key: String,
    pub idempotency_key: String,
    pub kind: HomeGuardianActionExecutionKind,
}

#[derive(Debug, Clone)]
pub enum HomeGuardianRuleEvaluationPlan {
    Completed {
        review: AutomationRuleReview,
        summary: Value,
        persist_summary: bool,
    },
    Execute {
        review: AutomationRuleReview,
        actions: Vec<HomeGuardianActionExecutionPlan>,
    },
}

pub fn build_home_guardian_activity_response(
    reviews: Vec<AutomationRuleReview>,
) -> HomeGuardianActivityResponse {
    build_home_guardian_activity_response_with_activity(reviews, Vec::new())
}

pub fn build_home_guardian_activity_response_with_activity(
    reviews: Vec<AutomationRuleReview>,
    mut external_activity: Vec<Value>,
) -> HomeGuardianActivityResponse {
    let rules = reviews
        .iter()
        .filter(|review| automation_review_is_home_guardian(review))
        .map(home_guardian_rule_summary)
        .collect::<Vec<_>>();
    let active_count = reviews
        .iter()
        .filter(|review| automation_review_is_home_guardian(review) && review.status == "active")
        .count();
    if external_activity.is_empty() {
        external_activity = reviews
            .iter()
            .filter(|review| automation_review_is_home_guardian(review))
            .flat_map(|review| review.run_summaries.iter().cloned())
            .collect::<Vec<_>>();
    }
    let mut activity = external_activity;
    activity.sort_by(|left, right| {
        json_string_at_paths(right, &["/created_at", "/evaluated_at"]).cmp(&json_string_at_paths(
            left,
            &["/created_at", "/evaluated_at"],
        ))
    });
    activity.truncate(25);
    let counters = home_guardian_result_counters(&activity);
    HomeGuardianActivityResponse {
        generated_at: now_unix_string(),
        rule_count: rules.len(),
        active_count,
        rules,
        activity,
        counters,
        metadata_only: true,
        secret_scan: "clean".to_string(),
    }
}

pub fn automation_review_is_home_guardian(review: &AutomationRuleReview) -> bool {
    let source = review.source.trim().to_ascii_lowercase();
    source == "home_guardian"
        || source == "harbornavi_home_guardian"
        || review
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.pointer("/home_guardian"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || review
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.pointer("/feature"))
            .and_then(Value::as_str)
            .is_some_and(|feature| feature == "home_guardian")
}

pub fn compact_home_guardian_proposal_evidence(value: &Value) -> Value {
    json!({
        "status": value.get("status").and_then(Value::as_str).unwrap_or("proposed"),
        "review_id": value.get("review_id").and_then(Value::as_str),
        "rule_id": value.get("rule_id").and_then(Value::as_str),
        "trigger": compact_home_guardian_trigger(value.get("trigger")),
        "action_plan": value
            .get("action_plan")
            .cloned()
            .unwrap_or_else(|| json!({"actions": [], "metadata_only": true})),
        "requires_enable": value
            .get("requires_enable")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        "created_at": value.get("created_at").and_then(Value::as_str),
        "metadata_only": true,
        "secret_scan": "clean",
        "redacted": true,
    })
}

pub fn compact_home_guardian_evaluation_evidence(value: &Value) -> Value {
    let results = value
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    json!({
        "evaluation_id": value.get("evaluation_id").and_then(Value::as_str),
        "status": value.get("status").and_then(Value::as_str).unwrap_or("evaluated"),
        "event_id": value.get("event_id").and_then(Value::as_str),
        "evaluated_at": value.get("evaluated_at").and_then(Value::as_str),
        "result_count": results.len(),
        "results": results
            .iter()
            .take(5)
            .map(compact_home_guardian_result)
            .collect::<Vec<_>>(),
        "counters": value.get("counters").cloned().unwrap_or_else(|| json!({})),
        "metadata_only": true,
        "secret_scan": "clean",
        "redacted": true,
    })
}

fn compact_home_guardian_result(result: &Value) -> Value {
    let actions = result
        .get("actions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    json!({
        "run_id": result.get("run_id").and_then(Value::as_str),
        "review_id": result.get("review_id").and_then(Value::as_str),
        "rule_id": result.get("rule_id").and_then(Value::as_str),
        "event_id": result.get("event_id").and_then(Value::as_str),
        "camera_id": result.get("camera_id").and_then(Value::as_str),
        "event_type": result.get("event_type").and_then(Value::as_str),
        "status": result.get("status").and_then(Value::as_str).unwrap_or("unknown"),
        "matched": result.get("matched").and_then(Value::as_bool).unwrap_or(false),
        "executed": result.get("executed").and_then(Value::as_bool).unwrap_or(false),
        "reason": result
            .get("reason")
            .and_then(Value::as_str)
            .map(compact_redacted_reason),
        "action_count": actions.len(),
        "actions": actions
            .iter()
            .take(5)
            .map(compact_home_guardian_action_result)
            .collect::<Vec<_>>(),
        "created_at": result.get("created_at").and_then(Value::as_str),
        "metadata_only": true,
        "secret_scan": "clean",
        "redacted": true,
    })
}

fn compact_home_guardian_action_result(action: &Value) -> Value {
    json!({
        "action": action.get("action").and_then(Value::as_str).unwrap_or("unknown"),
        "status": action.get("status").and_then(Value::as_str).unwrap_or("unknown"),
        "executed": action.get("executed").and_then(Value::as_bool).unwrap_or(false),
        "idempotency_key": action.get("idempotency_key").and_then(Value::as_str),
        "domain": action.get("domain").and_then(Value::as_str),
        "service": action.get("service").and_then(Value::as_str),
        "entity_id": action.get("entity_id").and_then(Value::as_str),
        "notification_status": action.get("notification_status").and_then(Value::as_str),
        "reason": action
            .get("reason")
            .and_then(Value::as_str)
            .map(compact_redacted_reason),
        "redacted": true,
    })
}

fn compact_home_guardian_trigger(trigger: Option<&Value>) -> Value {
    let Some(trigger) = trigger else {
        return json!({"metadata_only": true});
    };
    json!({
        "camera_id": json_string_at_paths(trigger, &["/camera_id", "/camera"]),
        "event_type": json_string_at_paths(trigger, &["/event_type"]),
        "labels": trigger
            .pointer("/labels")
            .and_then(Value::as_array)
            .map(|labels| {
                labels
                    .iter()
                    .filter_map(Value::as_str)
                    .take(8)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        "min_confidence": trigger.pointer("/min_confidence").and_then(Value::as_f64),
        "metadata_only": true,
    })
}

fn compact_redacted_reason(reason: &str) -> String {
    redact_home_guardian_string(reason)
        .chars()
        .take(240)
        .collect()
}

fn home_guardian_rule_summary(review: &AutomationRuleReview) -> Value {
    json!({
        "review_id": review.review_id,
        "rule_id": review.rule_id,
        "status": review.status,
        "source": review.source,
        "risk_level": review.risk_level,
        "requires_approval": review.requires_approval,
        "trigger": review.trigger_definition,
        "action_plan": home_guardian_redacted_action_plan(review.action_plan.as_ref()),
        "run_count": review.run_summaries.len(),
        "last_run": review.run_summaries.last(),
        "metadata_only": true,
        "secret_scan": "clean",
    })
}

pub fn home_guardian_redacted_action_plan(action_plan: Option<&Value>) -> Value {
    let actions = home_guardian_action_plan_actions(action_plan);
    json!({
        "actions": actions
            .into_iter()
            .map(|action| {
                if home_guardian_action_is_notify(&action) {
                    json!({
                        "kind": "notify_default_target",
                        "target": "default_notification_target",
                    })
                } else if home_guardian_action_is_ha_service_action(&action) {
                    let request = home_guardian_ha_service_request(&action);
                    json!({
                        "kind": "ha_service_action",
                        "domain": request.domain,
                        "service": request.service,
                        "entity_id": request.entity_id,
                        "fields_empty": true,
                    })
                } else {
                    json!({
                        "kind": home_guardian_action_key(&action),
                        "status": "unsupported",
                    })
                }
            })
            .collect::<Vec<_>>(),
        "metadata_only": true,
    })
}

pub fn home_guardian_trigger_matches_event(
    trigger: &Value,
    stored: &StoredLocalVisionEvent,
) -> (bool, String) {
    let event = &stored.event;
    if let Some(camera_id) = json_string_at_paths(trigger, &["/camera_id", "/camera"]) {
        if camera_id.trim().to_ascii_lowercase() != event.camera_id.to_ascii_lowercase() {
            return (false, "camera_id did not match".to_string());
        }
    }
    if let Some(event_type) = json_string_at_paths(trigger, &["/event_type"]) {
        if event_type.trim().to_ascii_lowercase() != event.event_type.to_ascii_lowercase() {
            return (false, "event_type did not match".to_string());
        }
    }
    if let Some(min_confidence) = trigger
        .pointer("/min_confidence")
        .and_then(Value::as_f64)
        .map(|value| value as f32)
    {
        if event.confidence < min_confidence {
            return (false, "confidence below rule threshold".to_string());
        }
    }
    if let Some(labels) = trigger.pointer("/labels").and_then(Value::as_array) {
        let required = labels
            .iter()
            .filter_map(Value::as_str)
            .map(|label| label.trim().to_ascii_lowercase())
            .filter(|label| !label.is_empty())
            .collect::<Vec<_>>();
        if !required.is_empty()
            && !required.iter().all(|label| {
                event
                    .labels
                    .iter()
                    .any(|existing| existing.to_ascii_lowercase() == *label)
                    || event.event_type.to_ascii_lowercase() == *label
            })
        {
            return (false, "labels did not match".to_string());
        }
    }
    (true, "matched".to_string())
}

pub fn home_guardian_action_plan_actions(action_plan: Option<&Value>) -> Vec<Value> {
    let Some(action_plan) = action_plan else {
        return Vec::new();
    };
    if let Some(actions) = action_plan.pointer("/actions").and_then(Value::as_array) {
        return actions.iter().cloned().collect();
    }
    if action_plan.as_object().is_some() {
        return vec![action_plan.clone()];
    }
    Vec::new()
}

fn home_guardian_action_kind(action: &Value) -> String {
    json_string_at_paths(action, &["/kind", "/action", "/type"])
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
}

pub fn home_guardian_action_key(action: &Value) -> String {
    let kind = home_guardian_action_kind(action);
    if kind.contains("notify") {
        "notify_default_target".to_string()
    } else if kind.contains("ha") || kind.contains("home_assistant") {
        let request = home_guardian_ha_service_request(action);
        format!(
            "ha:{}:{}:{}",
            request.domain, request.service, request.entity_id
        )
    } else if kind.is_empty() {
        "unsupported".to_string()
    } else {
        kind
    }
}

pub fn home_guardian_action_is_notify(action: &Value) -> bool {
    let kind = home_guardian_action_kind(action);
    kind == "notify"
        || kind == "notify_default_target"
        || kind == "event_notify"
        || kind == "notification"
}

pub fn home_guardian_action_is_ha_service_action(action: &Value) -> bool {
    let kind = home_guardian_action_kind(action);
    kind == "ha_service_action"
        || kind == "home_assistant_service_action"
        || kind == "home_assistant"
}

pub fn home_guardian_ha_service_request(action: &Value) -> HomeGuardianHaServiceRequest {
    let nested = action
        .pointer("/home_assistant")
        .or_else(|| action.pointer("/ha"))
        .unwrap_or(action);
    HomeGuardianHaServiceRequest {
        entity_id: json_string_at_paths(nested, &["/entity_id"]).unwrap_or_default(),
        domain: json_string_at_paths(nested, &["/domain"]).unwrap_or_default(),
        service: json_string_at_paths(nested, &["/service"]).unwrap_or_default(),
    }
}

pub fn select_home_guardian_reviews(
    reviews: Vec<AutomationRuleReview>,
    only_review_id: Option<&str>,
) -> Result<Vec<AutomationRuleReview>, String> {
    let mut selected = reviews
        .into_iter()
        .filter(|review| {
            only_review_id
                .map(|review_id| review.review_id == review_id)
                .unwrap_or(true)
        })
        .collect::<Vec<_>>();
    if only_review_id.is_some() && selected.is_empty() {
        return Err("home guardian rule review not found".to_string());
    }
    selected.sort_by(|left, right| left.review_id.cmp(&right.review_id));
    Ok(selected)
}

pub fn build_home_guardian_rule_evaluation_plan(
    review: AutomationRuleReview,
    stored: &StoredLocalVisionEvent,
    only_review_requested: bool,
    execute_active: bool,
) -> Option<HomeGuardianRuleEvaluationPlan> {
    if !automation_review_is_home_guardian(&review) {
        return only_review_requested.then(|| HomeGuardianRuleEvaluationPlan::Completed {
            summary: home_guardian_rule_run_summary(
                &review,
                stored,
                "skipped",
                false,
                false,
                "Automation review is not a Home Guardian rule.",
                Vec::new(),
            ),
            review,
            persist_summary: false,
        });
    }

    let Some(trigger) = review.trigger_definition.as_ref() else {
        return Some(HomeGuardianRuleEvaluationPlan::Completed {
            summary: home_guardian_rule_run_summary(
                &review,
                stored,
                "blocked",
                false,
                false,
                "Home Guardian rule is missing a trigger definition.",
                Vec::new(),
            ),
            review,
            persist_summary: false,
        });
    };

    let (matched, match_reason) = home_guardian_trigger_matches_event(trigger, stored);
    if !matched {
        return Some(HomeGuardianRuleEvaluationPlan::Completed {
            summary: home_guardian_rule_run_summary(
                &review,
                stored,
                "skipped",
                false,
                false,
                &match_reason,
                Vec::new(),
            ),
            review,
            persist_summary: only_review_requested,
        });
    }

    if review.status != "active" || !execute_active {
        return Some(HomeGuardianRuleEvaluationPlan::Completed {
            summary: home_guardian_rule_run_summary(
                &review,
                stored,
                "skipped",
                true,
                false,
                "Home Guardian rule is not active; explicit enable is required before execution.",
                Vec::new(),
            ),
            review,
            persist_summary: true,
        });
    }

    let actions = home_guardian_action_plan_actions(review.action_plan.as_ref());
    if actions.is_empty() {
        return Some(HomeGuardianRuleEvaluationPlan::Completed {
            summary: home_guardian_rule_run_summary(
                &review,
                stored,
                "blocked",
                true,
                false,
                "Home Guardian rule has no supported action plan.",
                Vec::new(),
            ),
            review,
            persist_summary: only_review_requested,
        });
    }

    let action_plans = actions
        .into_iter()
        .map(|action| home_guardian_action_execution_plan(&review, stored, action))
        .collect::<Vec<_>>();
    Some(HomeGuardianRuleEvaluationPlan::Execute {
        review,
        actions: action_plans,
    })
}

pub fn home_guardian_action_execution_plan(
    review: &AutomationRuleReview,
    stored: &StoredLocalVisionEvent,
    action: Value,
) -> HomeGuardianActionExecutionPlan {
    let action_key = home_guardian_action_key(&action);
    let idempotency_key = home_guardian_idempotency_key(review, stored, &action_key);
    let kind = if home_guardian_action_is_notify(&action) {
        HomeGuardianActionExecutionKind::NotifyDefaultTarget
    } else if home_guardian_action_is_ha_service_action(&action) {
        HomeGuardianActionExecutionKind::HomeAssistantService
    } else {
        HomeGuardianActionExecutionKind::Unsupported
    };
    HomeGuardianActionExecutionPlan {
        action,
        action_key,
        idempotency_key,
        kind,
    }
}

pub fn home_guardian_idempotent_action_result(action_key: &str, idempotency_key: &str) -> Value {
    json!({
        "action": action_key,
        "status": "skipped",
        "executed": false,
        "idempotency_key": idempotency_key,
        "reason": "idempotent duplicate event/action run",
        "redacted": true,
    })
}

pub fn home_guardian_unsupported_action_result(action_key: &str, idempotency_key: &str) -> Value {
    json!({
        "action": action_key,
        "status": "blocked",
        "executed": false,
        "idempotency_key": idempotency_key,
        "reason": "Unsupported Home Guardian action.",
        "redacted": true,
    })
}

pub fn complete_home_guardian_rule_execution(
    review: &AutomationRuleReview,
    stored: &StoredLocalVisionEvent,
    action_results: Vec<Value>,
) -> Value {
    let executed = action_results.iter().any(|result| {
        result
            .get("executed")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    });
    let status = home_guardian_run_status_from_actions(&action_results);
    home_guardian_rule_run_summary(
        review,
        stored,
        status,
        true,
        executed,
        "Home Guardian rule evaluated against a local vision event.",
        action_results,
    )
}

pub fn append_home_guardian_run_summary(
    mut review: AutomationRuleReview,
    summary: Value,
) -> AutomationRuleReview {
    review.run_summaries.push(summary);
    review.run_summaries = latest_home_guardian_run_summaries(review.run_summaries);
    review
}

pub fn build_home_guardian_evaluation_response(
    stored: &StoredLocalVisionEvent,
    results: Vec<Value>,
) -> HomeGuardianEvaluationResponse {
    let counters = home_guardian_result_counters(&results);
    let status = if results
        .iter()
        .any(|result| result["status"] == json!("acted"))
    {
        "acted"
    } else if results.iter().any(|result| {
        matches!(
            result.get("status").and_then(Value::as_str),
            Some("blocked" | "failed")
        )
    }) {
        "degraded"
    } else {
        "evaluated"
    };
    HomeGuardianEvaluationResponse {
        evaluation_id: format!("guardian_eval_{}", Uuid::new_v4().simple()),
        status: status.to_string(),
        event_id: stored.event.event_id.clone(),
        evaluated_at: now_unix_string(),
        results,
        counters,
        metadata_only: true,
        secret_scan: "clean".to_string(),
    }
}

pub fn home_guardian_idempotency_key(
    review: &AutomationRuleReview,
    stored: &StoredLocalVisionEvent,
    action_key: &str,
) -> String {
    format!(
        "{}:{}:{}",
        review
            .rule_id
            .as_deref()
            .filter(|rule_id| !rule_id.trim().is_empty())
            .unwrap_or(&review.review_id),
        stored.event.event_id,
        action_key
    )
}

pub fn home_guardian_run_already_recorded(
    review: &AutomationRuleReview,
    idempotency_key: &str,
) -> bool {
    review.run_summaries.iter().any(|summary| {
        let Some(actions) = summary.pointer("/actions").and_then(Value::as_array) else {
            return false;
        };
        actions.iter().any(|action| {
            action
                .get("idempotency_key")
                .and_then(Value::as_str)
                .is_some_and(|value| value == idempotency_key)
                && matches!(
                    action.get("status").and_then(Value::as_str),
                    Some("delivered" | "acted" | "skipped")
                )
        })
    })
}

pub fn home_guardian_rule_run_summary(
    review: &AutomationRuleReview,
    stored: &StoredLocalVisionEvent,
    status: &str,
    matched: bool,
    executed: bool,
    reason: &str,
    actions: Vec<Value>,
) -> Value {
    let compact_actions = actions
        .iter()
        .map(compact_home_guardian_action_result)
        .collect::<Vec<_>>();
    json!({
        "run_id": format!("guardian_run_{}", Uuid::new_v4().simple()),
        "review_id": review.review_id,
        "rule_id": review.rule_id,
        "event_id": stored.event.event_id,
        "camera_id": stored.event.camera_id,
        "event_type": stored.event.event_type,
        "status": status,
        "matched": matched,
        "executed": executed,
        "reason": redact_home_guardian_string(reason),
        "actions": compact_actions,
        "created_at": now_unix_string(),
        "metadata_only": true,
        "secret_scan": "clean",
    })
}

pub fn latest_home_guardian_run_summaries(mut summaries: Vec<Value>) -> Vec<Value> {
    summaries.sort_by(|left, right| {
        json_string_at_paths(right, &["/created_at", "/evaluated_at"]).cmp(&json_string_at_paths(
            left,
            &["/created_at", "/evaluated_at"],
        ))
    });
    summaries.truncate(50);
    summaries.reverse();
    summaries
}

pub fn home_guardian_run_status_from_actions(actions: &[Value]) -> &'static str {
    if actions.iter().any(|action| {
        matches!(
            action.get("status").and_then(Value::as_str),
            Some("delivered" | "acted")
        )
    }) {
        "acted"
    } else if actions
        .iter()
        .any(|action| matches!(action.get("status").and_then(Value::as_str), Some("failed")))
    {
        "failed"
    } else if actions.iter().any(|action| {
        matches!(
            action.get("status").and_then(Value::as_str),
            Some("blocked")
        )
    }) {
        "blocked"
    } else {
        "skipped"
    }
}

pub fn home_guardian_result_counters(results: &[Value]) -> Value {
    let mut counters: BTreeMap<String, usize> = BTreeMap::new();
    for result in results {
        let status = result
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        *counters.entry(status).or_default() += 1;
        if let Some(actions) = result.get("actions").and_then(Value::as_array) {
            for action in actions {
                let action_status = action
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                *counters
                    .entry(format!("action_{action_status}"))
                    .or_default() += 1;
            }
        }
    }
    json!(counters)
}

fn json_string_at_paths(value: &Value, paths: &[&str]) -> Option<String> {
    paths.iter().find_map(|path| {
        value
            .pointer(path)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    })
}

fn now_unix_string() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

fn redact_home_guardian_string(value: &str) -> String {
    let mut output = Vec::new();
    for token in value.split_whitespace() {
        let lower = token.to_ascii_lowercase();
        if lower.contains("token=")
            || lower.contains("password=")
            || lower.contains("secret=")
            || lower.contains("authorization:")
            || lower.contains("bearer ")
            || lower.starts_with("rtsp://")
            || lower.starts_with("http://")
            || lower.starts_with("https://")
            || looks_like_local_path(token)
        {
            output.push("[redacted]".to_string());
        } else {
            output.push(token.to_string());
        }
    }
    output.join(" ")
}

fn looks_like_local_path(value: &str) -> bool {
    let trimmed = value.trim_matches(|ch: char| matches!(ch, '"' | '\'' | ',' | ';' | ')' | '('));
    if trimmed.len() > 2 && trimmed.as_bytes().get(1) == Some(&b':') {
        return trimmed
            .as_bytes()
            .get(2)
            .is_some_and(|ch| matches!(*ch, b'\\' | b'/'));
    }
    trimmed.starts_with("/home/")
        || trimmed.starts_with("/Users/")
        || trimmed.starts_with("/mnt/")
        || trimmed.starts_with("/var/")
        || trimmed.starts_with("/tmp/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::vision_event::{
        LocalVisionEvent, SnapshotArtifact, StoredLocalVisionEvent,
    };

    fn sample_review(status: &str) -> AutomationRuleReview {
        AutomationRuleReview {
            review_id: "guardian_review_1".to_string(),
            workspace_id: "home".to_string(),
            source: "home_guardian".to_string(),
            source_channel: Some("task_api".to_string()),
            source_conversation_id: Some("conv-1".to_string()),
            original_prompt: "notify me when the front door sees a person".to_string(),
            status: status.to_string(),
            trigger_definition: Some(json!({
                "camera_id": "front-door",
                "event_type": "person_detected",
                "labels": ["person"],
                "min_confidence": 0.75,
            })),
            condition_definition: None,
            action_plan: Some(json!({
                "actions": [
                    {"kind": "notify_default_target"},
                    {
                        "kind": "ha_service_action",
                        "home_assistant": {
                            "domain": "light",
                            "service": "turn_on",
                            "entity_id": "light.entry"
                        }
                    }
                ],
                "metadata_only": true
            })),
            device_refs: Vec::new(),
            risk_level: Some("low".to_string()),
            requires_approval: true,
            created_at: Some("1700000000".to_string()),
            updated_at: None,
            expires_at: None,
            rule_id: Some("guardian_rule_1".to_string()),
            run_summaries: Vec::new(),
            metadata: Some(json!({"feature": "home_guardian", "home_guardian": true})),
        }
    }

    fn sample_event() -> StoredLocalVisionEvent {
        StoredLocalVisionEvent {
            event: LocalVisionEvent {
                event_id: "event-1".to_string(),
                camera_id: "front-door".to_string(),
                event_type: "person_detected".to_string(),
                confidence: 0.91,
                labels: vec!["person".to_string(), "doorway".to_string()],
                summary: "Person at the front door".to_string(),
                snapshot_artifact: SnapshotArtifact::default(),
                started_at: "1700000000".to_string(),
                analyzer: "k3-local".to_string(),
                latency_ms: 120,
                metrics: json!({}),
                vlm: None,
            },
            received_at: "1700000001".to_string(),
            audit_record: json!({"metadata_only": true}),
            ha_mqtt_payload: json!({"metadata_only": true}),
        }
    }

    #[test]
    fn home_guardian_activity_is_metadata_only_and_redacted() {
        let review = sample_review("pending");
        let response = build_home_guardian_activity_response(vec![review]);
        assert_eq!(response.rule_count, 1);
        assert_eq!(response.active_count, 0);
        assert!(response.metadata_only);
        assert_eq!(response.secret_scan, "clean");

        let serialized = serde_json::to_string(&response).expect("serialize response");
        assert!(serialized.contains("\"metadata_only\":true"));
        assert!(!serialized.contains("password"));
        assert!(!serialized.contains("token"));
        assert!(!serialized.contains("\"fields\""));
    }

    #[test]
    fn home_guardian_trigger_match_and_idempotency_are_stable() {
        let mut review = sample_review("active");
        let stored = sample_event();
        let trigger = review.trigger_definition.as_ref().expect("trigger");
        let (matched, reason) = home_guardian_trigger_matches_event(trigger, &stored);
        assert!(matched);
        assert_eq!(reason, "matched");

        let actions = home_guardian_action_plan_actions(review.action_plan.as_ref());
        assert_eq!(actions.len(), 2);
        assert!(home_guardian_action_is_notify(&actions[0]));
        assert!(home_guardian_action_is_ha_service_action(&actions[1]));

        let action_key = home_guardian_action_key(&actions[1]);
        assert_eq!(action_key, "ha:light:turn_on:light.entry");
        let idempotency_key = home_guardian_idempotency_key(&review, &stored, &action_key);
        assert_eq!(
            idempotency_key,
            "guardian_rule_1:event-1:ha:light:turn_on:light.entry"
        );
        assert!(!home_guardian_run_already_recorded(
            &review,
            &idempotency_key
        ));

        review.run_summaries.push(home_guardian_rule_run_summary(
            &review,
            &stored,
            "acted",
            true,
            true,
            "Executed via rtsp://user:pass@example.local/live token=secret",
            vec![json!({
                "action": action_key,
                "status": "acted",
                "executed": true,
                "idempotency_key": idempotency_key,
            })],
        ));
        assert!(home_guardian_run_already_recorded(
            &review,
            "guardian_rule_1:event-1:ha:light:turn_on:light.entry"
        ));
        let serialized = serde_json::to_string(&review.run_summaries).expect("serialize runs");
        assert!(!serialized.contains("user:pass"));
        assert!(!serialized.contains("token=secret"));
        assert!(serialized.contains("[redacted]"));
    }

    #[test]
    fn home_guardian_status_helpers_keep_pending_and_explicit_enable_paths() {
        let review = sample_review("pending");
        assert!(automation_review_is_home_guardian(&review));
        let stored = sample_event();
        let summary = home_guardian_rule_run_summary(
            &review,
            &stored,
            "skipped",
            true,
            false,
            "Home Guardian rule is not active; explicit enable is required before execution.",
            Vec::new(),
        );
        assert_eq!(summary["status"], json!("skipped"));
        assert_eq!(summary["matched"], json!(true));
        assert_eq!(summary["executed"], json!(false));

        let counters = home_guardian_result_counters(&[summary]);
        assert_eq!(counters["skipped"], json!(1));
        assert_eq!(home_guardian_run_status_from_actions(&[]), "skipped");
        assert_eq!(
            home_guardian_run_status_from_actions(&[json!({"status": "blocked"})]),
            "blocked"
        );
        assert_eq!(
            home_guardian_run_status_from_actions(&[json!({"status": "acted"})]),
            "acted"
        );
    }

    #[test]
    fn home_guardian_evaluation_plan_requires_active_status_before_execution() {
        let stored = sample_event();

        for status in ["pending", "draft", "paused", "discarded"] {
            let review = sample_review(status);
            let plan =
                build_home_guardian_rule_evaluation_plan(review, &stored, false, true).unwrap();
            match plan {
                HomeGuardianRuleEvaluationPlan::Completed {
                    summary,
                    persist_summary,
                    ..
                } => {
                    assert!(persist_summary, "{status} should persist skipped evidence");
                    assert_eq!(summary["status"], json!("skipped"));
                    assert_eq!(summary["matched"], json!(true));
                    assert_eq!(summary["executed"], json!(false));
                    assert_eq!(
                        summary["reason"],
                        json!(
                            "Home Guardian rule is not active; explicit enable is required before execution."
                        )
                    );
                }
                HomeGuardianRuleEvaluationPlan::Execute { .. } => {
                    panic!("{status} review must not execute")
                }
            }
        }

        let active = sample_review("active");
        let plan = build_home_guardian_rule_evaluation_plan(active, &stored, false, true).unwrap();
        match plan {
            HomeGuardianRuleEvaluationPlan::Execute { actions, .. } => {
                assert_eq!(actions.len(), 2);
                assert_eq!(
                    actions[0].kind,
                    HomeGuardianActionExecutionKind::NotifyDefaultTarget
                );
                assert_eq!(
                    actions[1].kind,
                    HomeGuardianActionExecutionKind::HomeAssistantService
                );
            }
            HomeGuardianRuleEvaluationPlan::Completed { .. } => {
                panic!("active matching review should produce an execution plan")
            }
        }
    }

    #[test]
    fn home_guardian_completed_plan_preserves_idempotent_skip_evidence() {
        let review = sample_review("active");
        let stored = sample_event();
        let action_plan = home_guardian_action_execution_plan(
            &review,
            &stored,
            json!({"kind": "notify_default_target"}),
        );
        let duplicate = home_guardian_idempotent_action_result(
            &action_plan.action_key,
            &action_plan.idempotency_key,
        );
        let summary = complete_home_guardian_rule_execution(&review, &stored, vec![duplicate]);

        assert_eq!(summary["status"], json!("skipped"));
        assert_eq!(summary["matched"], json!(true));
        assert_eq!(summary["executed"], json!(false));
        assert_eq!(summary["actions"][0]["status"], json!("skipped"));
        assert_eq!(summary["actions"][0]["executed"], json!(false));

        let review = append_home_guardian_run_summary(review, summary);
        assert!(home_guardian_run_already_recorded(
            &review,
            &action_plan.idempotency_key
        ));
    }
}
