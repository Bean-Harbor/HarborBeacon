//! Family timeline and Home Guardian helpers used by general-message task actions.

use serde_json::{json, Value};
use uuid::Uuid;

use crate::control_plane::events::EventSeverity;
use crate::orchestrator::contracts::RiskLevel;
use crate::runtime::admin_console::AutomationRuleReview;
use crate::runtime::family_memory::{
    append_family_memory_feedback_default, apply_family_memory_overlays_to_events,
    build_family_memory_stats, compact_family_memory_feedback_evidence,
    family_memory_overlays_default, list_family_memory_feedback_default,
    FamilyMemoryFeedbackRequest, DEFAULT_FAMILY_MEMORY_FEEDBACK_LIMIT,
};
use crate::runtime::family_timeline::{
    build_family_timeline_digest_from_query, FamilyTimelineQueryOptions,
};
use crate::runtime::vision_event::{
    list_recent_local_vision_events_default, StoredLocalVisionEvent,
};

use super::vision_event_actions::latest_local_vision_event;
use super::{
    build_task_event_record, home_assistant_client_from_admin_state, json_string_at_paths_task,
    normalize_optional_general_message_plan_field, redact_task_api_text,
    redacted_home_assistant_entity_summary, resolve_home_assistant_action_entity,
    step_id_for_request, GeneralMessagePlan, GeneralMessagePlanKind, HomeAssistantEntityResolution,
    HomeAssistantNaturalAction, HomeAssistantNaturalActionRequest, TaskApiService, TaskRequest,
    TaskResponse,
};

impl TaskApiService {
    pub(super) fn handle_general_message_family_timeline(
        &self,
        request: &TaskRequest,
        plan: &GeneralMessagePlan,
    ) -> TaskResponse {
        let events = match list_recent_local_vision_events_default(50) {
            Ok(events) => events,
            Err(error) => {
                return self.failed(
                    request,
                    "agentic_interpreter",
                    RiskLevel::Low,
                    format!("读取家庭时间线失败：{}", redact_task_api_text(&error)),
                )
            }
        };
        let query = FamilyTimelineQueryOptions::default();
        let overlays = match family_memory_overlays_default() {
            Ok(overlays) => overlays,
            Err(error) => {
                return self.failed(
                    request,
                    "agentic_interpreter",
                    RiskLevel::Low,
                    format!(
                        "读取家庭记忆 overlay 失败：{}",
                        redact_task_api_text(&error)
                    ),
                )
            }
        };
        let visible_events = apply_family_memory_overlays_to_events(&events, &overlays, false);
        if let Some(latest_event_id) = visible_events
            .first()
            .map(|stored| stored.event.event_id.clone())
        {
            if let Some(stored) = visible_events
                .iter()
                .find(|stored| stored.event.event_id == latest_event_id)
            {
                let _ = self.remember_family_memory_event(request, stored);
            }
        }
        let digest = match build_family_timeline_digest_from_query(&visible_events, &query) {
            Ok(digest) => digest,
            Err(error) => {
                return self.failed(
                    request,
                    "agentic_interpreter",
                    RiskLevel::Low,
                    format!("生成家庭时间线摘要失败：{}", redact_task_api_text(&error)),
                )
            }
        };
        let mut message = digest.headline.clone();
        if !digest.bullets.is_empty() {
            message.push_str("\n");
            message.push_str(&digest.bullets.join("\n"));
        }
        if plan.kind == GeneralMessagePlanKind::FamilyMemorySummary {
            message.push_str(&format!(
                "\n家庭记忆 VLM 覆盖：active {}，degraded {}，未抽样 {}。",
                digest.vlm_coverage.active,
                digest.vlm_coverage.degraded,
                digest.vlm_coverage.not_sampled
            ));
        }
        let event = self.serialize_event_record(&build_task_event_record(
            request,
            &step_id_for_request(request),
            "family.timeline_digest_read",
            EventSeverity::Info,
            json!({
                "status": digest.status,
                "event_count": digest.event_count,
                "vlm_coverage": digest.vlm_coverage,
                "latest_event_id": digest.latest_event_id,
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
                    "kind": if plan.kind == GeneralMessagePlanKind::FamilyTimelineQuery {
                        "family_timeline_query"
                    } else if plan.kind == GeneralMessagePlanKind::FamilyMemorySummary {
                        "family_memory_summary"
                    } else {
                        "family_timeline_summary"
                    },
                    "summary": message,
                    "summary_source": if digest.vlm_coverage.active > 0 { "vlm_mixed" } else { "yolo" },
                    "query": plan.query,
                    "digest": digest,
                    "redacted": true,
                }
            }),
            Vec::new(),
            vec![event],
            vec![
                "家庭守护状态".to_string(),
                "以后门口有人就通知我".to_string(),
            ],
        )
    }

