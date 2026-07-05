//! General-message helpers for local vision event actions.

use std::fs;
use std::time::Instant;

use base64::Engine as _;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::connectors::notifications::NotificationDeliveryService;
use crate::control_plane::events::EventSeverity;
use crate::orchestrator::contracts::RiskLevel;
use crate::runtime::admin_console::NotificationTargetRecord;
use crate::runtime::model_center::run_vlm_summary_with_state;
use crate::runtime::vision_event::{
    attach_vlm_summary_to_event, build_local_vision_notification_intent,
    ingest_local_vision_event_default, list_recent_local_vision_events_default,
    LocalVisionEventArtifact, SnapshotArtifact, StoredLocalVisionEvent,
};

use super::{
    build_task_event_record, classify_task_notification_delivery_error,
    default_notification_target_record, non_empty_task_string,
    notification_delivery_record_summary, redact_task_api_text, step_id_for_request,
    GeneralMessagePlan, GeneralMessagePlanKind, TaskApiService, TaskRequest, TaskResponse,
    BASE64_STANDARD,
};

impl TaskApiService {
    pub(super) fn handle_general_message_latest_vision_event(
        &self,
        request: &TaskRequest,
    ) -> TaskResponse {
        let stored = match latest_local_vision_event() {
            Ok(Some(stored)) => stored,
            Ok(None) => {
                let message = "最近还没有可查看的摄像头事件。".to_string();
                let event = self.serialize_event_record(&build_task_event_record(
                    request,
                    &step_id_for_request(request),
                    "local_vision_event.summary_blocked",
                    EventSeverity::Warning,
                    json!({
                        "status": "blocked",
                        "reason": "no_recent_event",
                        "metadata_only": true,
                    }),
                ));
                return self.completed_with_context(
                    request,
                    "vision_event_store",
                    RiskLevel::Low,
                    message.clone(),
                    json!({
                        "reply_pack": {
                            "kind": "vision_event_summary",
                            "status": "blocked",
                            "summary": message,
                        }
                    }),
                    Vec::new(),
                    vec![event],
                    vec!["拍一张门口".to_string(), "录一段门口".to_string()],
                );
            }
            Err(error) => {
                let message = format!("读取最近摄像头事件失败：{}", redact_task_api_text(&error));
                return self.failed(request, "vision_event_store", RiskLevel::Low, message);
            }
        };
        let summary = build_redacted_vision_event_summary(&stored);
        let message = latest_vision_event_reply(&stored);
        let _ = self.remember_family_memory_event(request, &stored);
        let event = self.serialize_event_record(&build_task_event_record(
            request,
            &step_id_for_request(request),
            "local_vision_event.summary_read",
            EventSeverity::Info,
            json!({
                "status": "summarized",
                "metadata_only": true,
                "secret_scan": "clean",
                "event_id": stored.event.event_id,
                "camera_id": stored.event.camera_id,
                "raw_image_included": false,
                "local_paths_included": false,
            }),
        ));
        self.completed_with_context(
            request,
            "vision_event_store",
            RiskLevel::Low,
            message.clone(),
            json!({
                "reply_pack": {
                    "kind": "vision_event_summary",
                    "status": "summarized",
                    "summary": message,
                    "event": summary,
                }
            }),
            Vec::new(),
            vec![event],
            vec![
                "通知最新事件".to_string(),
                "状态".to_string(),
                "拍一张门口".to_string(),
            ],
        )
    }

