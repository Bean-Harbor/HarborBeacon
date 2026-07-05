//! General-message system readiness action handling.

use serde_json::{json, Value};

use crate::control_plane::events::EventSeverity;
use crate::orchestrator::contracts::RiskLevel;
use crate::runtime::admin_console::AdminConsoleState;
use crate::runtime::vision_event::StoredLocalVisionEvent;

use super::vision_event_actions::latest_local_vision_event;
use super::{
    build_task_default_notification_target_readiness, build_task_event_record,
    build_task_weixin_gateway_status, fetch_task_gateway_status, redact_task_api_text,
    step_id_for_request, TaskApiService, TaskRequest, TaskResponse,
};

impl TaskApiService {
    pub(super) fn handle_general_message_system_readiness(
        &self,
        request: &TaskRequest,
    ) -> TaskResponse {
        let state = match self.admin_store.load_or_create_state() {
            Ok(state) => state,
            Err(error) => {
                return self.failed(
                    request,
                    "agentic_interpreter",
                    RiskLevel::Low,
                    format!("读取状态失败：{}", redact_task_api_text(&error)),
                );
            }
        };
        let gateway_status = fetch_task_gateway_status().ok();
        let camera_count = self
            .admin_store
            .registry_store()
            .load_devices()
            .map(|devices| devices.len())
            .unwrap_or(0);
        let latest_event = latest_local_vision_event().ok().flatten();
        let readiness = build_general_message_readiness_summary(
            &state,
            gateway_status.as_ref(),
            camera_count,
            latest_event.as_ref(),
        );
        let message =
            format!(
            "当前状态：微信 {}；默认通知目标 {}；HA {}（{} 个实体）；摄像头 {} 个，最近事件 {}。",
            readiness["gateway"]["weixin"]["status"]
                .as_str()
                .unwrap_or("unknown"),
            readiness["default_notification_target"]["status"]
                .as_str()
                .unwrap_or("unknown"),
            readiness["home_assistant"]["status"]
                .as_str()
                .unwrap_or("unknown"),
            readiness["home_assistant"]["entity_count"]
                .as_u64()
                .unwrap_or(0),
            camera_count,
            if latest_event.is_some() { "可用" } else { "暂无" },
        );
        let event = self.serialize_event_record(&build_task_event_record(
            request,
            &step_id_for_request(request),
            "general_message.system_readiness_read",
            EventSeverity::Info,
            json!({
                "status": "summarized",
                "metadata_only": true,
                "secret_scan": "clean",
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
                    "kind": "system_readiness",
                    "summary": message,
                    "readiness": readiness,
                    "redacted": true,
                }
            }),
            Vec::new(),
            vec![event],
            vec!["最近事件".to_string(), "通知最新事件".to_string()],
        )
    }
}

pub(super) fn build_general_message_readiness_summary(
    state: &AdminConsoleState,
    gateway_status: Option<&Value>,
    camera_count: usize,
    latest_event: Option<&StoredLocalVisionEvent>,
) -> Value {
    json!({
        "gateway": {
            "weixin": build_task_weixin_gateway_status(gateway_status),
        },
        "default_notification_target": build_task_default_notification_target_readiness(
            &state.notification_targets,
            gateway_status,
        ),
        "home_assistant": {
            "configured": !state.home_assistant.base_url.trim().is_empty()
                && !state.home_assistant.access_token.trim().is_empty(),
            "enabled": state.home_assistant.enabled,
            "status": state.home_assistant.last_status,
            "token_configured": !state.home_assistant.access_token.trim().is_empty(),
            "token_redacted": !state.home_assistant.access_token.trim().is_empty(),
            "last_sync_at": state.home_assistant.last_sync_at,
            "entity_count": state.home_assistant.entity_count,
            "service_count": state.home_assistant.service_count,
            "exposed_domains": state.home_assistant.exposed_domains,
        },
        "camera": {
            "camera_count": camera_count,
            "event_available": latest_event.is_some(),
            "latest_event_id": latest_event.map(|stored| stored.event.event_id.clone()),
            "latest_event_camera_id": latest_event.map(|stored| stored.event.camera_id.clone()),
        },
        "redacted": true,
    })
}