    pub(super) fn handle_general_message_family_memory_feedback(
        &self,
        request: &TaskRequest,
        plan: &GeneralMessagePlan,
    ) -> TaskResponse {
        if plan.kind == GeneralMessagePlanKind::FamilyMemoryShowFavorites {
            return self.handle_general_message_family_memory_favorites(request);
        }
        let event_id = plan
            .event_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| {
                self.recent_family_memory_event_for_request(request)
                    .map(|recent| recent.event_id)
            });
        let Some(event_id) = event_id else {
            let message = "我还不知道“这个”指哪条家庭事件。你可以先问“最近事件”或“今天家里发生了什么”，再回复收藏、隐藏或修正。".to_string();
            return self.completed_with_context(
                request,
                "family_memory_review",
                RiskLevel::Low,
                message.clone(),
                json!({
                    "reply_pack": {
                        "kind": "family_memory_feedback",
                        "status": "needs_input",
                        "reason": "no_recent_event_context",
                        "summary": message,
                        "redacted": true,
                    }
                }),
                Vec::new(),
                Vec::new(),
                vec!["最近事件".to_string(), "今天家里发生了什么".to_string()],
            );
        };
        let events = match list_recent_local_vision_events_default(100) {
            Ok(events) => events,
            Err(error) => {
                return self.failed(
                    request,
                    "family_memory_review",
                    RiskLevel::Low,
                    format!("读取家庭事件失败：{}", redact_task_api_text(&error)),
                )
            }
        };
        let Some(stored) = events
            .iter()
            .find(|stored| stored.event.event_id == event_id)
            .cloned()
        else {
            let message = "这条家庭事件已经不在最近事件窗口里了，不能直接纠错。".to_string();
            return self.completed_with_context(
                request,
                "family_memory_review",
                RiskLevel::Low,
                message.clone(),
                json!({
                    "reply_pack": {
                        "kind": "family_memory_feedback",
                        "status": "blocked",
                        "reason": "event_not_found",
                        "event_id": event_id,
                        "summary": message,
                        "redacted": true,
                    }
                }),
                Vec::new(),
                Vec::new(),
                vec!["最近事件".to_string()],
            );
        };
        let feedback_request = match family_memory_feedback_request_from_plan(plan) {
            Ok(request) => request,
            Err(error) => {
                return self.completed_with_context(
                    request,
                    "family_memory_review",
                    RiskLevel::Low,
                    error.clone(),
                    json!({
                        "reply_pack": {
                            "kind": "family_memory_feedback",
                            "status": "needs_input",
                            "reason": "feedback_slot_missing",
                            "summary": error,
                            "event_id": event_id,
                            "redacted": true,
                        }
                    }),
                    Vec::new(),
                    Vec::new(),
                    vec!["最近事件".to_string()],
                )
            }
        };
        let record =
            match append_family_memory_feedback_default(&event_id, feedback_request, "wechat") {
                Ok(record) => record,
                Err(error) => {
                    return self.failed(
                        request,
                        "family_memory_review",
                        RiskLevel::Low,
                        format!("保存家庭记忆反馈失败：{}", redact_task_api_text(&error)),
                    )
                }
            };
        let _ = self.remember_family_memory_event(request, &stored);
        let evidence = compact_family_memory_feedback_evidence(&record);
        let stats = list_family_memory_feedback_default(DEFAULT_FAMILY_MEMORY_FEEDBACK_LIMIT)
            .map(|records| build_family_memory_stats(&records))
            .ok();
        let message = family_memory_feedback_reply(record.action.as_str(), &stored);
        let event = self.serialize_event_record(&build_task_event_record(
            request,
            &step_id_for_request(request),
            "family_memory.feedback_recorded",
            EventSeverity::Info,
            json!({
                "status": "stored",
                "event_id": event_id,
                "camera_id": stored.event.camera_id,
                "feedback": evidence,
                "memory_overlay": stats,
                "metadata_only": true,
                "secret_scan": "clean",
                "redacted": true,
            }),
        ));
        self.completed_with_context(
            request,
            "family_memory_review",
            RiskLevel::Low,
            message.clone(),
            json!({
                "reply_pack": {
                    "kind": "family_memory_feedback",
                    "status": "stored",
                    "summary": message,
                    "event_id": event_id,
                    "camera_id": stored.event.camera_id,
                    "feedback": evidence,
                    "memory_overlay": stats,
                    "redacted": true,
                }
            }),
            Vec::new(),
            vec![event],
            vec![
                "看我收藏的家庭记忆".to_string(),
                "今天家里发生了什么".to_string(),
            ],
        )
    }

    fn handle_general_message_family_memory_favorites(
        &self,
        request: &TaskRequest,
    ) -> TaskResponse {
        let events = match list_recent_local_vision_events_default(100) {
            Ok(events) => events,
            Err(error) => {
                return self.failed(
                    request,
                    "family_memory_review",
                    RiskLevel::Low,
                    format!("读取家庭记忆失败：{}", redact_task_api_text(&error)),
                )
            }
        };
        let records =
            match list_family_memory_feedback_default(DEFAULT_FAMILY_MEMORY_FEEDBACK_LIMIT) {
                Ok(records) => records,
                Err(error) => {
                    return self.failed(
                        request,
                        "family_memory_review",
                        RiskLevel::Low,
                        format!(
                            "读取家庭记忆 overlay 失败：{}",
                            redact_task_api_text(&error)
                        ),
                    )
                }
            };
        let overlays = crate::runtime::family_memory::build_family_memory_overlays(&records);
        let visible_events = apply_family_memory_overlays_to_events(&events, &overlays, false);
        let favorites = visible_events
            .iter()
            .filter(|stored| {
                overlays
                    .get(&stored.event.event_id)
                    .is_some_and(|overlay| overlay.favorite)
            })
            .take(5)
            .collect::<Vec<_>>();
        let message = if favorites.is_empty() {
            "目前还没有收藏的家庭记忆。".to_string()
        } else {
            let lines = favorites
                .iter()
                .map(|stored| {
                    format!(
                        "{}：{}",
                        stored.event.camera_id,
                        redact_task_api_text(&stored.event.summary)
                    )
                })
                .collect::<Vec<_>>();
            format!("已收藏的家庭记忆：\n{}", lines.join("\n"))
        };
        if let Some(stored) = favorites.first() {
            let _ = self.remember_family_memory_event(request, stored);
        }
        let stats = build_family_memory_stats(&records);
        let event = self.serialize_event_record(&build_task_event_record(
            request,
            &step_id_for_request(request),
            "family_memory.favorites_read",
            EventSeverity::Info,
            json!({
                "favorite_count": stats.favorite_count,
                "returned_count": favorites.len(),
                "metadata_only": true,
                "secret_scan": "clean",
                "redacted": true,
            }),
        ));
        self.completed_with_context(
            request,
            "family_memory_review",
            RiskLevel::Low,
            message.clone(),
            json!({
                "reply_pack": {
                    "kind": "family_memory_show_favorites",
                    "status": "available",
                    "summary": message,
                    "memory_overlay": stats,
                    "redacted": true,
                }
            }),
            Vec::new(),
            vec![event],
            vec!["今天家里发生了什么".to_string()],
        )
    }

    pub(super) fn handle_general_message_guardian_rule_proposal(
        &self,
        request: &TaskRequest,
        plan: &GeneralMessagePlan,
    ) -> TaskResponse {
        let trigger = match build_guardian_trigger_from_plan(plan) {
            Some(trigger) => trigger,
            None => match latest_local_vision_event() {
                Ok(Some(stored)) => guardian_trigger_from_event(&stored),
                Ok(None) => {
                    return self.completed_with_context(
                        request,
                        "agentic_interpreter",
                        RiskLevel::Low,
                        "我还没有可参考的摄像头事件，先刷新 Camera 事件后再创建家庭守护规则。"
                            .to_string(),
                        json!({
                            "reply_pack": {
                                "kind": "guardian_rule_proposal",
                                "status": "needs_input",
                                "reason": "no_recent_event",
                                "redacted": true,
                            }
                        }),
                        Vec::new(),
                        Vec::new(),
                        vec!["最近事件".to_string()],
                    )
                }
                Err(error) => {
                    return self.failed(
                        request,
                        "agentic_interpreter",
                        RiskLevel::Low,
                        format!("读取最近事件失败：{}", redact_task_api_text(&error)),
                    )
                }
            },
        };
        let action_plan = match self.build_guardian_action_plan_from_plan(request, plan) {
            Ok(action_plan) => action_plan,
            Err(response) => return response,
        };
        let review = AutomationRuleReview {
            review_id: format!("guardian_review_{}", Uuid::new_v4().simple()),
            workspace_id: "home-1".to_string(),
            source: "home_guardian".to_string(),
            source_channel: Some(request.source.channel.clone()),
            source_conversation_id: Some(request.source.conversation_id.clone()),
            original_prompt: redact_task_api_text(&request.intent.raw_text),
            status: "pending".to_string(),
            trigger_definition: Some(trigger.clone()),
            condition_definition: None,
            action_plan: Some(action_plan.clone()),
            device_refs: Vec::new(),
            risk_level: Some("low".to_string()),
            requires_approval: true,
            created_at: None,
            updated_at: None,
            expires_at: None,
            rule_id: Some(format!("guardian_rule_{}", Uuid::new_v4().simple())),
            run_summaries: Vec::new(),
            metadata: Some(json!({
                "feature": "home_guardian",
                "home_guardian": true,
                "created_from": "wechat_nsp_first",
                "requires_explicit_enable": true,
                "metadata_only": true,
            })),
        };
        let review_id = review.review_id.clone();
        let rule_id = review.rule_id.clone();
        if let Err(error) = self.admin_store.upsert_automation_review(review) {
            return self.failed(
                request,
                "agentic_interpreter",
                RiskLevel::Low,
                format!("创建家庭守护规则草稿失败：{}", redact_task_api_text(&error)),
            );
        }
        let message =
            "已生成家庭守护规则草稿。回复“启用”后才会自动执行；回复“取消”会丢弃这个草稿。"
                .to_string();
        let event = self.serialize_event_record(&build_task_event_record(
            request,
            &step_id_for_request(request),
            "guardian.rule_proposed",
            EventSeverity::Info,
            json!({
                "status": "needs_input",
                "review_id": review_id,
                "rule_id": rule_id,
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
                    "kind": "guardian_rule_proposal",
                    "status": "needs_input",
                    "summary": message,
                    "review_id": review_id,
                    "rule_id": rule_id,
                    "trigger": trigger,
                    "action_plan": redacted_guardian_action_plan(&action_plan),
                    "redacted": true,
                }
            }),
            Vec::new(),
            vec![event],
            vec!["启用".to_string(), "取消".to_string()],
        )
    }

    pub(super) fn handle_general_message_guardian_status(
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
                    format!("读取家庭守护状态失败：{}", redact_task_api_text(&error)),
                )
            }
        };
        let rules = state
            .automation_reviews
            .iter()
            .filter(|review| automation_review_is_home_guardian_task(review))
            .map(redacted_guardian_review_summary)
            .collect::<Vec<_>>();
        let active_count = rules
            .iter()
            .filter(|rule| rule["status"] == json!("active"))
            .count();
        let message = format!(
            "家庭守护：{} 条规则，{} 条已启用。只有启用后的低风险通知/HA 动作会自动执行。",
            rules.len(),
            active_count
        );
        let event = self.serialize_event_record(&build_task_event_record(
            request,
            &step_id_for_request(request),
            "guardian.status_read",
            EventSeverity::Info,
            json!({
                "rule_count": rules.len(),
                "active_count": active_count,
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
                    "kind": "guardian_status",
                    "summary": message,
                    "rules": rules,
                    "redacted": true,
                }
            }),
            Vec::new(),
            vec![event],
            vec![
                "今天家里发生了什么".to_string(),
                "以后门口有人就通知我".to_string(),
            ],
        )
    }

    pub(super) fn handle_general_message_guardian_status_update(
        &self,
        request: &TaskRequest,
        desired_status: &str,
    ) -> TaskResponse {
        let state = match self.admin_store.load_or_create_state() {
            Ok(state) => state,
            Err(error) => {
                return self.failed(
                    request,
                    "agentic_interpreter",
                    RiskLevel::Low,
                    format!("读取家庭守护规则失败：{}", redact_task_api_text(&error)),
                )
            }
        };
        let Some(review) = latest_guardian_review_for_conversation(
            &state.automation_reviews,
            request.source.conversation_id.as_str(),
            desired_status,
        ) else {
            return self.completed_with_context(
                request,
                "agentic_interpreter",
                RiskLevel::Low,
                "没有找到可更新的家庭守护规则。".to_string(),
                json!({
                    "reply_pack": {
                        "kind": "guardian_status_update",
                        "status": "blocked",
                        "reason": "no_matching_rule",
                        "redacted": true,
                    }
                }),
                Vec::new(),
                Vec::new(),
                vec!["家庭守护状态".to_string()],
            );
        };
        let target_status = if desired_status == "paused"
            && matches!(review.status.as_str(), "draft" | "pending")
        {
            "discarded"
        } else {
            desired_status
        };
        let review_id = review.review_id.clone();
        let response_state = match self
            .admin_store
            .set_automation_review_status(&review_id, target_status)
        {
            Ok(state) => state,
            Err(error) => {
                return self.failed(
                    request,
                    "agentic_interpreter",
                    RiskLevel::Low,
                    format!("更新家庭守护规则失败：{}", redact_task_api_text(&error)),
                )
            }
        };
        let updated = response_state
            .automation_reviews
            .iter()
            .find(|item| item.review_id == review_id)
            .map(redacted_guardian_review_summary)
            .unwrap_or_else(|| json!({"review_id": review_id, "status": target_status}));
        let message = match target_status {
            "active" => "已启用这条家庭守护规则；之后匹配事件才会触发低风险动作。",
            "discarded" => "已取消这个家庭守护规则草稿。",
            "paused" => "已暂停这条家庭守护规则。",
            _ => "已更新家庭守护规则。",
        }
        .to_string();
        let event = self.serialize_event_record(&build_task_event_record(
            request,
            &step_id_for_request(request),
            "guardian.rule_status_updated",
            EventSeverity::Info,
            json!({
                "review_id": review_id,
                "status": target_status,
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
                    "kind": "guardian_status_update",
                    "status": target_status,
                    "summary": message,
                    "rule": updated,
                    "redacted": true,
                }
            }),
            Vec::new(),
            vec![event],
            vec!["家庭守护状态".to_string()],
        )
    }

    pub(super) fn build_guardian_action_plan_from_plan(
        &self,
        request: &TaskRequest,
        plan: &GeneralMessagePlan,
    ) -> Result<Value, TaskResponse> {
        if let Some(action_plan) = plan
            .guardian_rule
            .as_ref()
            .and_then(|rule| rule.pointer("/action_plan").cloned())
        {
            return self.resolve_guardian_action_plan_entities(request, action_plan, plan);
        }
        if let Some(action) = plan.home_assistant_action.as_ref() {
            let action_plan = self.build_guardian_ha_action_plan(request, action)?;
            return Ok(action_plan);
        }
        Ok(json!({
            "actions": [{
                "kind": "notify_default_target",
                "target": "default_notification_target",
            }],
            "metadata_only": true,
        }))
    }

    pub(super) fn resolve_guardian_action_plan_entities(
        &self,
        request: &TaskRequest,
        mut action_plan: Value,
        plan: &GeneralMessagePlan,
    ) -> Result<Value, TaskResponse> {
        let Some(actions) = action_plan
            .pointer_mut("/actions")
            .and_then(Value::as_array_mut)
        else {
            return Ok(action_plan);
        };
        for action in actions {
            if !guardian_action_is_ha(action) || action.pointer("/entity_id").is_some() {
                continue;
            }
            let domain = json_string_at_paths_task(action, &["/domain", "/home_assistant/domain"])
                .or_else(|| {
                    plan.home_assistant_action
                        .as_ref()
                        .and_then(|action| match action {
                            HomeAssistantNaturalAction::Request(request) => {
                                Some(request.domain.clone())
                            }
                            HomeAssistantNaturalAction::Blocked { .. } => None,
                        })
                })
                .unwrap_or_default();
            let service =
                json_string_at_paths_task(action, &["/service", "/home_assistant/service"])
                    .or_else(|| {
                        plan.home_assistant_action
                            .as_ref()
                            .and_then(|action| match action {
                                HomeAssistantNaturalAction::Request(request) => {
                                    Some(request.service.clone())
                                }
                                HomeAssistantNaturalAction::Blocked { .. } => None,
                            })
                    })
                    .unwrap_or_default();
            let entity_hint =
                json_string_at_paths_task(action, &["/entity_hint", "/home_assistant/entity_hint"])
                    .or_else(|| {
                        plan.home_assistant_action
                            .as_ref()
                            .and_then(|action| match action {
                                HomeAssistantNaturalAction::Request(request) => {
                                    request.entity_hint.clone()
                                }
                                HomeAssistantNaturalAction::Blocked { .. } => None,
                            })
                    });
            let natural = HomeAssistantNaturalAction::Request(HomeAssistantNaturalActionRequest {
                domain,
                service,
                entity_hint,
            });
            let resolved = self.build_guardian_ha_action_plan(request, &natural)?;
            let resolved_action = resolved
                .pointer("/actions/0")
                .cloned()
                .unwrap_or_else(|| json!({}));
            *action = resolved_action;
        }
        Ok(action_plan)
    }

    pub(super) fn build_guardian_ha_action_plan(
        &self,
        request: &TaskRequest,
        action: &HomeAssistantNaturalAction,
    ) -> Result<Value, TaskResponse> {
        let HomeAssistantNaturalAction::Request(action) = action else {
            return Err(self.completed_with_context(
                request,
                "agentic_interpreter",
                RiskLevel::Low,
                "这个 Home Assistant 动作不在家庭守护低风险范围内，没有创建规则。".to_string(),
                json!({
                    "reply_pack": {
                        "kind": "guardian_rule_proposal",
                        "status": "blocked",
                        "reason": "ha_action_blocked",
                        "redacted": true,
                    }
                }),
                Vec::new(),
                Vec::new(),
                vec!["家庭守护状态".to_string()],
            ));
        };
        let state = match self.admin_store.home_assistant_secret_state() {
            Ok(state) => state,
            Err(error) => {
                return Err(self.failed(
                    request,
                    "agentic_interpreter",
                    RiskLevel::Low,
                    format!(
                        "读取 Home Assistant 配置失败：{}",
                        redact_task_api_text(&error)
                    ),
                ))
            }
        };
        let client = match home_assistant_client_from_admin_state(&state) {
            Ok(client) => client,
            Err(error) => {
                return Err(self.completed_with_context(
                    request,
                    "agentic_interpreter",
                    RiskLevel::Low,
                    redact_task_api_text(&error),
                    json!({
                        "reply_pack": {
                            "kind": "guardian_rule_proposal",
                            "status": "blocked",
                            "reason": "home_assistant_unavailable",
                            "redacted": true,
                        }
                    }),
                    Vec::new(),
                    Vec::new(),
                    vec!["家庭守护状态".to_string()],
                ))
            }
        };
        let entities = match client.fetch_entities() {
            Ok(entities) => entities,
            Err(error) => {
                return Err(self.failed(
                    request,
                    "agentic_interpreter",
                    RiskLevel::Low,
                    format!(
                        "读取 Home Assistant 实体失败：{}",
                        redact_task_api_text(&error)
                    ),
                ))
            }
        };
        let entity = match resolve_home_assistant_action_entity(action, &entities) {
            HomeAssistantEntityResolution::Resolved(entity) => entity,
            HomeAssistantEntityResolution::Clarify(candidates) => {
                let candidate_summaries = candidates
                    .iter()
                    .take(5)
                    .map(redacted_home_assistant_entity_summary)
                    .collect::<Vec<_>>();
                return Err(self.completed_with_context(
                    request,
                    "agentic_interpreter",
                    RiskLevel::Low,
                    "这个 Home Assistant 动作匹配到多个实体；请先说清楚要绑定哪一个，我不会猜测创建自动规则。"
                        .to_string(),
                    json!({
                        "reply_pack": {
                            "kind": "guardian_rule_proposal",
                            "status": "needs_input",
                            "reason": "ambiguous_ha_entity",
                            "candidates": candidate_summaries,
                            "redacted": true,
                        }
                    }),
                    Vec::new(),
                    Vec::new(),
                    vec!["家庭守护状态".to_string()],
                ));
            }
            HomeAssistantEntityResolution::Missing(message) => {
                return Err(self.completed_with_context(
                    request,
                    "agentic_interpreter",
                    RiskLevel::Low,
                    message,
                    json!({
                        "reply_pack": {
                            "kind": "guardian_rule_proposal",
                            "status": "blocked",
                            "reason": "missing_ha_entity",
                            "redacted": true,
                        }
                    }),
                    Vec::new(),
                    Vec::new(),
                    vec!["家庭守护状态".to_string()],
                ))
            }
        };
        Ok(json!({
            "actions": [{
                "kind": "ha_service_action",
                "domain": action.domain,
                "service": action.service,
                "entity_id": entity.entity_id,
                "fields": {},
            }],
            "metadata_only": true,
        }))
    }
}