    pub(super) fn handle_general_message_vlm_describe_latest_event(
        &self,
        request: &TaskRequest,
        plan: &GeneralMessagePlan,
    ) -> TaskResponse {
        let stored = match latest_local_vision_event() {
            Ok(Some(stored)) => stored,
            Ok(None) => {
                let message = "最近还没有可理解的摄像头事件。".to_string();
                return self.completed_with_context(
                    request,
                    "vision_vlm_enrichment",
                    RiskLevel::Low,
                    message.clone(),
                    json!({
                        "reply_pack": {
                            "kind": "vlm_describe_latest_event",
                            "status": "blocked",
                            "reason": "no_recent_event",
                            "summary": message,
                            "redacted": true,
                        }
                    }),
                    Vec::new(),
                    Vec::new(),
                    vec!["拍一张门口".to_string(), "最近事件".to_string()],
                );
            }
            Err(error) => {
                return self.failed(
                    request,
                    "vision_vlm_enrichment",
                    RiskLevel::Low,
                    format!("读取最近摄像头事件失败：{}", redact_task_api_text(&error)),
                )
            }
        };

        if stored
            .event
            .vlm
            .as_ref()
            .is_some_and(|vlm| vlm.status == "active" && !vlm.summary.trim().is_empty())
        {
            return self.general_message_vlm_describe_response(
                request,
                &stored,
                "active",
                "cached_event_vlm",
                "latest_event",
                plan,
            );
        }

        let admin_state = match self.admin_store.load_state() {
            Ok(state) => state,
            Err(error) => {
                return self.failed(
                    request,
                    "vision_vlm_enrichment",
                    RiskLevel::Low,
                    format!("读取模型配置失败：{}", redact_task_api_text(&error)),
                )
            }
        };
        let snapshot = match self
            .hub()
            .capture_camera_snapshot_result(&stored.event.camera_id)
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let message = format!(
                    "最新事件还没有 VLM 摘要，且抓取当前帧失败：{}",
                    redact_task_api_text(&error)
                );
                return self.general_message_vlm_blocked_response(
                    request,
                    &stored,
                    "snapshot_failed",
                    &message,
                    plan,
                );
            }
        };
        let temp_dir =
            std::env::temp_dir().join(format!("harbornavi-task-vlm-{}", Uuid::new_v4().simple()));
        let frame_path = temp_dir.join("frame.jpg");
        let started = Instant::now();
        let result = (|| -> Result<StoredLocalVisionEvent, String> {
            fs::create_dir_all(&temp_dir)
                .map_err(|error| format!("failed to create VLM temp dir: {error}"))?;
            let bytes = BASE64_STANDARD
                .decode(snapshot.bytes_base64.as_bytes())
                .map_err(|error| format!("failed to decode VLM snapshot: {error}"))?;
            fs::write(&frame_path, bytes)
                .map_err(|error| format!("failed to write VLM temp frame: {error}"))?;
            let execution = run_vlm_summary_with_state(&frame_path, &admin_state.models);
            if execution.status == "busy" {
                return Err("vlm_queue_busy".to_string());
            }
            if execution.status == "blocked" {
                return Err("vlm_local_only_blocked".to_string());
            }
            let elapsed_ms = started.elapsed().as_millis() as u64;
            let enriched = attach_vlm_summary_to_event(
                stored.event.clone(),
                execution.clone(),
                elapsed_ms,
                json!({
                    "trigger": "wechat_nsp",
                    "decision": if plan.kind == GeneralMessagePlanKind::VlmDescribeEvent {
                        "vlm_describe_event"
                    } else {
                        "vlm_describe_latest_event"
                    },
                    "frame_source": "fresh_camera_snapshot",
                    "queue_mode": "global_serial_try_lock",
                    "local_only": true,
                    "fallback_allowed": false,
                    "model_endpoint_id": execution.model_endpoint_id,
                    "provider_key": execution.provider_key,
                }),
            );
            ingest_local_vision_event_default(enriched)
        })();
        let _ = fs::remove_dir_all(&temp_dir);

        match result {
            Ok(enriched) => {
                let status = enriched
                    .event
                    .vlm
                    .as_ref()
                    .map(|vlm| vlm.status.as_str())
                    .unwrap_or("degraded");
                let reason = if status == "active" {
                    "fresh_vlm_enriched"
                } else if status == "busy" {
                    "vlm_queue_busy"
                } else if status == "blocked" {
                    "vlm_local_only_blocked"
                } else {
                    "vlm_degraded"
                };
                self.general_message_vlm_describe_response(
                    request,
                    &enriched,
                    status,
                    reason,
                    "fresh_camera_snapshot",
                    plan,
                )
            }
            Err(error) => {
                let reason = match error.as_str() {
                    "vlm_queue_busy" => "queue_busy",
                    "vlm_local_only_blocked" => "local_only_blocked",
                    _ => "vlm_failed",
                };
                let message = match reason {
                    "queue_busy" => "VLM 正在处理上一条请求，这次没有更新事件摘要。".to_string(),
                    "local_only_blocked" => {
                        "VLM 只允许本地端点执行，当前配置被安全策略阻止。".to_string()
                    }
                    _ => format!("VLM 理解失败：{}", redact_task_api_text(&error)),
                };
                self.general_message_vlm_blocked_response(request, &stored, reason, &message, plan)
            }
        }
    }

    pub(super) fn handle_general_message_notify_latest_vision_event(
        &self,
        request: &TaskRequest,
    ) -> TaskResponse {
        let stored = match latest_local_vision_event() {
            Ok(Some(stored)) => stored,
            Ok(None) => {
                return self.general_message_vision_event_notification_response(
                    request,
                    None,
                    None,
                    "blocked",
                    "最近还没有可通知的摄像头事件。",
                    None,
                    "no_recent_event",
                    EventSeverity::Warning,
                );
            }
            Err(error) => {
                let message = format!("读取最近摄像头事件失败：{}", redact_task_api_text(&error));
                return self.failed(request, "notification_delivery", RiskLevel::Low, message);
            }
        };
        let state = match self.admin_store.load_or_create_state() {
            Ok(state) => state,
            Err(error) => {
                return self.general_message_vision_event_notification_response(
                    request,
                    Some(&stored),
                    None,
                    "failed",
                    &format!("读取默认通知目标失败：{}", redact_task_api_text(&error)),
                    None,
                    "state_unavailable",
                    EventSeverity::Error,
                );
            }
        };
        let target = match default_notification_target_record(&state.notification_targets).cloned()
        {
            Some(target) => target,
            None => {
                return self.general_message_vision_event_notification_response(
                    request,
                    Some(&stored),
                    None,
                    "blocked",
                    "还没有配置默认通知目标，最新事件没有发出。",
                    None,
                    "no_default_target",
                    EventSeverity::Warning,
                );
            }
        };
        let intent = match build_local_vision_notification_intent(&stored, &target.route_key) {
            Ok(intent) => intent,
            Err(error) => {
                return self.general_message_vision_event_notification_response(
                    request,
                    Some(&stored),
                    Some(&target),
                    "failed",
                    &format!("生成事件通知失败：{}", redact_task_api_text(&error)),
                    None,
                    "intent_failed",
                    EventSeverity::Error,
                );
            }
        };
        let service = match NotificationDeliveryService::new() {
            Ok(service) => service,
            Err(error) => {
                return self.general_message_vision_event_notification_response(
                    request,
                    Some(&stored),
                    Some(&target),
                    "blocked",
                    &format!(
                        "HarborGate 通知通道不可用：{}",
                        redact_task_api_text(&error)
                    ),
                    None,
                    "gateway_unavailable",
                    EventSeverity::Warning,
                );
            }
        };
        match service.deliver(&intent.notification_request) {
            Ok(record) if record.ok => self.general_message_vision_event_notification_response(
                request,
                Some(&stored),
                Some(&target),
                "delivered",
                "已把最新摄像头事件发送到默认通知目标。",
                Some(notification_delivery_record_summary(&record)),
                "delivered",
                EventSeverity::Info,
            ),
            Ok(record) => self.general_message_vision_event_notification_response(
                request,
                Some(&stored),
                Some(&target),
                "failed",
                "HarborGate 返回投递失败记录，最新事件没有确认送达。",
                Some(notification_delivery_record_summary(&record)),
                "delivery_record_failed",
                EventSeverity::Error,
            ),
            Err(error) => {
                let (status, message, reason, severity) =
                    classify_task_notification_delivery_error(error);
                self.general_message_vision_event_notification_response(
                    request,
                    Some(&stored),
                    Some(&target),
                    status,
                    &message,
                    None,
                    reason,
                    severity,
                )
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn general_message_vision_event_notification_response(
        &self,
        request: &TaskRequest,
        stored: Option<&StoredLocalVisionEvent>,
        target: Option<&NotificationTargetRecord>,
        status: &str,
        message: &str,
        delivery_record: Option<Value>,
        reason: &str,
        severity: EventSeverity,
    ) -> TaskResponse {
        let event_type = match status {
            "delivered" => "local_vision_event.notification_delivered",
            "blocked" => "local_vision_event.notification_blocked",
            _ => "local_vision_event.notification_failed",
        };
        let event = self.serialize_event_record(&build_task_event_record(
            request,
            &step_id_for_request(request),
            event_type,
            severity,
            json!({
                "status": status,
                "reason": reason,
                "metadata_only": true,
                "secret_scan": "clean",
                "event_id": stored.map(|stored| stored.event.event_id.clone()),
                "camera_id": stored.map(|stored| stored.event.camera_id.clone()),
                "target_bound": target.is_some(),
                "target_label": target.map(|target| target.label.clone()),
                "platform_hint": target.and_then(|target| non_empty_task_string(&target.platform_hint)),
                "route_key_redacted": target.is_some(),
                "text_only": true,
                "attachments_included": false,
                "raw_image_included": false,
                "local_paths_included": false,
            }),
        ));
        let redacted_message = redact_task_api_text(message);
        self.completed_with_context(
            request,
            "notification_delivery",
            RiskLevel::Low,
            redacted_message.clone(),
            json!({
                "reply_pack": {
                    "kind": "vision_event_notify_latest",
                    "status": status,
                    "summary": redacted_message,
                    "event": stored.map(build_redacted_vision_event_summary),
                    "target": {
                        "configured": target.is_some(),
                        "label": target.map(|target| target.label.clone()),
                        "platform_hint": target.and_then(|target| non_empty_task_string(&target.platform_hint)),
                        "route_key_redacted": target.is_some(),
                    },
                    "delivery_record": delivery_record,
                }
            }),
            Vec::new(),
            vec![event],
            vec!["最近事件".to_string(), "状态".to_string()],
        )
    }

    fn general_message_vlm_describe_response(
        &self,
        request: &TaskRequest,
        stored: &StoredLocalVisionEvent,
        status: &str,
        reason: &str,
        frame_source: &str,
        plan: &GeneralMessagePlan,
    ) -> TaskResponse {
        let message = vlm_describe_event_reply(stored, status, reason);
        let event = self.serialize_event_record(&build_task_event_record(
            request,
            &step_id_for_request(request),
            "local_vision_event.vlm_describe",
            EventSeverity::Info,
            json!({
                "status": status,
                "reason": reason,
                "event_id": stored.event.event_id,
                "camera_id": stored.event.camera_id,
                "frame_source": frame_source,
                "frame_path_redacted": true,
                "raw_image_included": false,
                "local_paths_included": false,
                "metadata_only": true,
                "secret_scan": "clean",
            }),
        ));
        let kind = if plan.kind == GeneralMessagePlanKind::VlmDescribeEvent {
            "vlm_describe_event"
        } else {
            "vlm_describe_latest_event"
        };
        let _ = self.remember_family_memory_event(request, stored);
        self.completed_with_context(
            request,
            "vision_vlm_enrichment",
            RiskLevel::Low,
            message.clone(),
            json!({
                "reply_pack": {
                    "kind": kind,
                    "status": status,
                    "reason": reason,
                    "summary": message,
                    "event": build_redacted_vision_event_summary(stored),
                    "summary_source": if status == "active" { "vlm" } else { "degraded_fallback" },
                    "frame_source": frame_source,
                    "frame_path_redacted": true,
                    "raw_image_included": false,
                    "redacted": true,
                }
            }),
            Vec::new(),
            vec![event],
            vec!["家庭时间线".to_string(), "通知最新事件".to_string()],
        )
    }

    fn general_message_vlm_blocked_response(
        &self,
        request: &TaskRequest,
        stored: &StoredLocalVisionEvent,
        reason: &str,
        message: &str,
        plan: &GeneralMessagePlan,
    ) -> TaskResponse {
        let event = self.serialize_event_record(&build_task_event_record(
            request,
            &step_id_for_request(request),
            "local_vision_event.vlm_describe_blocked",
            EventSeverity::Warning,
            json!({
                "status": "blocked",
                "reason": reason,
                "event_id": stored.event.event_id,
                "camera_id": stored.event.camera_id,
                "raw_image_included": false,
                "local_paths_included": false,
                "metadata_only": true,
                "secret_scan": "clean",
            }),
        ));
        let kind = if plan.kind == GeneralMessagePlanKind::VlmDescribeEvent {
            "vlm_describe_event"
        } else {
            "vlm_describe_latest_event"
        };
        let redacted_message = redact_task_api_text(message);
        self.completed_with_context(
            request,
            "vision_vlm_enrichment",
            RiskLevel::Low,
            redacted_message.clone(),
            json!({
                "reply_pack": {
                    "kind": kind,
                    "status": "blocked",
                    "reason": reason,
                    "summary": redacted_message,
                    "event": build_redacted_vision_event_summary(stored),
                    "summary_source": "degraded_fallback",
                    "raw_image_included": false,
                    "redacted": true,
                }
            }),
            Vec::new(),
            vec![event],
            vec!["最近事件".to_string(), "状态".to_string()],
        )
    }
}

pub(super) fn latest_local_vision_event() -> Result<Option<StoredLocalVisionEvent>, String> {
    Ok(list_recent_local_vision_events_default(1)?
        .into_iter()
        .next())
}

pub(super) fn build_redacted_vision_event_summary(stored: &StoredLocalVisionEvent) -> Value {
    let event = &stored.event;
    json!({
        "event_id": event.event_id,
        "summary": event.summary,
        "labels": event.labels,
        "confidence": event.confidence,
        "camera_id": event.camera_id,
        "event_type": event.event_type,
        "started_at": event.started_at,
        "received_at": stored.received_at,
        "latency_ms": event.latency_ms,
        "analyzer": event.analyzer,
        "vlm": event.vlm.as_ref().map(|vlm| json!({
            "status": vlm.status,
            "summary": vlm.summary,
            "tags": vlm.tags,
            "labels": vlm.labels,
            "artifact_count": vlm.artifacts.len(),
            "artifacts": vlm.artifacts.iter().map(redacted_vision_event_artifact).collect::<Vec<_>>(),
            "error": vlm.error.as_ref().map(|error| redact_task_api_text(error)),
        })),
        "snapshot_artifact": redacted_snapshot_artifact(&event.snapshot_artifact),
        "raw_image_included": false,
        "local_paths_included": false,
        "redacted": true,
    })
}

fn redacted_snapshot_artifact(artifact: &SnapshotArtifact) -> Value {
    json!({
        "artifact_id": artifact.artifact_id,
        "mime_type": artifact.mime_type,
        "byte_size": artifact.byte_size,
        "sha256_present": artifact.sha256.as_ref().is_some_and(|value| !value.trim().is_empty()),
        "source": artifact.source,
        "path_redacted": artifact.path.as_ref().is_some(),
    })
}

fn redacted_vision_event_artifact(artifact: &LocalVisionEventArtifact) -> Value {
    json!({
        "artifact_id": artifact.artifact_id,
        "role": artifact.role,
        "mime_type": artifact.mime_type,
        "byte_size": artifact.byte_size,
        "sha256_present": artifact.sha256.as_ref().is_some_and(|value| !value.trim().is_empty()),
        "source": artifact.source,
    })
}

pub(super) fn latest_vision_event_reply(stored: &StoredLocalVisionEvent) -> String {
    let event = &stored.event;
    let labels = if event.labels.is_empty() {
        "无标签".to_string()
    } else {
        event.labels.join("、")
    };
    let vlm_status = event
        .vlm
        .as_ref()
        .map(|vlm| vlm.status.as_str())
        .unwrap_or("not_available");
    format!(
        "最近事件：{}；标签：{}；置信度：{:.2}；摄像头：{}；时间：{}；延迟：{}ms；VLM：{}。",
        event.summary,
        labels,
        event.confidence,
        event.camera_id,
        event.started_at,
        event.latency_ms,
        vlm_status
    )
}

fn vlm_describe_event_reply(stored: &StoredLocalVisionEvent, status: &str, reason: &str) -> String {
    let event = &stored.event;
    let vlm_text = event
        .vlm
        .as_ref()
        .and_then(|vlm| {
            let text = if !vlm.summary.trim().is_empty() {
                vlm.summary.trim()
            } else {
                vlm.derived_text.trim()
            };
            (!text.is_empty()).then(|| text.to_string())
        })
        .unwrap_or_else(|| event.summary.clone());
    let labels = if event.labels.is_empty() {
        "无标签".to_string()
    } else {
        event.labels.join("、")
    };
    let prefix = if status == "active" {
        "VLM 理解"
    } else {
        "VLM 未完成，先用事件摘要"
    };
    format!(
        "{}：{}；摄像头：{}；标签：{}；时间：{}；状态：{}（{}）。",
        prefix,
        redact_task_api_text(&vlm_text),
        event.camera_id,
        labels,
        event.started_at,
        status,
        reason
    )
}
