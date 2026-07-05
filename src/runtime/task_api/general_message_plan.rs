//! General-message plan data structures shared by Task API action modules.

use serde::Deserialize;
use serde_json::Value;

use crate::runtime::task_session::RecentClipPlaybackState;

use super::HomeAssistantNaturalAction;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GeneralMessagePlanKind {
    CapabilitySummary,
    Clarify,
    ConversationAct,
    CameraReplayRecentClip,
    CameraSnapshot,
    CameraRecordClip,
    KnowledgeSearch,
    RagAnswer,
    HomeAssistantServiceAction,
    VisionEventSummary,
    VisionEventNotifyLatest,
    VlmDescribeLatestEvent,
    VlmDescribeEvent,
    FamilyMemorySummary,
    SystemReadiness,
    EvtReadiness,
    EvtPreflight,
    EvtEvidenceBundle,
    FamilyTimelineSummary,
    FamilyTimelineQuery,
    GuardianRuleProposal,
    GuardianRuleList,
    GuardianRuleEnable,
    GuardianRulePause,
    GuardianStatus,
    FamilyMemoryConfirm,
    FamilyMemoryFavorite,
    FamilyMemoryHide,
    FamilyMemoryCorrectSummary,
    FamilyMemoryCorrectLabels,
    FamilyMemoryShowFavorites,
    #[allow(dead_code)]
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GeneralMessageConversationAct {
    Continue,
    Boundary,
    Repair,
    Cancel,
    ClarifyContinue,
}

impl GeneralMessageConversationAct {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::Boundary => "boundary",
            Self::Repair => "repair",
            Self::Cancel => "cancel",
            Self::ClarifyContinue => "clarify_continue",
        }
    }

    pub(super) fn reply_pack_kind(self) -> &'static str {
        match self {
            Self::Continue => "conversation_continue",
            Self::Boundary => "conversation_boundary",
            Self::Repair => "conversation_repair",
            Self::Cancel => "conversation_cancel",
            Self::ClarifyContinue => "conversation_clarify_continue",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GeneralMessagePlan {
    pub(super) kind: GeneralMessagePlanKind,
    pub(super) conversation_act: Option<GeneralMessageConversationAct>,
    pub(super) reply_text: Option<String>,
    pub(super) canonical_phrase: Option<String>,
    pub(super) camera_hint: Option<String>,
    pub(super) query: Option<String>,
    pub(super) home_assistant_action: Option<HomeAssistantNaturalAction>,
    pub(super) guardian_rule: Option<Value>,
    pub(super) event_id: Option<String>,
    pub(super) corrected_summary: Option<String>,
    pub(super) corrected_labels: Option<Vec<String>>,
    pub(super) confidence: Option<u8>,
    pub(super) recent_clip: Option<RecentClipPlaybackState>,
    pub(super) reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Default)]
pub(super) struct GeneralMessagePlanPayload {
    #[serde(default)]
    pub(super) decision: String,
    #[serde(default)]
    pub(super) action: String,
    #[serde(default)]
    pub(super) conversation_act: Option<String>,
    #[serde(default)]
    pub(super) reply_text: Option<String>,
    #[serde(default)]
    pub(super) canonical_phrase: Option<String>,
    #[serde(default)]
    pub(super) confidence: Option<Value>,
    #[serde(default)]
    pub(super) camera_hint: Option<String>,
    #[serde(default)]
    pub(super) query: Option<String>,
    #[serde(default)]
    pub(super) domain: Option<String>,
    #[serde(default)]
    pub(super) service: Option<String>,
    #[serde(default)]
    pub(super) entity_hint: Option<String>,
    #[serde(default)]
    pub(super) home_assistant: Option<GeneralMessageHomeAssistantPlanPayload>,
    #[serde(default)]
    pub(super) ha: Option<GeneralMessageHomeAssistantPlanPayload>,
    #[serde(default)]
    pub(super) guardian_rule: Option<Value>,
    #[serde(default)]
    pub(super) event_id: Option<String>,
    #[serde(default)]
    pub(super) corrected_summary: Option<String>,
    #[serde(default)]
    pub(super) corrected_labels: Option<Vec<String>>,
    #[serde(default)]
    pub(super) reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Default)]
pub(super) struct GeneralMessageHomeAssistantPlanPayload {
    #[serde(default)]
    pub(super) domain: Option<String>,
    #[serde(default)]
    pub(super) service: Option<String>,
    #[serde(default)]
    pub(super) entity_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct GeneralMessageSignals {
    pub(super) normalized: String,
    pub(super) asks_capability: bool,
    pub(super) explicit_clip_playback: bool,
    pub(super) explicit_snapshot: bool,
    pub(super) explicit_clip: bool,
    pub(super) explicit_search: bool,
    pub(super) explicit_rag_answer: bool,
    pub(super) explicit_ha_action: bool,
    pub(super) explicit_event_summary: bool,
    pub(super) explicit_event_notify: bool,
    pub(super) explicit_system_readiness: bool,
    pub(super) mentions_camera_context: bool,
    pub(super) ambiguous_visual_request: bool,
    pub(super) recent_camera_context: bool,
    pub(super) recent_clip_available: bool,
    pub(super) recent_search_context: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct GeneralMessageCandidate {
    pub(super) kind: GeneralMessagePlanKind,
    pub(super) confidence: u8,
    pub(super) camera_hint: Option<String>,
    pub(super) query: Option<String>,
    pub(super) recent_clip: Option<RecentClipPlaybackState>,
    pub(super) reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct GeneralMessageWorkflowCompilerShadowTrace {
    pub(super) workflow_id: Option<String>,
    pub(super) node_id: Option<String>,
    pub(super) candidate_kind: Option<String>,
    pub(super) confidence: Option<u8>,
    pub(super) matched_current_plan: bool,
    pub(super) read_only: bool,
    pub(super) unsafe_action: bool,
    pub(super) reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct GeneralMessageControllerTrace {
    pub(super) controller_stage: String,
    pub(super) router_llm: bool,
    pub(super) router_latency_ms: Option<u64>,
    pub(super) renderer_latency_ms: Option<u64>,
    pub(super) fallback_reason: Option<String>,
    pub(super) candidate_count: usize,
    pub(super) nsp_schema_valid: bool,
    pub(super) nsp_local_only: bool,
    pub(super) nsp_decision: Option<String>,
    pub(super) nsp_confidence: Option<u8>,
    pub(super) nsp_canonical_phrase: Option<String>,
    pub(super) workflow_compiler_shadow: Option<GeneralMessageWorkflowCompilerShadowTrace>,
}
