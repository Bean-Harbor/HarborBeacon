//! General-message Home Assistant action handling.

use serde_json::{json, Value};

use crate::connectors::home_assistant::{
    normalize_home_assistant_service_action_request,
    validate_home_assistant_service_action_request, HomeAssistantEntity,
    HomeAssistantServiceActionRequest,
};
use crate::control_plane::events::EventSeverity;
use crate::orchestrator::contracts::RiskLevel;
use crate::runtime::task_session::{
    PendingTaskGeneralMessageLoop, PendingTaskHomeAssistantClarification,
};

use super::{
    build_task_event_record, ensure_resume_token, home_assistant_clarification_prompt,
    home_assistant_client_from_admin_state, home_assistant_pending_clarification_actions,
    infer_home_assistant_natural_action, non_empty_task_string,
    pending_home_assistant_candidates_from_entities, redact_task_api_text,
    redacted_home_assistant_entity_summary, redacted_pending_home_assistant_candidate_summary,
    resolve_home_assistant_action_entity, step_id_for_request, HomeAssistantEntityResolution,
    HomeAssistantNaturalAction, HomeAssistantNaturalActionRequest, TaskApiService, TaskRequest,
    TaskResponse,
};

impl TaskApiService {
    pub(super) fn handle_general_message_home_assistant_action(
        &self,
        request: &TaskRequest,
        nsp_action: Option<&HomeAssistantNaturalAction>,
    ) -> TaskResponse {
        let Some(action) = nsp_action
            .cloned()
            .or_else(|| infer_home_assistant_natural_action(request.intent.raw_text.as_str()))
        else {
            return self.general_message_unsupported_response(request, None);
        };
        let action = match action {
            HomeAssistantNaturalAction::Blocked { message } => {
                return self.general_message_home_assistant_action_response(
                    request,
                    "blocked",
                    false,
                    false,
                    &message,
                    None,
                    HomeAssistantServiceActionRequest::default(),
                    "home_assistant.service_action_blocked",
                    EventSeverity::Warning,
                    None,
                );
            }
            HomeAssistantNaturalAction::Request(action) => action,
        };
        let state = match self.admin_store.home_assistant_secret_state() {
            Ok(state) => state,
            Err(error) => {
                return self.general_message_home_assistant_action_response(
                    request,
                    "failed",
                    false,
                    false,
                    &format!(
                        "读取 Home Assistant 配置失败：{}",
                        redact_task_api_text(&error)
                    ),
                    None,
                    HomeAssistantServiceActionRequest {
                        domain: action.domain.clone(),
                        service: action.service.clone(),
                        fields: json!({}),
                        ..Default::default()
                    },
                    "home_assistant.service_action_failed",
                    EventSeverity::Error,
                    None,
                );
            }
        };
        let client = match home_assistant_client_from_admin_state(&state) {
            Ok(client) => client,
            Err(error) => {
                return self.general_message_home_assistant_action_response(
                    request,
                    "blocked",
                    false,
                    false,
                    &redact_task_api_text(&error),
                    None,
                    HomeAssistantServiceActionRequest {
                        domain: action.domain.clone(),
                        service: action.service.clone(),
                        fields: json!({}),
                        ..Default::default()
                    },
                    "home_assistant.service_action_blocked",
                    EventSeverity::Warning,
                    None,
                );
            }
        };
        let entities = match client.fetch_entities() {
            Ok(entities) => entities,
            Err(error) => {
                return self.general_message_home_assistant_action_response(
                    request,
                    "failed",
                    false,
                    false,
                    &format!(
                        "读取 Home Assistant 实体失败：{}",
                        redact_task_api_text(&error)
                    ),
                    None,
                    HomeAssistantServiceActionRequest {
                        domain: action.domain.clone(),
                        service: action.service.clone(),
                        fields: json!({}),
                        ..Default::default()
                    },
                    "home_assistant.service_action_failed",
                    EventSeverity::Error,
                    None,
                );
            }
        };
        let entity = match resolve_home_assistant_action_entity(&action, &entities) {
            HomeAssistantEntityResolution::Resolved(entity) => entity,
            HomeAssistantEntityResolution::Clarify(candidates) => {
                return self.general_message_home_assistant_clarification_response(
                    request,
                    &action,
                    &candidates,
                    "ambiguous_entity",
                    None,
                );
            }
            HomeAssistantEntityResolution::Missing(message) => {
                return self.general_message_home_assistant_action_response(
                    request,
                    "blocked",
                    false,
                    false,
                    &message,
                    None,
                    HomeAssistantServiceActionRequest {
                        domain: action.domain.clone(),
                        service: action.service.clone(),
                        fields: json!({}),
                        ..Default::default()
                    },
                    "home_assistant.service_action_blocked",
                    EventSeverity::Warning,
                    None,
                );
            }
        };
        let action_request =
            normalize_home_assistant_service_action_request(&HomeAssistantServiceActionRequest {
                entity_id: entity.entity_id.clone(),
                domain: action.domain.clone(),
                service: action.service.clone(),
                fields: json!({}),
            });
        if let Err(message) = validate_home_assistant_service_action_request(
            &action_request,
            state.enabled,
            &state.exposed_domains,
        ) {
            return self.general_message_home_assistant_action_response(
                request,
                "blocked",
                false,
                false,
                &message,
                None,
                action_request,
                "home_assistant.service_action_blocked",
                EventSeverity::Warning,
                Some(&entity),
            );
        }
        match client.call_service(
            &action_request.domain,
            &action_request.service,
            &action_request.entity_id,
            Some(&action_request.fields),
        ) {
            Ok(result) => self.general_message_home_assistant_action_response(
                request,
                "succeeded",
                true,
                true,
                &format!(
                    "已执行：{} {}.{}。",
                    entity.display_name, action_request.domain, action_request.service
                ),
                Some(json!(result)),
                action_request,
                "home_assistant.service_action_executed",
                EventSeverity::Info,
                Some(&entity),
            ),
            Err(error) => self.general_message_home_assistant_action_response(
                request,
                "failed",
                true,
                false,
                &format!(
                    "Home Assistant 动作执行失败：{}",
                    redact_task_api_text(&error)
                ),
                None,
                action_request,
                "home_assistant.service_action_failed",
                EventSeverity::Error,
                Some(&entity),
            ),
        }
    }

