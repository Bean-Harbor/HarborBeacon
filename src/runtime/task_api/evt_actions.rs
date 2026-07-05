//! General-message EVT readiness, preflight, and evidence actions.

use serde_json::{json, Value};

use crate::control_plane::events::EventSeverity;
use crate::orchestrator::contracts::RiskLevel;
use crate::runtime::evt_readiness::{
    build_evt_evidence_bundle, build_evt_readiness_report, evt_evidence_reply_summary,
    evt_preflight_reply_summary, evt_preflight_workflow_summary, evt_status_reply_summary,
    run_evt_preflight_report,
};

use super::{
    build_task_event_record, fetch_task_gateway_status, redact_task_api_text, step_id_for_request,
    TaskApiService, TaskRequest, TaskResponse,
};

impl TaskApiService {
    pub(super) fn handle_general_message_evt_readiness(
        &self,
        request: &TaskRequest,
    ) -> TaskResponse {
        let gateway_status = fetch_task_gateway_status().ok();
        let readiness = match build_evt_readiness_report(&self.admin_store, gateway_status.as_ref())
        {
            Ok(readiness) => readiness,
            Err(error) => {
                return self.failed(
                    request,
                    "agentic_interpreter",
                    RiskLevel::Low,
                    format!("读取 EVT 就绪状态失败：{}", redact_task_api_text(&error)),
                );
            }
        };
        let latest_preflight = self.last_evt_preflight();
        let message = evt_status_reply_summary(&readiness, latest_preflight.as_ref());
        let event = self.serialize_event_record(&build_task_event_record(
            request,
            &step_id_for_request(request),
            "evt.readiness_read",
            EventSeverity::Info,
            json!({
                "status": readiness.get("status").and_then(Value::as_str).unwrap_or("degraded"),
                "blocker_count": readiness
                    .get("blockers")
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or(0),
                "warning_count": readiness
                    .get("warnings")
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or(0),
                "metadata_only": true,
                "redacted": true,
            }),
        ));
        self.completed_with_context(
            request,
            "agentic_interpreter",
            RiskLevel::Low,
            message.clone(),
            json!({
                "reply_pack": {
                    "kind": "evt_readiness",
                    "summary": message,
                    "readiness": readiness,
                    "latest_preflight": evt_preflight_workflow_summary(latest_preflight),
                    "redacted": true,
                }
            }),
            Vec::new(),
            vec![event],
            vec!["帮我做一下EVT预检".to_string(), "生成压测证据".to_string()],
        )
    }

    pub(super) fn handle_general_message_evt_preflight(
        &self,
        request: &TaskRequest,
    ) -> TaskResponse {
        let gateway_status = fetch_task_gateway_status().ok();
        let preflight = match run_evt_preflight_report(&self.admin_store, gateway_status.as_ref()) {
            Ok(preflight) => preflight,
            Err(error) => {
                return self.failed(
                    request,
                    "agentic_interpreter",
                    RiskLevel::Low,
                    format!("运行 EVT 预检失败：{}", redact_task_api_text(&error)),
                );
            }
        };
        self.record_last_evt_preflight(&preflight);
        let message = evt_preflight_reply_summary(&preflight);
        let event = self.serialize_event_record(&build_task_event_record(
            request,
            &step_id_for_request(request),
            "evt.preflight_run",
            EventSeverity::Info,
            json!({
                "status": preflight.get("status").and_then(Value::as_str).unwrap_or("degraded"),
                "duration_ms": preflight.get("duration_ms").and_then(Value::as_u64),
                "long_run_started": false,
                "short_run_started": false,
                "metadata_only": true,
                "redacted": true,
            }),
        ));
        self.completed_with_context(
            request,
            "agentic_interpreter",
            RiskLevel::Low,
            message.clone(),
            json!({
                "reply_pack": {
                    "kind": "evt_preflight",
                    "summary": message,
                    "preflight": preflight,
                    "redacted": true,
                }
            }),
            Vec::new(),
            vec![event],
            vec!["压测前状态怎么样".to_string(), "生成压测证据".to_string()],
        )
    }

    pub(super) fn handle_general_message_evt_evidence_bundle(
        &self,
        request: &TaskRequest,
    ) -> TaskResponse {
        let gateway_status = fetch_task_gateway_status().ok();
        let readiness = match build_evt_readiness_report(&self.admin_store, gateway_status.as_ref())
        {
            Ok(readiness) => readiness,
            Err(error) => {
                return self.failed(
                    request,
                    "agentic_interpreter",
                    RiskLevel::Low,
                    format!("生成 EVT 证据包失败：{}", redact_task_api_text(&error)),
                );
            }
        };
        let bundle = build_evt_evidence_bundle(
            readiness,
            self.last_evt_preflight(),
            json!({
                "source": "task_api.general_message",
                "metadata_only": true,
                "redacted": true,
            }),
        );
        let message = evt_evidence_reply_summary(&bundle);
        let event = self.serialize_event_record(&build_task_event_record(
            request,
            &step_id_for_request(request),
            "evt.evidence_bundle_generated",
            EventSeverity::Info,
            json!({
                "status": bundle.get("status").and_then(Value::as_str).unwrap_or("degraded"),
                "metadata_only": true,
                "redacted": true,
            }),
        ));
        self.completed_with_context(
            request,
            "agentic_interpreter",
            RiskLevel::Low,
            message.clone(),
            json!({
                "reply_pack": {
                    "kind": "evt_evidence_bundle",
                    "summary": message,
                    "evidence_bundle": bundle,
                    "redacted": true,
                }
            }),
            Vec::new(),
            vec![event],
            vec![
                "压测前状态怎么样".to_string(),
                "帮我做一下EVT预检".to_string(),
            ],
        )
    }
}
