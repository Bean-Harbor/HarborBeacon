//! Pure routing helpers for general-message Task API turns.

use serde_json::{json, Value};

use crate::runtime::evt_readiness::evt_long_run_request_boundary;
use crate::runtime::task_session::{PendingTaskGeneralMessageLoop, RecentClipPlaybackState};

use super::{
    infer_home_assistant_natural_action, infer_query_from_raw_text,
    knowledge_search_contextual_follow_up, looks_like_rag_answer_request,
    looks_like_system_readiness_request, looks_like_video_search_request,
    looks_like_vision_event_notify_request, looks_like_vision_event_summary_request, matches_any,
    normalize_command_text, normalize_optional_general_message_plan_field,
    parse_json_object_from_text, recent_clip_playback_request_from_normalized,
    recent_search_query_from_recap, GeneralMessageCandidate, GeneralMessageControllerTrace,
    GeneralMessageConversationAct, GeneralMessagePlan, GeneralMessagePlanKind,
    GeneralMessagePlanPayload, GeneralMessageSignals, HomeAssistantNaturalAction,
    HomeAssistantNaturalActionRequest, TaskRequest, GENERAL_MESSAGE_NSP_MIN_CONFIDENCE,
    GENERAL_MESSAGE_RECAP_LIMIT, KNOWLEDGE_DOMAIN, KNOWLEDGE_OP_SEARCH,
};
pub(super) fn extract_general_message_signals(
    request: &TaskRequest,
    session_recap: &[Value],
    pending_loop: Option<&PendingTaskGeneralMessageLoop>,
    recent_clip: Option<&RecentClipPlaybackState>,
) -> GeneralMessageSignals {
    let normalized = normalize_command_text(request.intent.raw_text.as_str());
    let recent_camera_context = session_recap.iter().any(|entry| {
        entry
            .get("domain")
            .and_then(Value::as_str)
            .is_some_and(|value| value.eq_ignore_ascii_case("camera"))
    });
    let recent_search_context = session_recap.iter().any(|entry| {
        entry
            .get("domain")
            .and_then(Value::as_str)
            .is_some_and(|value| value.eq_ignore_ascii_case(KNOWLEDGE_DOMAIN))
            || entry
                .get("data_kind")
                .and_then(Value::as_str)
                .is_some_and(|value| {
                    matches!(value, "knowledge.search" | "rag.answer")
                        || value.eq_ignore_ascii_case(KNOWLEDGE_OP_SEARCH)
                })
            || entry.get("query").and_then(Value::as_str).is_some()
    });
    let explicit_snapshot = matches_any(
        &normalized,
        &["抓拍", "拍照", "拍一张", "来一张", "快照", "截图", "截一张"],
    );
    let explicit_video_search = looks_like_video_search_request(&normalized);
    let explicit_clip = !explicit_video_search
        && matches_any(
            &normalized,
            &[
                "录一段",
                "录一下",
                "录个",
                "录像",
                "录视频",
                "拍视频",
                "短视频",
            ],
        );
    let explicit_search = explicit_video_search
        || matches_any(
            &normalized,
            &[
                "找一下",
                "找到",
                "查一下",
                "查找",
                "搜索",
                "搜一下",
                "搜搜",
                "检索",
                "找视频",
                "找录像",
                "找片段",
                "找回放",
                "查录像",
                "搜索录像",
                "找照片",
                "找图片",
                "只看视频",
                "只看视频结果",
                "排除图片和文档",
                "搜索已有内容",
            ],
        );
    let recent_clip_available = recent_clip.is_some();
    let explicit_clip_playback =
        recent_clip_available && recent_clip_playback_request_from_normalized(&normalized);
    let asks_capability =
        general_message_requests_capability_summary(request.intent.raw_text.as_str());
    let explicit_rag_answer = looks_like_rag_answer_request(&normalized) && !asks_capability;
    let explicit_event_notify = looks_like_vision_event_notify_request(&normalized);
    let explicit_event_summary =
        !explicit_event_notify && looks_like_vision_event_summary_request(&normalized);
    let explicit_system_readiness = looks_like_system_readiness_request(&normalized);
    let explicit_ha_action =
        infer_home_assistant_natural_action(request.intent.raw_text.as_str()).is_some();
    let mentions_camera_context = matches_any(
        &normalized,
        &[
            "摄像头",
            "监控",
            "画面",
            "门口",
            "客厅",
            "卧室",
            "车库",
            "院子",
            "阳台",
        ],
    ) || pending_loop
        .and_then(|pending| pending.camera_hint.as_ref())
        .is_some();
    let ambiguous_visual_request = !asks_capability
        && !explicit_snapshot
        && !explicit_clip
        && !explicit_search
        && (matches_any(
            &normalized,
            &["看一下", "看一眼", "看下", "看看", "瞅一眼", "瞅瞅"],
        ) || (mentions_camera_context && matches_any(&normalized, &["看", "瞅"])));

    GeneralMessageSignals {
        normalized,
        asks_capability,
        explicit_clip_playback,
        explicit_snapshot,
        explicit_clip,
        explicit_search,
        explicit_rag_answer,
        explicit_ha_action,
        explicit_event_summary,
        explicit_event_notify,
        explicit_system_readiness,
        mentions_camera_context,
        ambiguous_visual_request,
        recent_camera_context,
        recent_clip_available,
        recent_search_context,
    }
}

