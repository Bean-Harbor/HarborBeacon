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
    CameraLiveView,
    CameraSnapshot,
    CameraSnapshotAndRecordClip,
    CameraRecordClip,
    CatActivityQuery,
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

pub(super) const CAT_ACTIVITY_DEFAULT_BATCH_LIMIT: usize = 3;
pub(super) const CAT_ACTIVITY_MAX_BATCH_LIMIT: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum CatActivityTimeScope {
    #[default]
    Inherit,
    Today,
    Morning,
    Afternoon,
    Evening,
    Recent,
}

impl CatActivityTimeScope {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::Today => "today",
            Self::Morning => "morning",
            Self::Afternoon => "afternoon",
            Self::Evening => "evening",
            Self::Recent => "recent",
        }
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "inherit" | "same" | "previous" => Some(Self::Inherit),
            "today" => Some(Self::Today),
            "morning" => Some(Self::Morning),
            "afternoon" => Some(Self::Afternoon),
            "evening" | "night" => Some(Self::Evening),
            "recent" | "just_now" => Some(Self::Recent),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum CatActivitySelection {
    #[default]
    Batch,
    Latest,
    Remaining,
    ResendLast,
}

impl CatActivitySelection {
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Batch => "batch",
            Self::Latest => "latest",
            Self::Remaining => "remaining",
            Self::ResendLast => "resend_last",
        }
    }

    pub(super) fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "batch" | "all" => Some(Self::Batch),
            "latest" => Some(Self::Latest),
            "remaining" | "more" => Some(Self::Remaining),
            "resend_last" | "resend" => Some(Self::ResendLast),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CatActivityQueryOptions {
    pub(super) time_scope: CatActivityTimeScope,
    pub(super) selection: CatActivitySelection,
    pub(super) limit: usize,
}

impl Default for CatActivityQueryOptions {
    fn default() -> Self {
        Self {
            time_scope: CatActivityTimeScope::Inherit,
            selection: CatActivitySelection::Batch,
            limit: CAT_ACTIVITY_DEFAULT_BATCH_LIMIT,
        }
    }
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
    pub(super) cat_activity: CatActivityQueryOptions,
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
    pub(super) cat_activity: Option<GeneralMessageCatActivityPlanPayload>,
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
pub(super) struct GeneralMessageCatActivityPlanPayload {
    #[serde(default)]
    pub(super) time_scope: Option<String>,
    #[serde(default)]
    pub(super) selection: Option<String>,
    #[serde(default)]
    pub(super) limit: Option<Value>,
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
    pub(super) explicit_live_view: bool,
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