fn family_memory_feedback_request_from_plan(
    plan: &GeneralMessagePlan,
) -> Result<FamilyMemoryFeedbackRequest, String> {
    match plan.kind {
        GeneralMessagePlanKind::FamilyMemoryConfirm => Ok(FamilyMemoryFeedbackRequest {
            action: "confirm_useful".to_string(),
            ..FamilyMemoryFeedbackRequest::default()
        }),
        GeneralMessagePlanKind::FamilyMemoryFavorite => Ok(FamilyMemoryFeedbackRequest {
            action: "favorite".to_string(),
            ..FamilyMemoryFeedbackRequest::default()
        }),
        GeneralMessagePlanKind::FamilyMemoryHide => Ok(FamilyMemoryFeedbackRequest {
            action: "hide".to_string(),
            ..FamilyMemoryFeedbackRequest::default()
        }),
        GeneralMessagePlanKind::FamilyMemoryCorrectSummary => {
            let summary = plan
                .corrected_summary
                .as_deref()
                .or(plan.query.as_deref())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "你要把这条事件改成什么摘要？".to_string())?;
            Ok(FamilyMemoryFeedbackRequest {
                action: "correct_summary".to_string(),
                corrected_summary: Some(summary.to_string()),
                ..FamilyMemoryFeedbackRequest::default()
            })
        }
        GeneralMessagePlanKind::FamilyMemoryCorrectLabels => {
            let labels = plan
                .corrected_labels
                .clone()
                .filter(|labels| !labels.is_empty())
                .ok_or_else(|| "你要把这条事件改成哪些标签？".to_string())?;
            Ok(FamilyMemoryFeedbackRequest {
                action: "correct_labels".to_string(),
                corrected_labels: Some(labels),
                ..FamilyMemoryFeedbackRequest::default()
            })
        }
        _ => Err("这类家庭记忆反馈还不支持。".to_string()),
    }
}