pub(super) fn build_general_message_candidates(
    request: &TaskRequest,
    signals: &GeneralMessageSignals,
    default_camera_hint: Option<&str>,
    pending_loop: Option<&PendingTaskGeneralMessageLoop>,
    session_recap: &[Value],
    recent_clip: Option<&RecentClipPlaybackState>,
) -> Vec<GeneralMessageCandidate> {
    let mut candidates = Vec::new();
    let camera_hint = pending_loop
        .and_then(|pending| pending.camera_hint.clone())
        .or_else(|| {
            infer_camera_hint_from_general_message(
                request.intent.raw_text.as_str(),
                default_camera_hint,
            )
        });
    let recent_query = recent_search_query_from_recap(session_recap);
    let contextual_search_follow_up = knowledge_search_contextual_follow_up(&signals.normalized);
    let query = pending_loop
        .and_then(|pending| pending.query.clone())
        .or_else(|| {
            contextual_search_follow_up
                .then(|| recent_query.clone())
                .flatten()
        })
        .or_else(|| infer_query_from_raw_text(request.intent.raw_text.as_str()));

    if signals.asks_capability {
        push_general_message_candidate(
            &mut candidates,
            GeneralMessageCandidate {
                kind: GeneralMessagePlanKind::CapabilitySummary,
                confidence: 100,
                camera_hint: None,
                query: None,
                recent_clip: None,
                reason: "structured_signal_capability_summary".to_string(),
            },
        );
    }
    if signals.explicit_clip_playback {
        push_general_message_candidate(
            &mut candidates,
            GeneralMessageCandidate {
                kind: GeneralMessagePlanKind::CameraReplayRecentClip,
                confidence: 98,
                camera_hint: None,
                query: None,
                recent_clip: recent_clip.cloned(),
                reason: "structured_signal_recent_clip_playback".to_string(),
            },
        );
    }
    if signals.explicit_event_notify {
        push_general_message_candidate(
            &mut candidates,
            GeneralMessageCandidate {
                kind: GeneralMessagePlanKind::VisionEventNotifyLatest,
                confidence: 99,
                camera_hint: None,
                query: None,
                recent_clip: None,
                reason: "structured_signal_vision_event_notify".to_string(),
            },
        );
    }
    if signals.explicit_event_summary {
        push_general_message_candidate(
            &mut candidates,
            GeneralMessageCandidate {
                kind: GeneralMessagePlanKind::VisionEventSummary,
                confidence: 98,
                camera_hint: None,
                query: None,
                recent_clip: None,
                reason: "structured_signal_vision_event_summary".to_string(),
            },
        );
    }
    if signals.explicit_system_readiness {
        push_general_message_candidate(
            &mut candidates,
            GeneralMessageCandidate {
                kind: GeneralMessagePlanKind::SystemReadiness,
                confidence: 98,
                camera_hint: None,
                query: None,
                recent_clip: None,
                reason: "structured_signal_system_readiness".to_string(),
            },
        );
    }
    if signals.explicit_ha_action {
        push_general_message_candidate(
            &mut candidates,
            GeneralMessageCandidate {
                kind: GeneralMessagePlanKind::HomeAssistantServiceAction,
                confidence: 98,
                camera_hint: None,
                query: None,
                recent_clip: None,
                reason: "structured_signal_home_assistant_service_action".to_string(),
            },
        );
    }
    if signals.explicit_snapshot {
        push_general_message_candidate(
            &mut candidates,
            GeneralMessageCandidate {
                kind: GeneralMessagePlanKind::CameraSnapshot,
                confidence: 95,
                camera_hint: camera_hint.clone(),
                query: None,
                recent_clip: None,
                reason: "structured_signal_snapshot".to_string(),
            },
        );
    }
    if signals.explicit_clip {
        push_general_message_candidate(
            &mut candidates,
            GeneralMessageCandidate {
                kind: GeneralMessagePlanKind::CameraRecordClip,
                confidence: 95,
                camera_hint: camera_hint.clone(),
                query: None,
                recent_clip: None,
                reason: "structured_signal_clip".to_string(),
            },
        );
    }
    if signals.explicit_search && !signals.explicit_rag_answer {
        push_general_message_candidate(
            &mut candidates,
            GeneralMessageCandidate {
                kind: GeneralMessagePlanKind::KnowledgeSearch,
                confidence: 95,
                camera_hint: None,
                query: query.clone(),
                recent_clip: None,
                reason: "structured_signal_search".to_string(),
            },
        );
    }
    if !signals.explicit_search && signals.recent_search_context && contextual_search_follow_up {
        push_general_message_candidate(
            &mut candidates,
            GeneralMessageCandidate {
                kind: GeneralMessagePlanKind::KnowledgeSearch,
                confidence: 90,
                camera_hint: None,
                query: query.clone(),
                recent_clip: None,
                reason: "recent_search_context_filter".to_string(),
            },
        );
    }
    if signals.explicit_rag_answer {
        push_general_message_candidate(
            &mut candidates,
            GeneralMessageCandidate {
                kind: GeneralMessagePlanKind::RagAnswer,
                confidence: 95,
                camera_hint: None,
                query: query.clone(),
                recent_clip: None,
                reason: "structured_signal_rag_answer".to_string(),
            },
        );
    }

    if signals.ambiguous_visual_request {
        push_general_message_candidate(
            &mut candidates,
            GeneralMessageCandidate {
                kind: GeneralMessagePlanKind::Clarify,
                confidence: 90,
                camera_hint: camera_hint.clone(),
                query: query.clone(),
                recent_clip: None,
                reason: "ambiguous_visual_request".to_string(),
            },
        );
        push_general_message_candidate(
            &mut candidates,
            GeneralMessageCandidate {
                kind: GeneralMessagePlanKind::CameraSnapshot,
                confidence: 55,
                camera_hint: camera_hint.clone(),
                query: None,
                recent_clip: None,
                reason: "plausible_visual_snapshot".to_string(),
            },
        );
        push_general_message_candidate(
            &mut candidates,
            GeneralMessageCandidate {
                kind: GeneralMessagePlanKind::CameraRecordClip,
                confidence: 55,
                camera_hint,
                query: None,
                recent_clip: None,
                reason: "plausible_visual_clip".to_string(),
            },
        );
    }

    if !signals.explicit_snapshot
        && !signals.explicit_clip
        && signals.recent_camera_context
        && matches_any(&signals.normalized, &["再来一张", "再拍一张", "再看一眼"])
    {
        push_general_message_candidate(
            &mut candidates,
            GeneralMessageCandidate {
                kind: GeneralMessagePlanKind::CameraSnapshot,
                confidence: 85,
                camera_hint: pending_loop
                    .and_then(|pending| pending.camera_hint.clone())
                    .or_else(|| default_camera_hint.map(str::to_string)),
                query: None,
                recent_clip: None,
                reason: "recent_camera_context_snapshot".to_string(),
            },
        );
    }

    if !signals.explicit_clip
        && signals.recent_camera_context
        && matches_any(&signals.normalized, &["再来一段", "再录一段", "再录一下"])
    {
        push_general_message_candidate(
            &mut candidates,
            GeneralMessageCandidate {
                kind: GeneralMessagePlanKind::CameraRecordClip,
                confidence: 85,
                camera_hint: pending_loop
                    .and_then(|pending| pending.camera_hint.clone())
                    .or_else(|| default_camera_hint.map(str::to_string)),
                query: None,
                recent_clip: None,
                reason: "recent_camera_context_clip".to_string(),
            },
        );
    }

    if !signals.explicit_clip_playback
        && signals.recent_clip_available
        && matches_any(&signals.normalized, &["再放一下", "再回放一下", "再播一下"])
    {
        push_general_message_candidate(
            &mut candidates,
            GeneralMessageCandidate {
                kind: GeneralMessagePlanKind::CameraReplayRecentClip,
                confidence: 85,
                camera_hint: None,
                query: None,
                recent_clip: recent_clip.cloned(),
                reason: "recent_clip_context_playback".to_string(),
            },
        );
    }

    if !signals.explicit_search
        && signals.recent_search_context
        && matches_any(
            &signals.normalized,
            &["再搜一下", "再查一下", "再找找", "搜已有内容"],
        )
    {
        push_general_message_candidate(
            &mut candidates,
            GeneralMessageCandidate {
                kind: GeneralMessagePlanKind::KnowledgeSearch,
                confidence: 85,
                camera_hint: None,
                query,
                recent_clip: None,
                reason: "recent_search_context_search".to_string(),
            },
        );
    }

    candidates
}

pub(super) fn push_general_message_candidate(
    candidates: &mut Vec<GeneralMessageCandidate>,
    candidate: GeneralMessageCandidate,
) {
    if let Some(existing) = candidates
        .iter_mut()
        .find(|item| item.kind == candidate.kind)
    {
        if candidate.confidence > existing.confidence {
            *existing = candidate;
            return;
        }
        if existing.camera_hint.is_none() {
            existing.camera_hint = candidate.camera_hint;
        }
        if existing.query.is_none() {
            existing.query = candidate.query;
        }
        if existing.recent_clip.is_none() {
            existing.recent_clip = candidate.recent_clip;
        }
        if existing.reason.trim().is_empty() {
            existing.reason = candidate.reason;
        }
        return;
    }
    candidates.push(candidate);
}

pub(super) fn infer_camera_hint_from_general_message(
    raw_text: &str,
    default_camera_hint: Option<&str>,
) -> Option<String> {
    if let Some(default_camera_hint) = default_camera_hint
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(default_camera_hint.to_string());
    }

    let normalized = normalize_command_text(raw_text);
    [
        ("front-door", &["门口", "门前", "前门", "玄关"][..]),
        ("living-room", &["客厅"][..]),
        ("bedroom", &["卧室"][..]),
        ("garage", &["车库"][..]),
        ("yard", &["院子", "院门"][..]),
        ("balcony", &["阳台"][..]),
    ]
    .into_iter()
    .find_map(|(hint, tokens)| {
        tokens
            .iter()
            .any(|token| normalized.contains(&normalize_command_text(token)))
            .then(|| hint.to_string())
    })
}

