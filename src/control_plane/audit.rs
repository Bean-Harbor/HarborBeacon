//! Audit event metadata used by the control plane.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use uuid::Uuid;

pub const MAX_ADMIN_AUDIT_RECORDS: usize = 500;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditActor {
    pub user_id: String,
    pub source: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AuditActorKind {
    #[default]
    User,
    System,
    Model,
    Provider,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AuditRecord {
    pub audit_id: String,
    pub workspace_id: String,
    pub entity_kind: String,
    pub entity_id: String,
    pub action: String,
    pub actor_kind: AuditActorKind,
    pub actor_id: String,
    #[serde(default)]
    pub request_snapshot: Value,
    #[serde(default)]
    pub result_snapshot: Value,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AuditRecordQuery {
    pub limit: Option<usize>,
    pub cursor: Option<usize>,
    pub entity_kind: Option<String>,
    pub entity_id: Option<String>,
    pub action: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditRecordPage {
    pub records: Vec<AuditRecord>,
    pub total: usize,
    pub limit: usize,
    pub cursor: Option<String>,
    pub next_cursor: Option<String>,
    pub metadata_only: bool,
    pub secret_scan: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditSummary {
    pub total: usize,
    pub window: String,
    pub by_entity_kind: BTreeMap<String, usize>,
    pub by_action: BTreeMap<String, usize>,
    pub by_actor_kind: BTreeMap<String, usize>,
    pub metadata_only: bool,
    pub secret_scan: String,
}

pub fn build_metadata_audit_record(
    workspace_id: impl Into<String>,
    entity_kind: impl Into<String>,
    entity_id: impl Into<String>,
    action: impl Into<String>,
    actor_kind: AuditActorKind,
    actor_id: impl Into<String>,
    request_snapshot: Value,
    result_snapshot: Value,
) -> AuditRecord {
    AuditRecord {
        audit_id: format!("audit-{}", Uuid::new_v4().simple()),
        workspace_id: workspace_id.into(),
        entity_kind: entity_kind.into(),
        entity_id: entity_id.into(),
        action: action.into(),
        actor_kind,
        actor_id: actor_id.into(),
        request_snapshot: mark_metadata_only(redact_audit_value(request_snapshot)),
        result_snapshot: mark_metadata_only(redact_audit_value(result_snapshot)),
        created_at: Some(now_unix_string()),
    }
}

pub fn append_bounded_audit_record(records: &mut Vec<AuditRecord>, record: AuditRecord) {
    records.push(sanitize_audit_record(record));
    *records = sanitize_audit_stream(std::mem::take(records));
}

pub fn sanitize_audit_stream(records: Vec<AuditRecord>) -> Vec<AuditRecord> {
    let start = records.len().saturating_sub(MAX_ADMIN_AUDIT_RECORDS);
    records
        .into_iter()
        .skip(start)
        .map(sanitize_audit_record)
        .collect()
}

pub fn sanitize_audit_record(mut record: AuditRecord) -> AuditRecord {
    record.audit_id = trim_or_generated(record.audit_id, "audit");
    record.workspace_id = trim_or_default(record.workspace_id, "home-1");
    record.entity_kind = trim_or_default(record.entity_kind, "unknown");
    record.entity_id = trim_or_default(record.entity_id, "unknown");
    record.action = trim_or_default(record.action, "unknown");
    record.actor_id = trim_or_default(record.actor_id, "system");
    record.request_snapshot = mark_metadata_only(redact_audit_value(record.request_snapshot));
    record.result_snapshot = mark_metadata_only(redact_audit_value(record.result_snapshot));
    record.created_at = record
        .created_at
        .and_then(|value| non_empty(value.trim()))
        .or_else(|| Some(now_unix_string()));
    record
}

pub fn query_audit_records(records: &[AuditRecord], query: AuditRecordQuery) -> AuditRecordPage {
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let cursor = query.cursor.unwrap_or(0);
    let filtered = records
        .iter()
        .rev()
        .filter(|record| audit_record_matches(record, &query))
        .cloned()
        .collect::<Vec<_>>();
    let total = filtered.len();
    let page = filtered
        .into_iter()
        .skip(cursor)
        .take(limit)
        .map(sanitize_audit_record)
        .collect::<Vec<_>>();
    let next_offset = cursor + page.len();
    AuditRecordPage {
        records: page,
        total,
        limit,
        cursor: if cursor > 0 {
            Some(cursor.to_string())
        } else {
            None
        },
        next_cursor: if next_offset < total {
            Some(next_offset.to_string())
        } else {
            None
        },
        metadata_only: true,
        secret_scan: "clean".to_string(),
    }
}

pub fn build_audit_summary(records: &[AuditRecord], window: impl Into<String>) -> AuditSummary {
    let mut by_entity_kind = BTreeMap::new();
    let mut by_action = BTreeMap::new();
    let mut by_actor_kind = BTreeMap::new();
    for record in records {
        *by_entity_kind
            .entry(record.entity_kind.clone())
            .or_insert(0) += 1;
        *by_action.entry(record.action.clone()).or_insert(0) += 1;
        *by_actor_kind
            .entry(actor_kind_key(record.actor_kind).to_string())
            .or_insert(0) += 1;
    }
    AuditSummary {
        total: records.len(),
        window: window.into(),
        by_entity_kind,
        by_action,
        by_actor_kind,
        metadata_only: true,
        secret_scan: "clean".to_string(),
    }
}

pub fn redact_audit_value(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut redacted = Map::new();
            for (key, value) in map {
                let normalized = key.trim().to_ascii_lowercase();
                if normalized == "route_key" {
                    redacted.insert("opaque_delivery_route_present".to_string(), json!(true));
                    continue;
                }
                if sensitive_audit_key(&normalized) {
                    redacted.insert(key, json!("[redacted]"));
                    continue;
                }
                redacted.insert(key, redact_audit_value(value));
            }
            Value::Object(redacted)
        }
        Value::Array(values) => Value::Array(values.into_iter().map(redact_audit_value).collect()),
        Value::String(value) => Value::String(redact_audit_string(&value)),
        other => other,
    }
}

fn audit_record_matches(record: &AuditRecord, query: &AuditRecordQuery) -> bool {
    query
        .entity_kind
        .as_deref()
        .map(|value| record.entity_kind == value)
        .unwrap_or(true)
        && query
            .entity_id
            .as_deref()
            .map(|value| record.entity_id == value)
            .unwrap_or(true)
        && query
            .action
            .as_deref()
            .map(|value| record.action == value)
            .unwrap_or(true)
}

fn mark_metadata_only(value: Value) -> Value {
    let mut object = match value {
        Value::Object(map) => map,
        other => {
            let mut map = Map::new();
            map.insert("value".to_string(), other);
            map
        }
    };
    object.insert("metadata_only".to_string(), json!(true));
    object.insert("redacted".to_string(), json!(true));
    object.insert("secret_scan".to_string(), json!("clean"));
    Value::Object(object)
}

fn sensitive_audit_key(key: &str) -> bool {
    key.contains("secret")
        || key.contains("token")
        || key.contains("password")
        || key.contains("api_key")
        || key.contains("authorization")
        || key.contains("credential")
        || key == "raw_path"
        || key == "path"
        || key.ends_with("_path")
        || key == "url"
        || key.ends_with("_url")
        || key == "uri"
        || key.ends_with("_uri")
        || key.contains("session")
}

fn redact_audit_string(value: &str) -> String {
    let lower = value.to_ascii_lowercase();
    if lower.contains("rtsp://")
        || lower.contains("token=")
        || lower.contains("secret=")
        || lower.contains("api_key")
        || lower.contains("bearer ")
        || lower.contains("\\\\")
        || lower.contains("c:\\")
        || lower.contains("/mnt/")
        || lower.contains(".harborbeacon/")
    {
        "[redacted]".to_string()
    } else {
        value.to_string()
    }
}

fn trim_or_generated(value: String, prefix: &str) -> String {
    non_empty(value.trim()).unwrap_or_else(|| format!("{prefix}-{}", Uuid::new_v4().simple()))
}

fn trim_or_default(value: String, default: &str) -> String {
    non_empty(value.trim()).unwrap_or_else(|| default.to_string())
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn now_unix_string() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

fn actor_kind_key(kind: AuditActorKind) -> &'static str {
    match kind {
        AuditActorKind::User => "user",
        AuditActorKind::System => "system",
        AuditActorKind::Model => "model",
        AuditActorKind::Provider => "provider",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_audit_redacts_sensitive_material_and_route_key_value() {
        let record = build_metadata_audit_record(
            "home-1",
            "vision_event",
            "event-1",
            "vision_event.notify",
            AuditActorKind::User,
            "user-1",
            json!({
                "route_key": "gw_route_secret",
                "snapshot_path": "C:\\models\\tokenizer.json",
                "token": "secret",
                "safe": "ok"
            }),
            json!({"status": "queued"}),
        );
        let text = serde_json::to_string(&record).expect("audit json");
        assert!(!text.contains("gw_route_secret"));
        assert!(!text.contains("C:\\models"));
        assert!(!text.contains("tokenizer"));
        assert!(text.contains("metadata_only"));
        assert_eq!(record.request_snapshot["secret_scan"], json!("clean"));
    }

    #[test]
    fn audit_query_paginates_and_filters_recent_first() {
        let mut records = Vec::new();
        for index in 0..3 {
            records.push(AuditRecord {
                audit_id: format!("audit-{index}"),
                workspace_id: "home-1".to_string(),
                entity_kind: "model_route_policy".to_string(),
                entity_id: "bulk".to_string(),
                action: if index == 1 {
                    "other".to_string()
                } else {
                    "model.route_policy.save".to_string()
                },
                actor_kind: AuditActorKind::User,
                actor_id: "user-1".to_string(),
                request_snapshot: json!({}),
                result_snapshot: json!({}),
                created_at: Some(index.to_string()),
            });
        }
        let page = query_audit_records(
            &records,
            AuditRecordQuery {
                limit: Some(1),
                action: Some("model.route_policy.save".to_string()),
                ..AuditRecordQuery::default()
            },
        );
        assert_eq!(page.total, 2);
        assert_eq!(page.records[0].audit_id, "audit-2");
        assert_eq!(page.next_cursor.as_deref(), Some("1"));
    }

    #[test]
    fn audit_query_clamps_limit_filters_entity_and_uses_cursor_offset() {
        let records = (0..4)
            .map(|index| AuditRecord {
                audit_id: format!("audit-{index}"),
                workspace_id: "home-1".to_string(),
                entity_kind: if index % 2 == 0 {
                    "home_guardian".to_string()
                } else {
                    "vision_event".to_string()
                },
                entity_id: if index == 2 {
                    "rule-porch".to_string()
                } else {
                    "other".to_string()
                },
                action: if index % 2 == 0 {
                    "home_guardian.evaluate_latest".to_string()
                } else {
                    "vision_event.notify".to_string()
                },
                actor_kind: AuditActorKind::User,
                actor_id: "user-1".to_string(),
                request_snapshot: json!({"raw_path": "/mnt/pool/private/frame.jpg"}),
                result_snapshot: json!({}),
                created_at: Some(index.to_string()),
            })
            .collect::<Vec<_>>();

        let page = query_audit_records(
            &records,
            AuditRecordQuery {
                limit: Some(0),
                cursor: Some(0),
                entity_kind: Some("home_guardian".to_string()),
                entity_id: Some("rule-porch".to_string()),
                action: Some("home_guardian.evaluate_latest".to_string()),
            },
        );
        assert_eq!(page.limit, 1);
        assert_eq!(page.total, 1);
        assert_eq!(page.records[0].audit_id, "audit-2");
        let text = serde_json::to_string(&page).expect("serialize audit page");
        assert!(!text.contains("/mnt/pool/private"));

        let empty_page = query_audit_records(
            &records,
            AuditRecordQuery {
                limit: Some(999),
                cursor: Some(2),
                entity_kind: Some("home_guardian".to_string()),
                action: Some("home_guardian.evaluate_latest".to_string()),
                ..AuditRecordQuery::default()
            },
        );
        assert_eq!(empty_page.limit, 200);
        assert_eq!(empty_page.cursor.as_deref(), Some("2"));
        assert_eq!(empty_page.records.len(), 0);
        assert_eq!(empty_page.next_cursor, None);
    }

    #[test]
    fn audit_stream_is_bounded() {
        let mut records = (0..(MAX_ADMIN_AUDIT_RECORDS + 5))
            .map(|index| AuditRecord {
                audit_id: format!("audit-{index}"),
                workspace_id: "home-1".to_string(),
                entity_kind: "task".to_string(),
                entity_id: index.to_string(),
                action: "test".to_string(),
                actor_kind: AuditActorKind::System,
                actor_id: "system".to_string(),
                request_snapshot: json!({}),
                result_snapshot: json!({}),
                created_at: Some(index.to_string()),
            })
            .collect::<Vec<_>>();
        append_bounded_audit_record(
            &mut records,
            AuditRecord {
                audit_id: "audit-last".to_string(),
                workspace_id: "home-1".to_string(),
                entity_kind: "task".to_string(),
                entity_id: "last".to_string(),
                action: "test".to_string(),
                actor_kind: AuditActorKind::System,
                actor_id: "system".to_string(),
                request_snapshot: json!({}),
                result_snapshot: json!({}),
                created_at: Some("last".to_string()),
            },
        );
        assert_eq!(records.len(), MAX_ADMIN_AUDIT_RECORDS);
        assert_eq!(records.last().unwrap().audit_id, "audit-last");
    }
}