fn family_memory_feedback_reply(action: &str, stored: &StoredLocalVisionEvent) -> String {
    let camera = stored.event.camera_id.as_str();
    match action {
        "confirm_useful" => format!("已确认这条 {camera} 事件有用。"),
        "favorite" => format!("已收藏这条 {camera} 家庭记忆。"),
        "hide" => format!("已隐藏这条 {camera} 事件；原始事件审计不会被修改。"),
        "correct_summary" => format!("已修正这条 {camera} 事件摘要。"),
        "correct_labels" => format!("已修正这条 {camera} 事件标签。"),
        _ => format!("已记录这条 {camera} 事件的家庭记忆反馈。"),
    }
}

pub(super) fn build_guardian_trigger_from_plan(plan: &GeneralMessagePlan) -> Option<Value> {
    let rule = plan.guardian_rule.as_ref()?;
    rule.pointer("/trigger").cloned().or_else(|| {
        Some(json!({
            "camera_id": normalize_optional_general_message_plan_field(plan.camera_hint.clone()),
            "event_type": json_string_at_paths_task(rule, &["/event_type"]).unwrap_or_else(|| "motion_like_scene".to_string()),
            "labels": rule.pointer("/labels").cloned().unwrap_or_else(|| json!([])),
            "min_confidence": rule
                .pointer("/min_confidence")
                .cloned()
                .unwrap_or_else(|| json!(0.5)),
            "local_time_window": rule
                .pointer("/local_time_window")
                .cloned()
                .unwrap_or_else(|| json!({"start": "00:00", "end": "23:59"})),
        }))
    })
}