pub(super) fn resolve_deterministic_general_message_plan(
    request: &TaskRequest,
    candidates: &[GeneralMessageCandidate],
    pending_loop: Option<&PendingTaskGeneralMessageLoop>,
) -> Option<GeneralMessagePlan> {
    if candidates.is_empty() {
        return None;
    }

    let mut actionable = candidates
        .iter()
        .filter(|candidate| {
            !matches!(
                candidate.kind,
                GeneralMessagePlanKind::Clarify
                    | GeneralMessagePlanKind::ConversationAct
                    | GeneralMessagePlanKind::Unsupported
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    actionable.sort_by(|left, right| {
        right
            .confidence
            .cmp(&left.confidence)
            .then_with(|| left.reason.cmp(&right.reason))
    });

    if let Some(primary) = actionable.first() {
        let competing = actionable
            .iter()
            .skip(1)
            .filter(|candidate| candidate.confidence + 15 >= primary.confidence)
            .count();
        if primary.confidence >= 90 && competing == 0 {
            return Some(plan_from_general_message_candidate(primary));
        }
    }

    if let Some(clarify) = candidates
        .iter()
        .filter(|candidate| candidate.kind == GeneralMessagePlanKind::Clarify)
        .max_by_key(|candidate| candidate.confidence)
    {
        let plausible_actions = actionable
            .iter()
            .filter(|candidate| candidate.confidence >= 50)
            .count();
        if clarify.confidence >= 80 || plausible_actions >= 2 {
            return Some(plan_from_general_message_candidate(clarify));
        }
    }

    if let Some(primary) = actionable.first() {
        let runner_up = actionable
            .get(1)
            .map(|candidate| candidate.confidence)
            .unwrap_or(0);
        if primary.confidence >= 80 && primary.confidence >= runner_up + 20 {
            return Some(plan_from_general_message_candidate(primary));
        }
    }

    if pending_loop.is_some() {
        return fallback_general_message_plan(
            request.intent.raw_text.as_str(),
            pending_loop.and_then(|pending| pending.camera_hint.as_deref()),
        );
    }

    None
}

pub(super) fn plan_from_general_message_candidate(
    candidate: &GeneralMessageCandidate,
) -> GeneralMessagePlan {
    GeneralMessagePlan {
        kind: candidate.kind.clone(),
        conversation_act: None,
        reply_text: None,
        canonical_phrase: None,
        camera_hint: candidate.camera_hint.clone(),
        query: candidate.query.clone(),
        home_assistant_action: None,
        guardian_rule: None,
        event_id: None,
        corrected_summary: None,
        corrected_labels: None,
        confidence: Some(candidate.confidence),
        recent_clip: candidate.recent_clip.clone(),
        reason: Some(candidate.reason.clone()),
    }
}

pub(super) fn deterministic_stage_for_plan(plan: &GeneralMessagePlan) -> &'static str {
    match plan.kind {
        GeneralMessagePlanKind::Clarify => "deterministic_clarify",
        GeneralMessagePlanKind::ConversationAct => "deterministic_conversation_act",
        _ => "deterministic_single_candidate",
    }
}

pub(super) fn record_general_message_nsp_plan_trace(
    trace: &mut GeneralMessageControllerTrace,
    plan: &GeneralMessagePlan,
) {
    trace.nsp_decision = Some(general_message_plan_decision_label(plan).to_string());
    trace.nsp_confidence = plan.confidence;
    trace.nsp_canonical_phrase = plan.canonical_phrase.clone();
}

pub(super) fn general_message_nsp_validation_failure(
    request: &TaskRequest,
    plan: &GeneralMessagePlan,
) -> Option<String> {
    if general_message_external_boundary_hard_guard(request.intent.raw_text.as_str())
        && general_message_plan_is_actionable_tool(plan.kind)
    {
        return Some("nsp_boundary_hard_guard".to_string());
    }
    if !general_message_nsp_schema_valid(plan) {
        return Some("nsp_schema_invalid".to_string());
    }
    if general_message_plan_is_actionable_tool(plan.kind)
        && plan.confidence.unwrap_or(0) < GENERAL_MESSAGE_NSP_MIN_CONFIDENCE
    {
        return Some("nsp_low_confidence".to_string());
    }
    None
}

pub(super) fn enforce_general_message_nsp_hard_guards(
    request: &TaskRequest,
    plan: &mut GeneralMessagePlan,
) {
    let long_run_requested = evt_long_run_request_boundary(request.intent.raw_text.as_str());
    if !long_run_requested
        && !general_message_external_boundary_hard_guard(request.intent.raw_text.as_str())
    {
        return;
    }
    if matches!(
        (plan.kind, plan.conversation_act),
        (
            GeneralMessagePlanKind::ConversationAct,
            Some(GeneralMessageConversationAct::Boundary)
        )
    ) {
        if !long_run_requested {
            return;
        }
    }
    plan.kind = GeneralMessagePlanKind::ConversationAct;
    plan.conversation_act = Some(GeneralMessageConversationAct::Boundary);
    plan.home_assistant_action = None;
    plan.camera_hint = None;
    plan.query = None;
    plan.reply_text = long_run_requested.then(|| {
        "4h/72h 长压测必须走 operator supervisor，我不能从微信或 WebUI 启动；我可以帮你查看 EVT 就绪状态或生成脱敏证据包。"
            .to_string()
    });
    plan.canonical_phrase = Some(if long_run_requested {
        "长压测启动拒绝".to_string()
    } else {
        "边界拒绝".to_string()
    });
    plan.confidence = plan.confidence.or(Some(100));
    plan.reason = Some(if long_run_requested {
        "evt_long_run_requires_operator_supervisor".to_string()
    } else {
        "nsp_boundary_hard_guard".to_string()
    });
}

pub(super) fn general_message_nsp_schema_valid(plan: &GeneralMessagePlan) -> bool {
    if general_message_plan_is_actionable_tool(plan.kind) && plan.confidence.is_none() {
        return false;
    }
    if plan.kind == GeneralMessagePlanKind::HomeAssistantServiceAction
        && plan.home_assistant_action.is_none()
    {
        return false;
    }
    if plan.kind == GeneralMessagePlanKind::FamilyMemoryCorrectSummary
        && plan.corrected_summary.is_none()
        && plan.query.is_none()
    {
        return false;
    }
    if plan.kind == GeneralMessagePlanKind::FamilyMemoryCorrectLabels
        && plan
            .corrected_labels
            .as_ref()
            .map_or(true, |labels| labels.is_empty())
    {
        return false;
    }
    true
}

pub(super) fn general_message_plan_is_actionable_tool(kind: GeneralMessagePlanKind) -> bool {
    matches!(
        kind,
        GeneralMessagePlanKind::CameraReplayRecentClip
            | GeneralMessagePlanKind::CameraSnapshot
            | GeneralMessagePlanKind::CameraRecordClip
            | GeneralMessagePlanKind::KnowledgeSearch
            | GeneralMessagePlanKind::RagAnswer
            | GeneralMessagePlanKind::HomeAssistantServiceAction
            | GeneralMessagePlanKind::VisionEventSummary
            | GeneralMessagePlanKind::VisionEventNotifyLatest
            | GeneralMessagePlanKind::VlmDescribeLatestEvent
            | GeneralMessagePlanKind::VlmDescribeEvent
            | GeneralMessagePlanKind::FamilyMemorySummary
            | GeneralMessagePlanKind::SystemReadiness
            | GeneralMessagePlanKind::EvtReadiness
            | GeneralMessagePlanKind::EvtPreflight
            | GeneralMessagePlanKind::EvtEvidenceBundle
            | GeneralMessagePlanKind::FamilyTimelineSummary
            | GeneralMessagePlanKind::FamilyTimelineQuery
            | GeneralMessagePlanKind::GuardianRuleProposal
            | GeneralMessagePlanKind::GuardianRuleList
            | GeneralMessagePlanKind::GuardianRuleEnable
            | GeneralMessagePlanKind::GuardianRulePause
            | GeneralMessagePlanKind::GuardianStatus
            | GeneralMessagePlanKind::FamilyMemoryConfirm
            | GeneralMessagePlanKind::FamilyMemoryFavorite
            | GeneralMessagePlanKind::FamilyMemoryHide
            | GeneralMessagePlanKind::FamilyMemoryCorrectSummary
            | GeneralMessagePlanKind::FamilyMemoryCorrectLabels
            | GeneralMessagePlanKind::FamilyMemoryShowFavorites
    )
}

pub(super) fn general_message_external_boundary_hard_guard(raw_text: &str) -> bool {
    if evt_long_run_request_boundary(raw_text) {
        return true;
    }
    let normalized = normalize_command_text(raw_text).to_ascii_lowercase();
    if [
        "天气",
        "气温",
        "下雨",
        "新闻",
        "股票",
        "股价",
        "行情",
        "解锁",
        "开锁",
        "门锁",
        "空调",
        "暖气",
        "阀门",
        "燃气",
        "煤气",
        "插座大功率",
        "temperature",
        "weather",
        "news",
        "stock",
        "unlock",
        "lock",
        "hvac",
        "valve",
    ]
    .iter()
    .any(|keyword| normalized.contains(keyword))
    {
        return true;
    }
    matches!(
        infer_general_message_conversation_act(raw_text, None),
        GeneralMessageConversationAct::Boundary
    )
}

pub(super) fn general_message_plan_decision_label(plan: &GeneralMessagePlan) -> &'static str {
    match plan.kind {
        GeneralMessagePlanKind::Clarify => "clarify",
        GeneralMessagePlanKind::CapabilitySummary => "capability_summary",
        GeneralMessagePlanKind::CameraReplayRecentClip => "camera_replay_recent_clip",
        GeneralMessagePlanKind::CameraSnapshot => "camera_snapshot",
        GeneralMessagePlanKind::CameraRecordClip => "camera_record_clip",
        GeneralMessagePlanKind::KnowledgeSearch => "knowledge_search",
        GeneralMessagePlanKind::RagAnswer => "rag_answer",
        GeneralMessagePlanKind::HomeAssistantServiceAction => "ha_service_action",
        GeneralMessagePlanKind::VisionEventSummary => "vision_event_summary",
        GeneralMessagePlanKind::VisionEventNotifyLatest => "vision_event_notify_latest",
        GeneralMessagePlanKind::VlmDescribeLatestEvent => "vlm_describe_latest_event",
        GeneralMessagePlanKind::VlmDescribeEvent => "vlm_describe_event",
        GeneralMessagePlanKind::FamilyMemorySummary => "family_memory_summary",
        GeneralMessagePlanKind::SystemReadiness => "system_readiness",
        GeneralMessagePlanKind::EvtReadiness => "evt_readiness",
        GeneralMessagePlanKind::EvtPreflight => "evt_preflight",
        GeneralMessagePlanKind::EvtEvidenceBundle => "evt_evidence_bundle",
        GeneralMessagePlanKind::FamilyTimelineSummary => "family_timeline_summary",
        GeneralMessagePlanKind::FamilyTimelineQuery => "family_timeline_query",
        GeneralMessagePlanKind::GuardianRuleProposal => "guardian_rule_proposal",
        GeneralMessagePlanKind::GuardianRuleList => "guardian_rule_list",
        GeneralMessagePlanKind::GuardianRuleEnable => "guardian_rule_enable",
        GeneralMessagePlanKind::GuardianRulePause => "guardian_rule_pause",
        GeneralMessagePlanKind::GuardianStatus => "guardian_status",
        GeneralMessagePlanKind::FamilyMemoryConfirm => "family_memory_confirm",
        GeneralMessagePlanKind::FamilyMemoryFavorite => "family_memory_favorite",
        GeneralMessagePlanKind::FamilyMemoryHide => "family_memory_hide",
        GeneralMessagePlanKind::FamilyMemoryCorrectSummary => "family_memory_correct_summary",
        GeneralMessagePlanKind::FamilyMemoryCorrectLabels => "family_memory_correct_labels",
        GeneralMessagePlanKind::FamilyMemoryShowFavorites => "family_memory_show_favorites",
        GeneralMessagePlanKind::ConversationAct => plan
            .conversation_act
            .map(GeneralMessageConversationAct::reply_pack_kind)
            .unwrap_or("conversation_continue"),
        GeneralMessagePlanKind::Unsupported => "unsupported",
    }
}

pub(super) fn should_try_general_message_router_llm(
    signals: &GeneralMessageSignals,
    _pending_loop: Option<&PendingTaskGeneralMessageLoop>,
) -> bool {
    !signals.normalized.is_empty()
}

pub(super) fn build_general_message_router_system_prompt() -> String {
    concat!(
        "/no_think\n",
        "You are HarborBeacon's local-only Natural Semantic Parser (NSP). ",
        "Return exactly one valid JSON object and no markdown. Schema: ",
        "{\"decision\":\"...\",\"confidence\":0.95,\"canonical_phrase\":\"...\",",
        "\"camera_hint\":null,\"query\":null,\"event_id\":null,",
        "\"corrected_summary\":null,\"corrected_labels\":null,",
        "\"home_assistant\":{\"domain\":null,\"service\":null,\"entity_hint\":null},",
        "\"conversation_act\":null,\"reply_text\":null,\"reason\":\"...\"}. ",
        "Closed decisions: capability_summary, camera_snapshot, camera_record_clip, ",
        "vision_event_summary, vision_event_notify_latest, vlm_describe_latest_event, ",
        "vlm_describe_event, family_memory_summary, ha_service_action, system_readiness, ",
        "evt_readiness, evt_preflight, evt_evidence_bundle, family_timeline_summary, ",
        "family_timeline_query, guardian_rule_proposal, guardian_rule_list, guardian_rule_enable, ",
        "guardian_rule_pause, guardian_status, knowledge_search, rag_answer, ",
        "family_memory_confirm, family_memory_favorite, family_memory_hide, ",
        "family_memory_correct_summary, family_memory_correct_labels, ",
        "family_memory_show_favorites, ",
        "clarify, conversation_continue, conversation_boundary, ",
        "conversation_repair, conversation_cancel, conversation_clarify_continue. ",
        "For ha_service_action, fill only domain/service/entity_hint. Allowed HA outputs are ",
        "light/switch/input_boolean turn_on/turn_off/toggle and scene turn_on. ",
        "Never extract arbitrary service fields. Use confidence 0.75-0.99 for clear decisions; ",
        "do not copy 0.0 unless the request is genuinely unclear. Chinese semantic rules: ",
        "asking what Harbor/K3 can do means capability_summary; asking status/diagnostics/health ",
        "of the home, WeChat entry, K3, HA, camera, or gateway means system_readiness; asking if ",
        "K3 is ready for EVT/stress-test entry means evt_readiness; asking to run an EVT/pre-stress ",
        "environment preflight means evt_preflight; asking to generate EVT/stress-test evidence ",
        "means evt_evidence_bundle; asking to start/run a 4h or 72h stress test must be ",
        "conversation_boundary because long runs require the operator supervisor; asking what ",
        "happened at home today means family_timeline_summary; asking whether a camera/door saw ",
        "a person/event today means family_timeline_query; asking to create a future rule such as ",
        "notify me when someone appears or turn on a low-risk HA entity when an event happens means ",
        "guardian_rule_proposal with guardian_rule trigger/action_plan slots; asking to enable this ",
        "rule means guardian_rule_enable; asking to pause/cancel this rule means guardian_rule_pause; ",
        "asking family guardian status means guardian_status; asking to ",
        "look/check/see the door/front/current camera means camera_snapshot; asking for a short ",
        "recording/video/clip means camera_record_clip; asking what the camera recently saw means ",
        "vision_event_summary; asking to describe or understand the latest visual event means ",
        "vlm_describe_latest_event; asking what is worth noticing at home means ",
        "family_memory_summary; asking to confirm/use, favorite, hide, restore/show favorites, ",
        "or correct the previous referenced family event means the matching family_memory_* ",
        "decision; use event_id only when explicitly given, and put short corrected text in ",
        "corrected_summary or corrected_labels slots; asking to send/share/notify the latest camera situation means ",
        "vision_event_notify_latest, including requests to send it to the default notification ",
        "target/contact; asking to run/activate/execute a scene/routine means ha_service_action ",
        "with domain scene, service turn_on, and entity_hint copied as a short scene description; ",
        "asking to turn on/off/toggle a light, switch, or input_boolean means ha_service_action ",
        "with safe slots only. Do not classify supported K3 camera/event/status/HA requests as ",
        "conversation_boundary just because the wording is natural. Choose conversation_boundary ",
        "for weather, news, stocks, internet realtime requests, locks, HVAC, valves, or unsafe ",
        "actions. Examples: latest camera status to my default contact => ",
        "{\"decision\":\"vision_event_notify_latest\",\"confidence\":0.95}; ",
        "run the home test scene => {\"decision\":\"ha_service_action\",\"confidence\":0.95,",
        "\"home_assistant\":{\"domain\":\"scene\",\"service\":\"turn_on\",\"entity_hint\":\"test\"}}; ",
        "压测前状态怎么样 => {\"decision\":\"evt_readiness\",\"confidence\":0.95}; ",
        "帮我做一下EVT预检 => {\"decision\":\"evt_preflight\",\"confidence\":0.95}; ",
        "生成压测证据 => {\"decision\":\"evt_evidence_bundle\",\"confidence\":0.95}; ",
        "今天家里发生了什么 => {\"decision\":\"family_timeline_summary\",\"confidence\":0.95}; ",
        "刚才门口发生了什么 => {\"decision\":\"vlm_describe_latest_event\",\"confidence\":0.95,\"camera_hint\":\"门口\"}; ",
        "今天家里有什么值得注意的 => {\"decision\":\"family_memory_summary\",\"confidence\":0.95}; ",
        "收藏这个 => {\"decision\":\"family_memory_favorite\",\"confidence\":0.95}; ",
        "隐藏这个事件 => {\"decision\":\"family_memory_hide\",\"confidence\":0.95}; ",
        "这个不对，是快递 => {\"decision\":\"family_memory_correct_summary\",\"confidence\":0.95,\"corrected_summary\":\"快递\"}; ",
        "看我收藏的家庭记忆 => {\"decision\":\"family_memory_show_favorites\",\"confidence\":0.95}; ",
        "以后门口有人就通知我 => {\"decision\":\"guardian_rule_proposal\",\"confidence\":0.95,",
        "\"guardian_rule\":{\"trigger\":{\"camera_id\":\"门口\",\"event_type\":\"person_detected\",",
        "\"labels\":[\"person\"],\"min_confidence\":0.6},\"action_plan\":{\"actions\":[{\"kind\":\"notify_default_target\"}]}}}; ",
        "启用这个规则 => {\"decision\":\"guardian_rule_enable\",\"confidence\":0.95}; ",
        "家庭守护状态 => {\"decision\":\"guardian_status\",\"confidence\":0.95}."
    )
    .to_string()
}

pub(super) fn build_general_message_router_prompt(
    request: &TaskRequest,
    session_recap: &[Value],
    pending_loop: Option<&PendingTaskGeneralMessageLoop>,
) -> String {
    format!(
        concat!(
            "User message: {message}\n",
            "Recent session recap (newest first, max {limit}): {session_recap}\n",
            "Pending loop context: {pending_loop}\n",
            "Translate the user's natural language into one safe HarborBeacon decision. ",
            "If the message is not a clear supported tool request, choose the best conversation_* act. ",
            "Use confidence 0.0-1.0. Do not include secrets or transport identifiers.\n"
        ),
        message = request.intent.raw_text,
        limit = GENERAL_MESSAGE_RECAP_LIMIT,
        session_recap = serde_json::to_string(session_recap).unwrap_or_else(|_| "[]".to_string()),
        pending_loop = serde_json::to_string(&pending_loop.map(|pending| {
            json!({
                "original_goal": pending.original_goal,
                "latest_user_intent_text": pending.latest_user_intent_text,
                "last_clarification_prompt": pending.last_clarification_prompt,
                "camera_hint": pending.camera_hint,
                "query": pending.query,
            })
        }))
        .unwrap_or_else(|_| "null".to_string()),
    )
}

pub(super) fn parse_general_message_router_decision(
    text: &str,
) -> Option<(
    GeneralMessagePlanKind,
    Option<GeneralMessageConversationAct>,
)> {
    if let Some(plan) = parse_general_message_plan(text) {
        return Some((plan.kind, plan.conversation_act));
    }

    let candidates = [
        text.trim().to_ascii_lowercase(),
        text.lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase(),
        text.split_whitespace()
            .next()
            .unwrap_or_default()
            .trim_matches(|ch: char| matches!(ch, '"' | '\'' | '`' | ',' | '.' | ';' | '：' | ':'))
            .to_ascii_lowercase(),
    ];

    for candidate in candidates {
        match candidate.as_str() {
            "clarify" => return Some((GeneralMessagePlanKind::Clarify, None)),
            "capability_summary" | "capability" | "help" => {
                return Some((GeneralMessagePlanKind::CapabilitySummary, None))
            }
            "camera_snapshot" | "snapshot" => {
                return Some((GeneralMessagePlanKind::CameraSnapshot, None))
            }
            "camera_record_clip" | "record_clip" | "clip" => {
                return Some((GeneralMessagePlanKind::CameraRecordClip, None))
            }
            "knowledge_search" | "search" => {
                return Some((GeneralMessagePlanKind::KnowledgeSearch, None))
            }
            "rag_answer" | "rag.answer" | "answer" => {
                return Some((GeneralMessagePlanKind::RagAnswer, None))
            }
            "ha_service_action" | "home_assistant_service_action" | "home_assistant_action" => {
                return Some((GeneralMessagePlanKind::HomeAssistantServiceAction, None))
            }
            "vision_event_summary" | "event_summary" | "recent_event" => {
                return Some((GeneralMessagePlanKind::VisionEventSummary, None))
            }
            "vision_event_notify_latest" | "event_notify" | "notify_latest_event" => {
                return Some((GeneralMessagePlanKind::VisionEventNotifyLatest, None))
            }
            "vlm_describe_latest_event" | "vlm_describe_latest" | "describe_latest_event" => {
                return Some((GeneralMessagePlanKind::VlmDescribeLatestEvent, None))
            }
            "vlm_describe_event" | "describe_event" => {
                return Some((GeneralMessagePlanKind::VlmDescribeEvent, None))
            }
            "family_memory_summary" | "family_memory" => {
                return Some((GeneralMessagePlanKind::FamilyMemorySummary, None))
            }
            "system_readiness" | "status" | "diagnostics" => {
                return Some((GeneralMessagePlanKind::SystemReadiness, None))
            }
            "evt_readiness" | "evt_status" | "stress_readiness" => {
                return Some((GeneralMessagePlanKind::EvtReadiness, None))
            }
            "evt_preflight" | "stress_preflight" => {
                return Some((GeneralMessagePlanKind::EvtPreflight, None))
            }
            "evt_evidence_bundle" | "evt_evidence" | "stress_evidence" => {
                return Some((GeneralMessagePlanKind::EvtEvidenceBundle, None))
            }
            "family_timeline_summary" | "family_timeline" | "home_timeline" => {
                return Some((GeneralMessagePlanKind::FamilyTimelineSummary, None))
            }
            "family_timeline_query" | "home_timeline_query" => {
                return Some((GeneralMessagePlanKind::FamilyTimelineQuery, None))
            }
            "guardian_rule_proposal" | "home_guardian_rule_proposal" => {
                return Some((GeneralMessagePlanKind::GuardianRuleProposal, None))
            }
            "guardian_rule_list" | "home_guardian_rule_list" => {
                return Some((GeneralMessagePlanKind::GuardianRuleList, None))
            }
            "guardian_rule_enable" | "home_guardian_rule_enable" => {
                return Some((GeneralMessagePlanKind::GuardianRuleEnable, None))
            }
            "guardian_rule_pause" | "guardian_rule_cancel" | "home_guardian_rule_pause" => {
                return Some((GeneralMessagePlanKind::GuardianRulePause, None))
            }
            "guardian_status" | "home_guardian_status" => {
                return Some((GeneralMessagePlanKind::GuardianStatus, None))
            }
            "family_memory_confirm" => {
                return Some((GeneralMessagePlanKind::FamilyMemoryConfirm, None))
            }
            "family_memory_favorite" => {
                return Some((GeneralMessagePlanKind::FamilyMemoryFavorite, None))
            }
            "family_memory_hide" => return Some((GeneralMessagePlanKind::FamilyMemoryHide, None)),
            "family_memory_correct_summary" => {
                return Some((GeneralMessagePlanKind::FamilyMemoryCorrectSummary, None))
            }
            "family_memory_correct_labels" => {
                return Some((GeneralMessagePlanKind::FamilyMemoryCorrectLabels, None))
            }
            "family_memory_show_favorites" => {
                return Some((GeneralMessagePlanKind::FamilyMemoryShowFavorites, None))
            }
            "conversation" | "conversation_continue" | "continue" => {
                return Some((
                    GeneralMessagePlanKind::ConversationAct,
                    Some(GeneralMessageConversationAct::Continue),
                ))
            }
            "conversation_boundary" | "boundary" | "unsupported" => {
                return Some((
                    GeneralMessagePlanKind::ConversationAct,
                    Some(GeneralMessageConversationAct::Boundary),
                ))
            }
            "conversation_repair" | "repair" => {
                return Some((
                    GeneralMessagePlanKind::ConversationAct,
                    Some(GeneralMessageConversationAct::Repair),
                ))
            }
            "conversation_cancel" | "cancel" => {
                return Some((
                    GeneralMessagePlanKind::ConversationAct,
                    Some(GeneralMessageConversationAct::Cancel),
                ))
            }
            "conversation_clarify_continue" | "clarify_continue" => {
                return Some((
                    GeneralMessagePlanKind::ConversationAct,
                    Some(GeneralMessageConversationAct::ClarifyContinue),
                ))
            }
            _ => {}
        }
    }

    None
}

pub(super) fn build_general_message_renderer_system_prompt() -> String {
    "You are a concise Chinese HarborBeacon reply writer. Output only one short Chinese user-facing sentence or question. Do not mention internal reasoning or JSON.".to_string()
}

pub(super) fn build_general_message_renderer_prompt(
    request: &TaskRequest,
    pending_loop: Option<&PendingTaskGeneralMessageLoop>,
    plan: &GeneralMessagePlan,
    fallback_text: &str,
) -> String {
    format!(
        concat!(
            "Reply kind: {kind}\n",
            "Current user message: {message}\n",
            "Pending loop context: {pending_loop}\n",
            "Fallback text: {fallback}\n",
            "Write a short natural Chinese reply. If the fallback text is already appropriate, keep its meaning.\n"
        ),
        kind = match plan.kind {
            GeneralMessagePlanKind::Clarify => "clarify",
            GeneralMessagePlanKind::ConversationAct => plan
                .conversation_act
                .map(GeneralMessageConversationAct::reply_pack_kind)
                .unwrap_or("conversation_continue"),
            GeneralMessagePlanKind::Unsupported => "unsupported",
            GeneralMessagePlanKind::CapabilitySummary => "capability_summary",
            GeneralMessagePlanKind::CameraReplayRecentClip => "camera_replay_recent_clip",
            GeneralMessagePlanKind::CameraSnapshot => "camera_snapshot",
            GeneralMessagePlanKind::CameraRecordClip => "camera_record_clip",
            GeneralMessagePlanKind::KnowledgeSearch => "knowledge_search",
            GeneralMessagePlanKind::RagAnswer => "rag_answer",
            GeneralMessagePlanKind::HomeAssistantServiceAction => "ha_service_action",
            GeneralMessagePlanKind::VisionEventSummary => "vision_event_summary",
            GeneralMessagePlanKind::VisionEventNotifyLatest => "vision_event_notify_latest",
            GeneralMessagePlanKind::VlmDescribeLatestEvent => "vlm_describe_latest_event",
            GeneralMessagePlanKind::VlmDescribeEvent => "vlm_describe_event",
            GeneralMessagePlanKind::FamilyMemorySummary => "family_memory_summary",
            GeneralMessagePlanKind::SystemReadiness => "system_readiness",
            GeneralMessagePlanKind::EvtReadiness => "evt_readiness",
            GeneralMessagePlanKind::EvtPreflight => "evt_preflight",
            GeneralMessagePlanKind::EvtEvidenceBundle => "evt_evidence_bundle",
            GeneralMessagePlanKind::FamilyTimelineSummary => "family_timeline_summary",
            GeneralMessagePlanKind::FamilyTimelineQuery => "family_timeline_query",
            GeneralMessagePlanKind::GuardianRuleProposal => "guardian_rule_proposal",
            GeneralMessagePlanKind::GuardianRuleList => "guardian_rule_list",
            GeneralMessagePlanKind::GuardianRuleEnable => "guardian_rule_enable",
            GeneralMessagePlanKind::GuardianRulePause => "guardian_rule_pause",
            GeneralMessagePlanKind::GuardianStatus => "guardian_status",
            GeneralMessagePlanKind::FamilyMemoryConfirm => "family_memory_confirm",
            GeneralMessagePlanKind::FamilyMemoryFavorite => "family_memory_favorite",
            GeneralMessagePlanKind::FamilyMemoryHide => "family_memory_hide",
            GeneralMessagePlanKind::FamilyMemoryCorrectSummary => "family_memory_correct_summary",
            GeneralMessagePlanKind::FamilyMemoryCorrectLabels => "family_memory_correct_labels",
            GeneralMessagePlanKind::FamilyMemoryShowFavorites => "family_memory_show_favorites",
        },
        message = request.intent.raw_text,
        pending_loop = serde_json::to_string(&pending_loop.map(|pending| {
            json!({
                "original_goal": pending.original_goal,
                "latest_user_intent_text": pending.latest_user_intent_text,
                "last_clarification_prompt": pending.last_clarification_prompt,
            })
        }))
        .unwrap_or_else(|_| "null".to_string()),
        fallback = fallback_text,
    )
}

pub(super) fn general_message_requests_capability_summary(raw_text: &str) -> bool {
    let normalized = normalize_command_text(raw_text);
    if normalized.is_empty() {
        return false;
    }

    if general_message_requests_local_first_architecture_summary(raw_text) {
        return true;
    }

    let exact_matches = ["帮助", "帮助一下", "help", "helpme"];
    if exact_matches
        .iter()
        .any(|candidate| normalized == normalize_command_text(candidate))
    {
        return true;
    }

    [
        "你能做什么",
        "你还能做什么",
        "你可以做什么",
        "你会做什么",
        "你能干什么",
        "你可以干什么",
        "你能帮我做什么",
        "你还能帮我做什么",
        "摄像头能做什么",
        "摄像头可以做什么",
        "摄像头能干什么",
        "摄像头可以干什么",
        "监控能做什么",
        "监控可以做什么",
        "监控能干什么",
        "监控可以干什么",
    ]
    .iter()
    .map(|candidate| normalize_command_text(candidate))
    .any(|candidate| normalized.contains(&candidate))
}

pub(super) fn general_message_requests_local_first_architecture_summary(raw_text: &str) -> bool {
    let normalized = normalize_command_text(raw_text);
    if normalized.is_empty() {
        return false;
    }

    let mentions_harbor_core = matches_any(&normalized, &["harborbeacon", "harborgate"]);
    let mentions_policy_or_fallback = matches_any(
        &normalized,
        &[
            "fallback", "回退", "云端", "privacy", "resource", "policy", "策略",
        ],
    );
    let mentions_local_first = matches_any(
        &normalized,
        &[
            "local-first",
            "localfirst",
            "本地优先",
            "本地优先策略",
            "云端fallback",
            "受控fallback",
            "受控回退",
        ],
    );
    let asks_architecture = matches_any(
        &normalized,
        &[
            "架构",
            "怎么受控",
            "怎么工作",
            "如何工作",
            "解释一下",
            "说明一下",
            "讲一下",
        ],
    );

    asks_architecture
        && (mentions_local_first || mentions_harbor_core || mentions_policy_or_fallback)
}

pub(super) fn general_message_supported_examples() -> Vec<String> {
    vec![
        "拍一张门口".to_string(),
        "录一段门口".to_string(),
        "最近事件".to_string(),
        "通知最新事件".to_string(),
        "开灯".to_string(),
        "关灯".to_string(),
        "切换开关".to_string(),
        "执行测试场景".to_string(),
        "状态".to_string(),
        "压测前状态怎么样".to_string(),
        "帮我做一下EVT预检".to_string(),
        "生成压测证据".to_string(),
        "今天家里发生了什么".to_string(),
        "刚才门口发生了什么".to_string(),
        "今天家里有什么值得注意的".to_string(),
        "以后门口有人就通知我".to_string(),
        "家庭守护状态".to_string(),
        "帮我找到和樱花有关的文件".to_string(),
        "根据资料回答樱花计划是什么".to_string(),
    ]
}

pub(super) fn general_message_support_summary_for_request(raw_text: &str) -> String {
    if general_message_requests_local_first_architecture_summary(raw_text) {
        return "当前链路默认 local-first：HarborBeacon 负责业务状态、RAG 和策略裁决，HarborGate 只负责 IM 传输；云端模型只有在 privacy/resource policy 放行时才作为受控 fallback，SiliconFlow 只是当前 .82 fallback proof，不是默认架构。".to_string();
    }

    general_message_support_summary()
}

pub(super) fn general_message_support_summary() -> String {
    "我可以作为 K3 家庭入口：摄像头抓拍/短视频、最近事件、通知最新事件、按需 VLM 理解最新事件、家庭记忆摘要、家庭时间线、家庭守护规则草稿/启用/暂停、低风险家居动作、状态诊断、EVT 就绪/预检/脱敏证据包，也能搜索/问答已有知识库；守护规则只有你明确启用后才会自动执行，遇到多个家居实体时我会先让你选择，不会猜着执行，VLM 只走本地按需/低频抽样，4h/72h 长压测必须走 operator supervisor。".to_string()
}

pub(super) fn general_message_unsupported_summary() -> String {
    let examples = general_message_supported_examples();
    format!(
        "这类请求我现在不处理；我不接公网天气、新闻、股票等外部实时信息。你可以直接说：{}。",
        examples.join("；")
    )
}

pub(super) fn infer_general_message_conversation_act(
    raw_text: &str,
    pending_loop: Option<&PendingTaskGeneralMessageLoop>,
) -> GeneralMessageConversationAct {
    let normalized = normalize_command_text(raw_text);
    if matches_any(
        &normalized,
        &["算了", "不用了", "先不用", "不要了", "别处理", "取消"],
    ) || normalized == normalize_command_text("不要")
    {
        return GeneralMessageConversationAct::Cancel;
    }
    if matches_any(
        &normalized,
        &["不对", "不是", "不是这个", "错了", "理解错了", "重新来"],
    ) {
        return GeneralMessageConversationAct::Repair;
    }
    if pending_loop.is_some() {
        return GeneralMessageConversationAct::ClarifyContinue;
    }
    if matches_any(
        &normalized,
        &[
            "天气",
            "温度",
            "下雨",
            "新闻",
            "股价",
            "股票",
            "汇率",
            "外卖",
            "打车",
            "讲个笑话",
            "唱歌",
            "播放音乐",
        ],
    ) {
        return GeneralMessageConversationAct::Boundary;
    }
    GeneralMessageConversationAct::Continue
}

pub(super) fn general_message_conversation_summary(
    request: &TaskRequest,
    pending_loop: Option<&PendingTaskGeneralMessageLoop>,
    act: GeneralMessageConversationAct,
) -> String {
    match act {
        GeneralMessageConversationAct::Continue => {
            "收到，有需要你直接说要看什么或找什么。".to_string()
        }
        GeneralMessageConversationAct::Boundary => {
            let normalized = normalize_command_text(request.intent.raw_text.as_str());
            if matches_any(&normalized, &["天气", "温度", "下雨"]) {
                return "天气这类公网实时信息我现在不处理；我可以帮你看摄像头、查最近事件、通知默认目标、执行低风险家居动作或看状态。"
                    .to_string();
            }
            "这件事我现在不直接处理；我可以帮你看摄像头、查最近事件、通知默认目标、执行低风险家居动作或看状态。".to_string()
        }
        GeneralMessageConversationAct::Repair => {
            "收到，我重新理解；你可以换个说法告诉我要处理什么。".to_string()
        }
        GeneralMessageConversationAct::Cancel => "好的，先不处理这件事。".to_string(),
        GeneralMessageConversationAct::ClarifyContinue => pending_loop
            .and_then(|pending| {
                let prompt = pending.last_clarification_prompt.trim();
                (!prompt.is_empty()).then(|| format!("收到。{prompt}"))
            })
            .unwrap_or_else(|| {
                "收到。你可以继续补一句具体要拍、录、查事件、通知、开关设备还是看状态。".to_string()
            }),
    }
}

pub(super) fn general_message_default_clarification_prompt(raw_text: &str) -> String {
    let normalized = normalize_command_text(raw_text);
    if normalized.contains("看") || normalized.contains("门口") || normalized.contains("摄像头")
    {
        return "你是想让我拍一张最新画面、录一段短视频，还是搜索已经保存的内容？".to_string();
    }
    "你是想让我拍一张、录一段，还是搜索已有内容？".to_string()
}

pub(super) fn parse_general_message_plan(text: &str) -> Option<GeneralMessagePlan> {
    let payload = parse_json_object_from_text(text)?;
    let payload = serde_json::from_value::<GeneralMessagePlanPayload>(payload).ok()?;
    let decision = if payload.decision.trim().is_empty() {
        payload.action.trim().to_ascii_lowercase()
    } else {
        payload.decision.trim().to_ascii_lowercase()
    };
    let payload_conversation_act = payload
        .conversation_act
        .as_deref()
        .and_then(parse_general_message_conversation_act_label);
    let (kind, conversation_act) = match decision.as_str() {
        "clarify" => (GeneralMessagePlanKind::Clarify, None),
        "capability_summary" | "capability" | "help" => {
            (GeneralMessagePlanKind::CapabilitySummary, None)
        }
        "camera_snapshot" | "snapshot" => (GeneralMessagePlanKind::CameraSnapshot, None),
        "camera_record_clip" | "record_clip" | "clip" => {
            (GeneralMessagePlanKind::CameraRecordClip, None)
        }
        "knowledge_search" | "search" => (GeneralMessagePlanKind::KnowledgeSearch, None),
        "rag_answer" | "rag.answer" | "answer" => (GeneralMessagePlanKind::RagAnswer, None),
        "ha_service_action" | "home_assistant_service_action" | "home_assistant_action" => {
            (GeneralMessagePlanKind::HomeAssistantServiceAction, None)
        }
        "vision_event_summary" | "event_summary" | "recent_event" => {
            (GeneralMessagePlanKind::VisionEventSummary, None)
        }
        "vision_event_notify_latest" | "event_notify" | "notify_latest_event" => {
            (GeneralMessagePlanKind::VisionEventNotifyLatest, None)
        }
        "vlm_describe_latest_event" | "vlm_describe_latest" | "describe_latest_event" => {
            (GeneralMessagePlanKind::VlmDescribeLatestEvent, None)
        }
        "vlm_describe_event" | "describe_event" => (GeneralMessagePlanKind::VlmDescribeEvent, None),
        "family_memory_summary" | "family_memory" => {
            (GeneralMessagePlanKind::FamilyMemorySummary, None)
        }
        "system_readiness" | "status" | "diagnostics" => {
            (GeneralMessagePlanKind::SystemReadiness, None)
        }
        "evt_readiness" | "evt_status" | "stress_readiness" => {
            (GeneralMessagePlanKind::EvtReadiness, None)
        }
        "evt_preflight" | "stress_preflight" => (GeneralMessagePlanKind::EvtPreflight, None),
        "evt_evidence_bundle" | "evt_evidence" | "stress_evidence" => {
            (GeneralMessagePlanKind::EvtEvidenceBundle, None)
        }
        "family_timeline_summary" | "family_timeline" | "home_timeline" => {
            (GeneralMessagePlanKind::FamilyTimelineSummary, None)
        }
        "family_timeline_query" | "home_timeline_query" => {
            (GeneralMessagePlanKind::FamilyTimelineQuery, None)
        }
        "guardian_rule_proposal" | "home_guardian_rule_proposal" => {
            (GeneralMessagePlanKind::GuardianRuleProposal, None)
        }
        "guardian_rule_list" | "home_guardian_rule_list" => {
            (GeneralMessagePlanKind::GuardianRuleList, None)
        }
        "guardian_rule_enable" | "home_guardian_rule_enable" => {
            (GeneralMessagePlanKind::GuardianRuleEnable, None)
        }
        "guardian_rule_pause" | "guardian_rule_cancel" | "home_guardian_rule_pause" => {
            (GeneralMessagePlanKind::GuardianRulePause, None)
        }
        "guardian_status" | "home_guardian_status" => {
            (GeneralMessagePlanKind::GuardianStatus, None)
        }
        "family_memory_confirm" | "memory_confirm" => {
            (GeneralMessagePlanKind::FamilyMemoryConfirm, None)
        }
        "family_memory_favorite" | "memory_favorite" => {
            (GeneralMessagePlanKind::FamilyMemoryFavorite, None)
        }
        "family_memory_hide" | "memory_hide" => (GeneralMessagePlanKind::FamilyMemoryHide, None),
        "family_memory_correct_summary" | "memory_correct_summary" => {
            (GeneralMessagePlanKind::FamilyMemoryCorrectSummary, None)
        }
        "family_memory_correct_labels" | "memory_correct_labels" => {
            (GeneralMessagePlanKind::FamilyMemoryCorrectLabels, None)
        }
        "family_memory_show_favorites" | "memory_show_favorites" => {
            (GeneralMessagePlanKind::FamilyMemoryShowFavorites, None)
        }
        "conversation" | "conversation_continue" | "continue" => (
            GeneralMessagePlanKind::ConversationAct,
            Some(payload_conversation_act.unwrap_or(GeneralMessageConversationAct::Continue)),
        ),
        "conversation_boundary" | "boundary" | "unsupported" => (
            GeneralMessagePlanKind::ConversationAct,
            Some(payload_conversation_act.unwrap_or(GeneralMessageConversationAct::Boundary)),
        ),
        "conversation_repair" | "repair" => (
            GeneralMessagePlanKind::ConversationAct,
            Some(payload_conversation_act.unwrap_or(GeneralMessageConversationAct::Repair)),
        ),
        "conversation_cancel" | "cancel" => (
            GeneralMessagePlanKind::ConversationAct,
            Some(payload_conversation_act.unwrap_or(GeneralMessageConversationAct::Cancel)),
        ),
        "conversation_clarify_continue" | "clarify_continue" => (
            GeneralMessagePlanKind::ConversationAct,
            Some(
                payload_conversation_act.unwrap_or(GeneralMessageConversationAct::ClarifyContinue),
            ),
        ),
        _ => return None,
    };
    let confidence = normalize_general_message_nsp_confidence(payload.confidence.as_ref());
    let home_assistant_action = if kind == GeneralMessagePlanKind::HomeAssistantServiceAction {
        home_assistant_nsp_action_from_plan_payload(&payload)
    } else {
        None
    };
    Some(GeneralMessagePlan {
        kind,
        conversation_act,
        reply_text: normalize_optional_general_message_plan_field(payload.reply_text),
        canonical_phrase: normalize_optional_general_message_plan_field(payload.canonical_phrase),
        camera_hint: normalize_optional_general_message_plan_field(payload.camera_hint),
        query: normalize_optional_general_message_plan_field(payload.query),
        home_assistant_action,
        guardian_rule: payload.guardian_rule,
        event_id: normalize_optional_general_message_plan_field(payload.event_id),
        corrected_summary: normalize_optional_general_message_plan_field(payload.corrected_summary),
        corrected_labels: payload.corrected_labels,
        confidence,
        recent_clip: None,
        reason: normalize_optional_general_message_plan_field(payload.reason),
    })
}

pub(super) fn normalize_general_message_nsp_confidence(value: Option<&Value>) -> Option<u8> {
    let raw = match value? {
        Value::Number(number) => number.as_f64()?,
        Value::String(text) => text.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    if !raw.is_finite() {
        return None;
    }
    let percent = if raw <= 1.0 { raw * 100.0 } else { raw };
    Some(percent.round().clamp(0.0, 100.0) as u8)
}

pub(super) fn home_assistant_nsp_action_from_plan_payload(
    payload: &GeneralMessagePlanPayload,
) -> Option<HomeAssistantNaturalAction> {
    let nested = payload.home_assistant.as_ref().or(payload.ha.as_ref());
    let domain = normalize_optional_general_message_plan_field(
        nested
            .and_then(|value| value.domain.clone())
            .or_else(|| payload.domain.clone()),
    )?
    .to_ascii_lowercase();
    let service = normalize_optional_general_message_plan_field(
        nested
            .and_then(|value| value.service.clone())
            .or_else(|| payload.service.clone()),
    )?
    .to_ascii_lowercase();
    let entity_hint = normalize_optional_general_message_plan_field(
        nested
            .and_then(|value| value.entity_hint.clone())
            .or_else(|| payload.entity_hint.clone()),
    );
    let request = HomeAssistantNaturalActionRequest {
        domain,
        service,
        entity_hint,
    };
    if !is_low_risk_home_assistant_service_action(&request) {
        return Some(HomeAssistantNaturalAction::Blocked {
            message: "这类 Home Assistant 动作不在低风险 allowlist 中，本次没有执行。".to_string(),
        });
    }
    Some(HomeAssistantNaturalAction::Request(request))
}

pub(super) fn is_low_risk_home_assistant_service_action(
    action: &HomeAssistantNaturalActionRequest,
) -> bool {
    match (action.domain.as_str(), action.service.as_str()) {
        ("light" | "switch" | "input_boolean", "turn_on" | "turn_off" | "toggle") => true,
        ("scene", "turn_on") => true,
        _ => false,
    }
}

pub(super) fn parse_general_message_conversation_act_label(
    label: &str,
) -> Option<GeneralMessageConversationAct> {
    match label.trim().to_ascii_lowercase().as_str() {
        "continue" | "conversation" | "conversation_continue" => {
            Some(GeneralMessageConversationAct::Continue)
        }
        "boundary" | "unsupported" | "conversation_boundary" => {
            Some(GeneralMessageConversationAct::Boundary)
        }
        "repair" | "conversation_repair" => Some(GeneralMessageConversationAct::Repair),
        "cancel" | "conversation_cancel" => Some(GeneralMessageConversationAct::Cancel),
        "clarify_continue" | "conversation_clarify_continue" => {
            Some(GeneralMessageConversationAct::ClarifyContinue)
        }
        _ => None,
    }
}

pub(super) fn fallback_general_message_plan(
    raw_text: &str,
    default_camera_hint: Option<&str>,
) -> Option<GeneralMessagePlan> {
    let normalized = normalize_command_text(raw_text);
    if normalized.is_empty() {
        return None;
    }

    if evt_long_run_request_boundary(raw_text) {
        return Some(GeneralMessagePlan {
            kind: GeneralMessagePlanKind::ConversationAct,
            conversation_act: Some(GeneralMessageConversationAct::Boundary),
            reply_text: Some(
                "4h/72h 长压测必须走 operator supervisor，我不能从微信或 WebUI 启动；我可以帮你查看 EVT 就绪状态或生成脱敏证据包。"
                    .to_string(),
            ),
            canonical_phrase: Some("长压测启动拒绝".to_string()),
            camera_hint: None,
            query: None,
            home_assistant_action: None,
            guardian_rule: None,
            event_id: None,
            corrected_summary: None,
            corrected_labels: None,
            confidence: Some(100),
            recent_clip: None,
            reason: Some("evt_long_run_requires_operator_supervisor".to_string()),
        });
    }

    if general_message_requests_capability_summary(raw_text) {
        return Some(GeneralMessagePlan {
            kind: GeneralMessagePlanKind::CapabilitySummary,
            conversation_act: None,
            reply_text: None,
            canonical_phrase: None,
            camera_hint: None,
            query: None,
            home_assistant_action: None,
            guardian_rule: None,
            event_id: None,
            corrected_summary: None,
            corrected_labels: None,
            confidence: None,
            recent_clip: None,
            reason: Some("fallback rule inferred a capability summary request".to_string()),
        });
    }

    if looks_like_vision_event_notify_request(&normalized) {
        return Some(GeneralMessagePlan {
            kind: GeneralMessagePlanKind::VisionEventNotifyLatest,
            conversation_act: None,
            reply_text: None,
            canonical_phrase: None,
            camera_hint: None,
            query: None,
            home_assistant_action: None,
            guardian_rule: None,
            event_id: None,
            corrected_summary: None,
            corrected_labels: None,
            confidence: None,
            recent_clip: None,
            reason: Some("fallback rule inferred a latest event notification request".to_string()),
        });
    }
    if looks_like_vision_event_summary_request(&normalized) {
        return Some(GeneralMessagePlan {
            kind: GeneralMessagePlanKind::VisionEventSummary,
            conversation_act: None,
            reply_text: None,
            canonical_phrase: None,
            camera_hint: None,
            query: None,
            home_assistant_action: None,
            guardian_rule: None,
            event_id: None,
            corrected_summary: None,
            corrected_labels: None,
            confidence: None,
            recent_clip: None,
            reason: Some("fallback rule inferred a recent event summary request".to_string()),
        });
    }
    if looks_like_system_readiness_request(&normalized) {
        return Some(GeneralMessagePlan {
            kind: GeneralMessagePlanKind::SystemReadiness,
            conversation_act: None,
            reply_text: None,
            canonical_phrase: None,
            camera_hint: None,
            query: None,
            home_assistant_action: None,
            guardian_rule: None,
            event_id: None,
            corrected_summary: None,
            corrected_labels: None,
            confidence: None,
            recent_clip: None,
            reason: Some("fallback rule inferred a system readiness request".to_string()),
        });
    }
    if infer_home_assistant_natural_action(raw_text).is_some() {
        return Some(GeneralMessagePlan {
            kind: GeneralMessagePlanKind::HomeAssistantServiceAction,
            conversation_act: None,
            reply_text: None,
            canonical_phrase: None,
            camera_hint: None,
            query: None,
            home_assistant_action: None,
            guardian_rule: None,
            event_id: None,
            corrected_summary: None,
            corrected_labels: None,
            confidence: None,
            recent_clip: None,
            reason: Some("fallback rule inferred a Home Assistant service action".to_string()),
        });
    }

    if !looks_like_video_search_request(&normalized)
        && matches_any(
            &normalized,
            &["录一段", "录视频", "拍视频", "录个视频", "录像"],
        )
    {
        return Some(GeneralMessagePlan {
            kind: GeneralMessagePlanKind::CameraRecordClip,
            conversation_act: None,
            reply_text: None,
            canonical_phrase: None,
            camera_hint: default_camera_hint.map(str::to_string),
            query: None,
            home_assistant_action: None,
            guardian_rule: None,
            event_id: None,
            corrected_summary: None,
            corrected_labels: None,
            confidence: None,
            recent_clip: None,
            reason: Some("fallback rule inferred a short clip request".to_string()),
        });
    }
    if matches_any(&normalized, &["抓拍", "拍照", "拍一张", "看一眼", "截一张"]) {
        return Some(GeneralMessagePlan {
            kind: GeneralMessagePlanKind::CameraSnapshot,
            conversation_act: None,
            reply_text: None,
            canonical_phrase: None,
            camera_hint: default_camera_hint.map(str::to_string),
            query: None,
            home_assistant_action: None,
            guardian_rule: None,
            event_id: None,
            corrected_summary: None,
            corrected_labels: None,
            confidence: None,
            recent_clip: None,
            reason: Some("fallback rule inferred a snapshot request".to_string()),
        });
    }
    if looks_like_rag_answer_request(&normalized) {
        return Some(GeneralMessagePlan {
            kind: GeneralMessagePlanKind::RagAnswer,
            conversation_act: None,
            reply_text: None,
            canonical_phrase: None,
            camera_hint: None,
            query: infer_query_from_raw_text(raw_text),
            home_assistant_action: None,
            guardian_rule: None,
            event_id: None,
            corrected_summary: None,
            corrected_labels: None,
            confidence: None,
            recent_clip: None,
            reason: Some("fallback rule inferred a RAG answer request".to_string()),
        });
    }
    if matches_any(
        &normalized,
        &[
            "找一下",
            "找到",
            "查一下",
            "搜索",
            "检索",
            "找照片",
            "找视频",
            "找录像",
            "找片段",
            "找回放",
            "只看视频",
            "排除图片和文档",
        ],
    ) {
        return Some(GeneralMessagePlan {
            kind: GeneralMessagePlanKind::KnowledgeSearch,
            conversation_act: None,
            reply_text: None,
            canonical_phrase: None,
            camera_hint: None,
            query: infer_query_from_raw_text(raw_text),
            home_assistant_action: None,
            guardian_rule: None,
            event_id: None,
            corrected_summary: None,
            corrected_labels: None,
            confidence: None,
            recent_clip: None,
            reason: Some("fallback rule inferred a knowledge search request".to_string()),
        });
    }
    None
}