    pub(super) fn general_message_home_assistant_clarification_response(
        &self,
        request: &TaskRequest,
        action: &HomeAssistantNaturalActionRequest,
        candidates: &[HomeAssistantEntity],
        reason: &str,
        prior_pending: Option<&PendingTaskGeneralMessageLoop>,
    ) -> TaskResponse {
        let pending_candidates = pending_home_assistant_candidates_from_entities(candidates);
        let message = home_assistant_clarification_prompt(action, pending_candidates.as_slice());
        let resume_token = prior_pending
            .map(|pending| pending.resume_token.clone())
            .filter(|token| !token.trim().is_empty())
            .unwrap_or_else(ensure_resume_token);
        let pending_ha = PendingTaskHomeAssistantClarification {
            domain: action.domain.clone(),
            service: action.service.clone(),
            entity_hint: action.entity_hint.clone(),
            candidates: pending_candidates,
        };
        let mut conversation = self.load_or_create_conversation(request);
        conversation.set_general_message_loop(Some(PendingTaskGeneralMessageLoop {
            resume_token: resume_token.clone(),
            original_goal: prior_pending
                .map(|pending| pending.original_goal.clone())
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| request.intent.raw_text.trim().to_string()),
            latest_user_intent_text: request.intent.raw_text.trim().to_string(),
            last_clarification_prompt: message.clone(),
            selected_candidate_action: None,
            camera_hint: prior_pending.and_then(|pending| pending.camera_hint.clone()),
            query: prior_pending.and_then(|pending| pending.query.clone()),
            home_assistant: Some(pending_ha.clone()),
        }));
        if let Err(error) = self.save_conversation(request, &conversation) {
            return self.failed(
                request,
                "agentic_interpreter",
                RiskLevel::Low,
                format!("无法保存 Home Assistant 澄清状态: {error}"),
            );
        }

        let event = self.serialize_event_record(&build_task_event_record(
            request,
            &step_id_for_request(request),
            "home_assistant.service_action_clarification_required",
            EventSeverity::Warning,
            json!({
                "status": "needs_input",
                "reason": reason,
                "metadata_only": true,
                "secret_scan": "clean",
                "domain": action.domain,
                "service": action.service,
                "candidate_count": pending_ha.candidates.len(),
                "executed": false,
            }),
        ));
        self.needs_input_with_context(
            request,
            "agentic_interpreter",
            RiskLevel::Low,
            message.clone(),
            vec!["home_assistant_entity".to_string()],
            resume_token,
            json!({
                "reply_pack": {
                    "kind": "ha_action_clarify",
                    "status": "needs_input",
                    "summary": message,
                    "domain": action.domain,
                    "service": action.service,
                    "candidates": pending_ha.candidates.iter().map(redacted_pending_home_assistant_candidate_summary).collect::<Vec<_>>(),
                    "redacted": true,
                },
                "home_assistant_clarification": {
                    "kind": "home_assistant.service_action_clarification",
                    "reason": reason,
                    "candidate_count": pending_ha.candidates.len(),
                    "redacted": true,
                }
            }),
            vec![event],
            home_assistant_pending_clarification_actions(&pending_ha.candidates),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn general_message_home_assistant_action_response(
        &self,
        request: &TaskRequest,
        status: &str,
        allowed: bool,
        executed: bool,
        message: &str,
        result: Option<Value>,
        action_request: HomeAssistantServiceActionRequest,
        audit_kind: &str,
        severity: EventSeverity,
        entity: Option<&HomeAssistantEntity>,
    ) -> TaskResponse {
        let redacted_message = redact_task_api_text(message);
        let event = self.serialize_event_record(&build_task_event_record(
            request,
            &step_id_for_request(request),
            audit_kind,
            severity,
            json!({
                "audit_kind": audit_kind,
                "status": status,
                "metadata_only": true,
                "secret_scan": "clean",
                "allowed": allowed,
                "executed": executed,
                "entity_id": non_empty_task_string(&action_request.entity_id),
                "entity_display_name": entity.map(|entity| entity.display_name.clone()),
                "domain": non_empty_task_string(&action_request.domain),
                "service": non_empty_task_string(&action_request.service),
                "fields_empty": action_request.fields.as_object().map(|object| object.is_empty()).unwrap_or(true),
                "message": redacted_message,
            }),
        ));
        self.completed_with_context(
            request,
            "home_assistant_connector",
            RiskLevel::Low,
            redacted_message.clone(),
            json!({
                "reply_pack": {
                    "kind": "ha_service_action",
                    "status": status,
                    "summary": redacted_message,
                    "allowed": allowed,
                    "executed": executed,
                    "domain": non_empty_task_string(&action_request.domain),
                    "service": non_empty_task_string(&action_request.service),
                    "entity": entity.map(redacted_home_assistant_entity_summary),
                    "fields": {},
                    "result": result,
                    "redacted": true,
                }
            }),
            Vec::new(),
            vec![event],
            vec!["状态".to_string(), "最近事件".to_string()],
        )
    }
}