pub(super) fn guardian_trigger_from_event(stored: &StoredLocalVisionEvent) -> Value {
    json!({
        "camera_id": stored.event.camera_id,
        "event_type": stored.event.event_type,
        "labels": stored.event.labels,
        "min_confidence": (stored.event.confidence - 0.05).max(0.5),
        "local_time_window": {
            "start": "00:00",
            "end": "23:59",
        },
        "source_event_id": stored.event.event_id,
        "metadata_only": true,
    })
}

pub(super) fn guardian_action_is_ha(action: &Value) -> bool {
    json_string_at_paths_task(action, &["/kind", "/action", "/type"])
        .map(|kind| {
            matches!(
                kind.trim().to_ascii_lowercase().as_str(),
                "ha_service_action" | "home_assistant_service_action" | "home_assistant"
            )
        })
        .unwrap_or(false)
}

pub(super) fn redacted_guardian_action_plan(action_plan: &Value) -> Value {
    let actions = action_plan
        .pointer("/actions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| {
            if action_plan.as_object().is_some() {
                vec![action_plan.clone()]
            } else {
                Vec::new()
            }
        });
    json!({
        "actions": actions
            .iter()
            .map(|action| {
                if guardian_action_is_ha(action) {
                    json!({
                        "kind": "ha_service_action",
                        "domain": json_string_at_paths_task(action, &["/domain", "/home_assistant/domain"]),
                        "service": json_string_at_paths_task(action, &["/service", "/home_assistant/service"]),
                        "entity_id": json_string_at_paths_task(action, &["/entity_id", "/home_assistant/entity_id"]),
                        "fields_empty": true,
                    })
                } else {
                    json!({
                        "kind": "notify_default_target",
                        "target": "default_notification_target",
                    })
                }
            })
            .collect::<Vec<_>>(),
        "metadata_only": true,
        "secret_scan": "clean",
    })
}

pub(super) fn automation_review_is_home_guardian_task(review: &AutomationRuleReview) -> bool {
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

pub(super) fn redacted_guardian_review_summary(review: &AutomationRuleReview) -> Value {
    json!({
        "review_id": review.review_id,
        "rule_id": review.rule_id,
        "status": review.status,
        "trigger": review.trigger_definition,
        "action_plan": review
            .action_plan
            .as_ref()
            .map(redacted_guardian_action_plan)
            .unwrap_or_else(|| json!({"actions": [], "metadata_only": true})),
        "run_count": review.run_summaries.len(),
        "last_run": review.run_summaries.last(),
        "metadata_only": true,
        "secret_scan": "clean",
    })
}

pub(super) fn latest_guardian_review_for_conversation(
    reviews: &[AutomationRuleReview],
    conversation_id: &str,
    desired_status: &str,
) -> Option<AutomationRuleReview> {
    let mut candidates = reviews
        .iter()
        .filter(|review| automation_review_is_home_guardian_task(review))
        .filter(|review| {
            review
                .source_conversation_id
                .as_deref()
                .map(|value| value == conversation_id)
                .unwrap_or(true)
        })
        .filter(|review| {
            if desired_status == "active" {
                matches!(review.status.as_str(), "draft" | "pending")
            } else {
                matches!(review.status.as_str(), "draft" | "pending" | "active")
            }
        })
        .cloned()
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| right.created_at.cmp(&left.created_at))
            .then_with(|| right.review_id.cmp(&left.review_id))
    });
    candidates.into_iter().next()
}
