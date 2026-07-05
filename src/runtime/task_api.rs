//! Minimal Assistant Task API service for HarborBeacon integration.

mod evt_actions;
mod family_guardian_actions;
mod general_message_plan;
mod general_message_router;
mod home_assistant_actions;
mod system_readiness_actions;
mod vision_event_actions;

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use self::general_message_plan::{
    GeneralMessageCandidate, GeneralMessageControllerTrace, GeneralMessageConversationAct,
    GeneralMessagePlan, GeneralMessagePlanKind, GeneralMessagePlanPayload, GeneralMessageSignals,
    GeneralMessageWorkflowCompilerShadowTrace,
};
#[cfg(test)]
use self::general_message_router::general_message_requests_capability_summary;
use self::general_message_router::{
    build_general_message_candidates, build_general_message_renderer_prompt,
    build_general_message_renderer_system_prompt, build_general_message_router_prompt,
    build_general_message_router_system_prompt, deterministic_stage_for_plan,
    enforce_general_message_nsp_hard_guards, extract_general_message_signals,
    fallback_general_message_plan, general_message_conversation_summary,
    general_message_default_clarification_prompt, general_message_nsp_schema_valid,
    general_message_nsp_validation_failure, general_message_plan_decision_label,
    general_message_support_summary_for_request, general_message_supported_examples,
    general_message_unsupported_summary, infer_general_message_conversation_act,
    parse_general_message_plan, parse_general_message_router_decision,
    record_general_message_nsp_plan_trace, resolve_deterministic_general_message_plan,
    should_try_general_message_router_llm,
};
#[cfg(test)]
use self::system_readiness_actions::build_general_message_readiness_summary;
#[cfg(test)]
use self::vision_event_actions::build_redacted_vision_event_summary;

use crate::adapters::rtsp::CommandRtspAdapter;
use crate::connectors::home_assistant::{
    normalize_home_assistant_service_action_request,
    validate_home_assistant_service_action_request, HomeAssistantClient, HomeAssistantClientConfig,
    HomeAssistantEntity, HomeAssistantServiceActionRequest,
};
use crate::connectors::notifications::{
    NotificationAttachment, NotificationAttachmentKind, NotificationContent, NotificationDelivery,
    NotificationDeliveryError, NotificationDeliveryMode, NotificationDeliveryRecord,
    NotificationDeliveryService, NotificationDestination, NotificationDestinationKind,
    NotificationGatewayConfig, NotificationMetadata, NotificationPayloadFormat,
    NotificationRequest, NotificationSource,
};
#[cfg(test)]
use crate::connectors::notifications::{NotificationRecipient, NotificationRecipientIdType};
use crate::connectors::storage::StorageTarget;
use crate::control_plane::approvals::{ApprovalStatus, ApprovalTicket};
use crate::control_plane::audit::{build_metadata_audit_record, AuditActorKind};
use crate::control_plane::events::{EventRecord, EventSeverity, EventSourceKind};
use crate::control_plane::media::{
    MediaAsset, MediaAssetKind, MediaDeliveryMode, MediaSession, MediaSessionKind,
    MediaSessionStatus, RecordingPolicy, ShareAccessScope, ShareLink, StorageTargetKind,
};
use crate::control_plane::models::{
    ModelEndpointKind, ModelEndpointStatus, ModelKind, PrivacyLevel,
};
use crate::control_plane::tasks::{
    ArtifactKind, ArtifactRecord, ConversationSession, ExecutionRoute, TaskRun, TaskRunStatus,
    TaskStepRun, TaskStepRunStatus,
};
use crate::domains::knowledge::{DOMAIN as KNOWLEDGE_DOMAIN, OP_SEARCH as KNOWLEDGE_OP_SEARCH};
use crate::domains::vision::OP_ANALYZE_CAMERA;
use crate::orchestrator::approval::{ApprovalManager, AutonomyConfig, AutonomyLevel};
use crate::orchestrator::contracts::{Action, ExecutionResult, RiskLevel, StepStatus};
use crate::orchestrator::executors::harbor_ops::{register_harbor_executors, HarborExecutorConfig};
use crate::orchestrator::executors::vision::VisionExecutor;
use crate::orchestrator::policy::{
    action_requires_approval, apply_governance_defaults, effective_risk_level, enforce,
    ApprovalContext,
};
use crate::orchestrator::router::{Executor, Router};
use crate::orchestrator::workflow_compiler::{
    compile_system_diagnostics_candidate, WorkflowCandidate,
};
use crate::runtime::admin_console::{
    harboros_writable_root, AdminConsoleState, AdminConsoleStore, AdminModelCenterState,
    NotificationTargetRecord, RagResourceProfile,
};
#[cfg(test)]
use crate::runtime::admin_console::{resolved_identity_binding_records, IdentityBindingRecord};
use crate::runtime::hub::{
    looks_like_auth_error, CameraConnectRequest, CameraHubService, HubScanRequest,
    HubScanResultItem,
};
use crate::runtime::knowledge::{
    KnowledgeSearchCitation, KnowledgeSearchRequest, KnowledgeSearchResponse,
    KnowledgeSearchService,
};
use crate::runtime::media::{ClipCaptureRequest, ClipCaptureResult, SnapshotCaptureResult};
use crate::runtime::model_center::{
    run_llm_text_with_state_and_options, run_ocr_with_state, run_vlm_summary_with_state,
    LlmTextExecution, LlmTextOptions,
};
use crate::runtime::privacy_gateway::{PrivacyGateway, PrivacyGatewayEvaluation};
use crate::runtime::registry::ResolvedCameraTarget;
use crate::runtime::remote_view;
use crate::runtime::task_session::{
    session_state_value_from_conversation, PendingTaskCandidate, PendingTaskClipConfirmation,
    PendingTaskConnect, PendingTaskGeneralMessageLoop, PendingTaskHomeAssistantCandidate,
    PendingTaskHomeAssistantClarification, RecentClipPlaybackState, RecentFamilyMemoryEventState,
    TaskConversationState, TaskConversationStore,
};
use crate::runtime::vision_event::StoredLocalVisionEvent;
const ALLOW_NON_HARBOROS_CAPTURE_ROOT_ENV: &str = "HARBOR_ALLOW_NON_HARBOROS_CAPTURE_ROOT";
const GENERAL_MESSAGE_RECAP_LIMIT: usize = 3;
const GENERAL_MESSAGE_TURN_BUDGET_MS: u64 = 24_000;
const GENERAL_MESSAGE_ROUTER_BUDGET_MS: u64 = 12_000;
const GENERAL_MESSAGE_ROUTER_MAX_TOKENS: u32 = 220;
const GENERAL_MESSAGE_NSP_MIN_CONFIDENCE: u8 = 60;
const GENERAL_MESSAGE_RENDERER_BUDGET_MS: u64 = 1_800;
const GENERAL_MESSAGE_RENDERER_MAX_TOKENS: u32 = 48;
const RAG_DOMAIN: &str = "rag";
const RAG_OP_ANSWER: &str = "answer";
const RAG_ANSWER_CONTEXT_LIMIT: usize = 5;
const RAG_ANSWER_BUDGET_MS: u64 = 6_000;
const RAG_ANSWER_MAX_TOKENS: u32 = 256;
const RAG_ANSWER_BUDGET_MS_ENV: &str = "HARBOR_RAG_ANSWER_BUDGET_MS";
const RAG_ANSWER_MAX_TOKENS_ENV: &str = "HARBOR_RAG_ANSWER_MAX_TOKENS";
const RECENT_CLIP_PLAYBACK_WINDOW_MS: u128 = 15 * 60 * 1000;
const RECENT_FAMILY_MEMORY_EVENT_WINDOW_MS: u128 = 24 * 60 * 60 * 1000;
const DEFAULT_TURN_INTENT_DOMAIN: &str = "general";
const DEFAULT_TURN_INTENT_ACTION: &str = "message";
const CONTINUATION_TOKEN_KEY: &str = "continuation_token";
const CONTINUATION_TOKEN_POINTER: &str = "/continuation_token";
const LEGACY_RESUME_TOKEN_KEY: &str = "resume_token";
const LEGACY_RESUME_TOKEN_POINTER: &str = concat!("/", "resume_token");

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TaskSource {
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub surface: String,
    #[serde(default)]
    pub conversation_id: String,
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub route_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TaskMessageMention {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TaskMessageAttachmentDownloadAuth {
    #[serde(rename = "type", default)]
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TaskMessageAttachmentDownload {
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub headers: Value,
    #[serde(default)]
    pub auth: Option<TaskMessageAttachmentDownloadAuth>,
    #[serde(default)]
    pub expires_at: String,
    #[serde(default)]
    pub max_size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TaskMessageAttachment {
    #[serde(default)]
    pub attachment_id: String,
    #[serde(rename = "type", default)]
    pub attachment_type: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub mime_type: String,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    #[serde(default)]
    pub download: Option<TaskMessageAttachmentDownload>,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TaskMessage {
    #[serde(default)]
    pub message_id: String,
    #[serde(default)]
    pub chat_type: String,
    #[serde(default)]
    pub mentions: Vec<TaskMessageMention>,
    #[serde(default)]
    pub attachments: Vec<TaskMessageAttachment>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TaskIntent {
    #[serde(default)]
    pub domain: String,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub raw_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskAutonomy {
    #[serde(default = "default_task_autonomy_level")]
    pub level: String,
}

impl Default for TaskAutonomy {
    fn default() -> Self {
        Self {
            level: default_task_autonomy_level(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TaskRequest {
    #[serde(default = "new_task_id")]
    pub task_id: String,
    #[serde(default)]
    pub trace_id: String,
    #[serde(default)]
    pub step_id: String,
    #[serde(default)]
    pub source: TaskSource,
    #[serde(default)]
    pub intent: TaskIntent,
    #[serde(default)]
    pub entity_refs: Value,
    #[serde(default)]
    pub args: Value,
    #[serde(default)]
    pub autonomy: TaskAutonomy,
    #[serde(default)]
    pub message: Option<TaskMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TaskTurnBlock {
    #[serde(default = "new_turn_id")]
    pub turn_id: String,
    #[serde(default)]
    pub trace_id: String,
    #[serde(default)]
    pub occurred_at: String,
    #[serde(default)]
    pub retry_of: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TaskTurnActor {
    #[serde(default)]
    pub user_id: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub account_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TaskTurnConversation {
    #[serde(default)]
    pub handle: Option<String>,
    #[serde(default)]
    pub channel: String,
    #[serde(default)]
    pub surface: String,
    #[serde(default)]
    pub thread_id: String,
    #[serde(default)]
    pub chat_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TaskTurnTransport {
    #[serde(default)]
    pub route_key: String,
    #[serde(default)]
    pub message_id: String,
    #[serde(default)]
    pub capabilities: Value,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TaskTurnInput {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub parts: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TaskTurnContinuation {
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub frame_id: String,
    #[serde(default)]
    pub reply_to_turn_id: String,
    #[serde(default)]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TaskTurnEnvelope {
    #[serde(default)]
    pub turn: TaskTurnBlock,
    #[serde(default)]
    pub actor: TaskTurnActor,
    #[serde(default)]
    pub conversation: TaskTurnConversation,
    #[serde(default)]
    pub transport: TaskTurnTransport,
    #[serde(default)]
    pub input: TaskTurnInput,
    #[serde(default)]
    pub continuation: Option<TaskTurnContinuation>,
    #[serde(default)]
    pub autonomy: TaskAutonomy,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Completed,
    NeedsInput,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TaskArtifact {
    pub kind: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub mime_type: String,
    #[serde(default)]
    pub media_asset_id: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TaskResultEnvelope {
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub data: Value,
    #[serde(default)]
    pub artifacts: Vec<TaskArtifact>,
    #[serde(default)]
    pub events: Vec<Value>,
    #[serde(default)]
    pub next_actions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskResponse {
    pub task_id: String,
    pub trace_id: String,
    pub status: TaskStatus,
    pub executor_used: String,
    pub risk_level: RiskLevel,
    #[serde(default)]
    pub result: TaskResultEnvelope,
    pub audit_ref: String,
    #[serde(default)]
    pub missing_fields: Vec<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub resume_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskTurnStatusBlock {
    pub turn_id: String,
    pub trace_id: String,
    pub status: TaskStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskTurnConversationResponse {
    pub handle: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ActiveDialogueFrame {
    pub frame_id: String,
    pub kind: String,
    pub state: String,
    #[serde(default)]
    pub expected_reply: Vec<String>,
    pub continuation_token: String,
    #[serde(default)]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskTurnReply {
    pub kind: String,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskDeliveryHint {
    pub kind: String,
    #[serde(default)]
    pub artifact_id: Option<String>,
    #[serde(default)]
    pub fallback: Option<String>,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskTurnResponse {
    pub turn: TaskTurnStatusBlock,
    pub conversation: TaskTurnConversationResponse,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_frame: Option<ActiveDialogueFrame>,
    pub reply: TaskTurnReply,
    #[serde(default)]
    pub artifacts: Vec<TaskArtifact>,
    #[serde(default)]
    pub delivery_hints: Vec<TaskDeliveryHint>,
    #[serde(default)]
    pub observability: Value,
    #[serde(default)]
    pub error: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskApprovalSummary {
    pub approval_ticket: ApprovalTicket,
    pub source_channel: String,
    pub surface: String,
    pub conversation_id: String,
    pub user_id: String,
    pub session_id: String,
    pub domain: String,
    pub action: String,
    pub intent_text: String,
    pub autonomy_level: String,
    pub risk_level: RiskLevel,
}

#[derive(Debug, Clone, PartialEq)]
pub enum TaskRequestAcceptance {
    Accept,
    Replay(TaskResponse),
    Conflict(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum TaskTurnRequestAcceptance {
    Accept,
    Replay(TaskTurnResponse),
    Conflict(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClipConfirmationReplyDecision {
    Deliver,
    Decline,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActiveFrameDecision {
    Preserve,
    Supersede,
    Deliver,
    Cancel,
}

#[derive(Debug, Clone)]
pub struct TaskApiService {
    admin_store: AdminConsoleStore,
    conversation_store: TaskConversationStore,
    last_evt_preflight: Arc<Mutex<Option<Value>>>,
}

#[derive(Debug, Clone)]
struct TaskRuntimeTracking {
    session_id: String,
    step_id: String,
    started_at: String,
}

#[derive(Debug, Clone)]
struct NotificationDeliveryOutcome {
    event_type: &'static str,
    severity: EventSeverity,
    payload: Value,
}

impl TaskApiService {
    pub fn new(admin_store: AdminConsoleStore, conversation_store: TaskConversationStore) -> Self {
        Self {
            admin_store,
            conversation_store,
            last_evt_preflight: Arc::new(Mutex::new(None)),
        }
    }

    pub fn conversation_store(&self) -> &TaskConversationStore {
        &self.conversation_store
    }

    fn record_last_evt_preflight(&self, preflight: &Value) {
        let Ok(mut guard) = self.last_evt_preflight.lock() else {
            return;
        };
        *guard = Some(preflight.clone());
    }

    fn last_evt_preflight(&self) -> Option<Value> {
        self.last_evt_preflight
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    }

    pub fn accept_or_replay_task(
        &self,
        request: &TaskRequest,
    ) -> Result<TaskRequestAcceptance, String> {
        if request.task_id.trim().is_empty() {
            return Ok(TaskRequestAcceptance::Accept);
        }

        let Some(task_run) = self.conversation_store.load_task_run(&request.task_id)? else {
            return Ok(TaskRequestAcceptance::Accept);
        };

        let incoming_identity = task_request_identity(request);
        let existing_identity = persisted_task_request_identity(&task_run);
        if existing_identity != incoming_identity {
            return Ok(TaskRequestAcceptance::Conflict(
                "task_id already exists with a different request identity".to_string(),
            ));
        }

        Ok(TaskRequestAcceptance::Replay(
            self.replay_task_response(&task_run)?,
        ))
    }

    pub fn accept_or_replay_turn(
        &self,
        envelope: &TaskTurnEnvelope,
    ) -> Result<TaskTurnRequestAcceptance, String> {
        let request = task_request_from_turn_envelope(envelope);
        match self.accept_or_replay_task(&request)? {
            TaskRequestAcceptance::Accept => Ok(TaskTurnRequestAcceptance::Accept),
            TaskRequestAcceptance::Conflict(message) => {
                Ok(TaskTurnRequestAcceptance::Conflict(message))
            }
            TaskRequestAcceptance::Replay(response) => Ok(TaskTurnRequestAcceptance::Replay(
                turn_response_from_task_response(envelope, &request, response),
            )),
        }
    }

    pub fn pending_approvals(&self) -> Result<Vec<TaskApprovalSummary>, String> {
        self.conversation_store
            .pending_approvals()?
            .into_iter()
            .map(|approval| self.load_approval_summary(&approval))
            .collect()
    }

    pub fn approve_pending_approval(
        &self,
        approval_id: &str,
        approver_user_id: Option<String>,
    ) -> Result<(TaskApprovalSummary, TaskResponse), String> {
        let (approval, task_run, session) = self.load_approval_context(approval_id)?;
        if approval.status != ApprovalStatus::Pending {
            return Err(format!("approval is not pending: {}", approval.approval_id));
        }

        let request = self.build_approval_resume_request(
            &approval,
            &task_run,
            session.as_ref(),
            approver_user_id.clone(),
        );
        let response = self.handle_task(request);

        let updated_approval = self
            .conversation_store
            .load_approval(approval_id)?
            .unwrap_or(approval.clone());
        self.record_approval_decision_event(
            &updated_approval,
            &task_run,
            session.as_ref(),
            "task.approval_approved",
            EventSeverity::Info,
            approver_user_id,
        )?;
        Ok((self.load_approval_summary(&updated_approval)?, response))
    }

    pub fn reject_pending_approval(
        &self,
        approval_id: &str,
        approver_user_id: Option<String>,
    ) -> Result<TaskApprovalSummary, String> {
        let (approval, mut task_run, session) = self.load_approval_context(approval_id)?;
        if approval.status != ApprovalStatus::Pending {
            return Err(format!("approval is not pending: {}", approval.approval_id));
        }

        let decided_at = Some(current_timestamp());
        let updated_approval = self
            .conversation_store
            .update_approval_status(
                approval_id,
                ApprovalStatus::Rejected,
                approver_user_id.clone(),
                decided_at.clone(),
            )?
            .ok_or_else(|| format!("approval not found: {approval_id}"))?;

        task_run.status = TaskRunStatus::Failed;
        task_run.completed_at = decided_at;
        self.conversation_store.save_task_run(&task_run)?;

        if let Some(mut session) = session.clone() {
            session.resume_token = None;
            self.conversation_store.save_session(&session)?;
        }

        self.record_approval_decision_event(
            &updated_approval,
            &task_run,
            session.as_ref(),
            "task.approval_rejected",
            EventSeverity::Warning,
            approver_user_id,
        )?;
        Ok(self.load_approval_summary(&updated_approval)?)
    }

    pub fn handle_task(&self, mut request: TaskRequest) -> TaskResponse {
        if request.task_id.trim().is_empty() {
            request.task_id = new_task_id();
        }
        if request.trace_id.trim().is_empty() {
            request.trace_id = request.task_id.clone();
        }
        let _ = self.admin_store.record_member_interactive_surface(
            &request.source.user_id,
            &request.source.channel,
            Some(&request.source.route_key),
        );
        let tracking = self.begin_task_tracking(&request);

        let mut response = match (
            request.intent.domain.trim().to_lowercase(),
            request.intent.action.trim().to_lowercase(),
        ) {
            (domain, action) if domain == KNOWLEDGE_DOMAIN && action == KNOWLEDGE_OP_SEARCH => {
                self.handle_knowledge_search(&request)
            }
            (domain, action)
                if (domain == RAG_DOMAIN && action == RAG_OP_ANSWER) || action == "rag.answer" =>
            {
                self.handle_rag_answer(&request)
            }
            (domain, action) if domain == "general" && action == "message" => {
                self.handle_general_message(&request)
            }
            (domain, action) if is_supported_harbor_task(&domain, &action) => {
                self.handle_harbor_system_action(&request)
            }
            (domain, action) if domain == "camera" && action == "scan" => {
                self.handle_camera_scan(&request)
            }
            (domain, action) if domain == "camera" && action == "connect" => {
                self.handle_camera_connect(&request)
            }
            (domain, action) if domain == "camera" && action == "snapshot" => {
                self.handle_camera_snapshot(&request)
            }
            (domain, action)
                if domain == "camera" && (action == "share_link" || action == "live_view") =>
            {
                self.handle_camera_share_link(&request)
            }
            (domain, action) if domain == "camera" && action == "analyze" => {
                self.handle_camera_analyze(&request)
            }
            (domain, action) => self.failed(
                &request,
                "task_api",
                RiskLevel::Low,
                format!("unsupported task action: {domain}.{action}"),
            ),
        };
        self.append_task_lifecycle_event(&request, &tracking, &mut response);
        let _ = self.finish_task_tracking(&request, &response, &tracking);
        response
    }

    pub fn handle_turn(&self, envelope: TaskTurnEnvelope) -> TaskTurnResponse {
        let request = task_request_from_turn_envelope(&envelope);
        let response = self.handle_task(request.clone());
        turn_response_from_task_response(&envelope, &request, response)
    }

    fn replay_task_response(&self, task_run: &TaskRun) -> Result<TaskResponse, String> {
        let trace_id = task_run
            .metadata
            .pointer("/trace_id")
            .and_then(Value::as_str)
            .map(|value| value.to_string())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| task_run.task_id.clone());
        let step_id = task_run
            .metadata
            .pointer("/step_id")
            .and_then(Value::as_str)
            .map(|value| value.to_string())
            .filter(|value| !value.trim().is_empty());
        let task_step = step_id
            .as_deref()
            .map(|value| self.conversation_store.load_task_step(value))
            .transpose()?
            .flatten();

        let artifacts = self
            .conversation_store
            .artifacts_for_task(&task_run.task_id)?
            .into_iter()
            .filter(|artifact| {
                step_id.is_none() || artifact.step_id.as_deref() == step_id.as_deref()
            })
            .map(task_artifact_from_record)
            .collect::<Vec<_>>();
        let events = self
            .conversation_store
            .events_for_task(&task_run.task_id)?
            .into_iter()
            .filter(|event| {
                step_id.is_none() || event.causation_id.as_deref() == step_id.as_deref()
            })
            .map(|event| serde_json::to_value(event).unwrap_or(Value::Null))
            .collect::<Vec<_>>();

        let (executor_used, audit_ref, output_payload) = if let Some(task_step) = task_step {
            (
                task_step.executor_used,
                task_step.audit_ref.unwrap_or_default(),
                task_step.output_payload,
            )
        } else {
            ("task_api_dispatch".to_string(), String::new(), Value::Null)
        };

        Ok(TaskResponse {
            task_id: task_run.task_id.clone(),
            trace_id,
            status: task_status_from_task_run_status(task_run.status),
            executor_used,
            risk_level: task_run.risk_level,
            result: TaskResultEnvelope {
                message: string_at_paths(&output_payload, &["/message"]).unwrap_or_default(),
                data: output_payload
                    .pointer("/data")
                    .cloned()
                    .unwrap_or_else(|| json!({})),
                artifacts,
                events,
                next_actions: string_vec_at_paths(&output_payload, &["/next_actions"]),
            },
            audit_ref,
            missing_fields: string_vec_at_paths(&output_payload, &["/missing_fields"]),
            prompt: string_at_paths(&output_payload, &["/prompt"]),
            resume_token: string_at_paths(
                &output_payload,
                &[CONTINUATION_TOKEN_POINTER, LEGACY_RESUME_TOKEN_POINTER],
            ),
        })
    }

    fn handle_harbor_system_action(&self, request: &TaskRequest) -> TaskResponse {
        let action = match build_harbor_action_from_request(request) {
            Ok(action) => action,
            Err(error) => {
                return self.failed(
                    request,
                    "harboros_router",
                    expected_risk_level(request),
                    error,
                );
            }
        };

        if let Err(response) = self.ensure_action_allowed(request, &action, "harboros_router") {
            return response;
        }

        let mut router = Router::new();
        if let Err(error) =
            register_harbor_executors(&mut router, &HarborExecutorConfig::from_env())
        {
            let error_message = error.clone();
            let data = json!({
                "domain": action.domain.clone(),
                "operation": action.operation.clone(),
                "resource": action.resource.clone(),
                "executor_used": "harboros_router",
                "route_fallback_used": false,
                "error_code": "EXECUTOR_CONFIG_ERROR",
                "error_message": error_message,
            });
            let event = self.serialize_event_record(&build_task_event_record(
                request,
                &step_id_for_request(request),
                "task.harboros_failed",
                EventSeverity::Error,
                data.clone(),
            ));
            return self.failed_with_context(
                request,
                "harboros_router",
                action.risk_level,
                format!("HarborOS executor configuration error: {error}"),
                data,
                vec![event],
            );
        }

        let execution = router.execute(&action, &request.task_id, &step_id_for_request(request));
        self.harbor_response_from_execution(request, &action, execution)
    }

    fn harbor_response_from_execution(
        &self,
        request: &TaskRequest,
        action: &Action,
        execution: ExecutionResult,
    ) -> TaskResponse {
        let preview = harbor_execution_is_preview(&execution.result_payload);
        let data = json!({
            "domain": action.domain.clone(),
            "operation": action.operation.clone(),
            "resource": action.resource.clone(),
            "executor_used": execution.executor_used.clone(),
            "route_fallback_used": execution.fallback_used,
            "duration_ms": execution.duration_ms,
            "preview": preview,
            "result": execution.result_payload.clone(),
            "error_code": execution.error_code.clone(),
            "error_message": execution.error_message.clone(),
        });
        let (status, event_type, severity, message) = if execution.ok() {
            (
                TaskStatus::Completed,
                "task.harboros_dispatched",
                EventSeverity::Info,
                format!(
                    "HarborOS {}.{} 已通过 {} 执行",
                    action.domain, action.operation, execution.executor_used
                ),
            )
        } else {
            (
                TaskStatus::Failed,
                "task.harboros_failed",
                EventSeverity::Error,
                format!(
                    "HarborOS {}.{} 执行失败: {}",
                    action.domain,
                    action.operation,
                    execution
                        .error_message
                        .clone()
                        .unwrap_or_else(|| "unknown error".to_string())
                ),
            )
        };
        let event = self.serialize_event_record(&build_task_event_record(
            request,
            &step_id_for_request(request),
            event_type,
            severity,
            data.clone(),
        ));

        TaskResponse {
            task_id: request.task_id.clone(),
            trace_id: request.trace_id.clone(),
            status,
            executor_used: execution.executor_used.clone(),
            risk_level: action.risk_level,
            result: TaskResultEnvelope {
                message,
                data,
                artifacts: Vec::new(),
                events: vec![event],
                next_actions: Vec::new(),
            },
            audit_ref: non_empty_audit_ref(&execution.audit_ref),
            missing_fields: Vec::new(),
            prompt: None,
            resume_token: None,
        }
    }

    fn handle_camera_scan(&self, request: &TaskRequest) -> TaskResponse {
        let hub = self.hub();
        let scan_request = HubScanRequest {
            cidr: string_at_paths(&request.args, &["/cidr"]),
            protocol: protocol_string(&request.args),
            rtsp_port: request
                .args
                .get("rtsp_port")
                .and_then(Value::as_u64)
                .and_then(|value| u16::try_from(value).ok()),
            rtsp_username: string_at_paths(&request.args, &["/rtsp_username"]),
            rtsp_password: string_at_paths(&request.args, &["/rtsp_password"]),
        };
        let action = apply_governance_defaults(Action {
            domain: "camera".to_string(),
            operation: "scan".to_string(),
            resource: json!({
                "workspace_id": workspace_id_for_request(request),
            }),
            args: json!({
                "cidr": scan_request.cidr.clone(),
                "protocol": scan_request.protocol.clone(),
                "rtsp_port": scan_request.rtsp_port,
                "rtsp_username": scan_request.rtsp_username.clone(),
            }),
            risk_level: RiskLevel::Low,
            requires_approval: request_requires_approval(request),
            dry_run: false,
        });
        if let Err(response) = self.ensure_action_allowed(request, &action, "camera_hub_service") {
            return response;
        }

        match hub.scan(scan_request, None) {
            Ok(summary) => {
                let pending_candidates = pending_candidates_from_results(&summary.results);
                let mut conversation = self.load_or_create_conversation(request);
                conversation.set_camera_pending_candidates(pending_candidates.clone());
                conversation.set_camera_pending_connect(None);
                conversation.last_scan_cidr = summary.defaults.cidr.clone();
                let _ = self.save_conversation(request, &conversation);

                let message = format_scan_message(
                    &summary.defaults.cidr,
                    &summary.results,
                    &pending_candidates,
                    summary.devices.len(),
                );
                let next_actions = if pending_candidates.is_empty() {
                    vec!["分析客厅摄像头".to_string()]
                } else {
                    vec!["接入 1".to_string(), "密码 xxxxxx".to_string()]
                };
                self.completed(
                    request,
                    "camera_hub_service",
                    RiskLevel::Low,
                    message,
                    json!({
                        "summary": {
                            "scanned_hosts": summary.scanned_hosts,
                            "devices": summary.devices.len(),
                            "results": summary.results.len(),
                        },
                        "candidates": summary.results,
                    }),
                    Vec::new(),
                    next_actions,
                )
            }
            Err(error) => self.failed(request, "camera_hub_service", RiskLevel::Low, error),
        }
    }

    fn handle_camera_connect(&self, request: &TaskRequest) -> TaskResponse {
        let action = apply_governance_defaults(Action {
            domain: "camera".to_string(),
            operation: "connect".to_string(),
            resource: json!({
                "candidate_index": usize_at_paths(&request.entity_refs, &["/candidate_index"])
                    .or_else(|| usize_at_paths(&request.args, &["/candidate_index"])),
                "ip": first_string(&[&request.entity_refs, &request.args], &["/ip"]),
                "continuation_token": continuation_token_from_request(request),
            }),
            args: request.args.clone(),
            risk_level: RiskLevel::Low,
            requires_approval: request_requires_approval(request),
            dry_run: false,
        });
        if let Err(response) = self.ensure_action_allowed(request, &action, "camera_hub_service") {
            return response;
        }

        if let Some(resume_token) = continuation_token_from_request(request) {
            return self.resume_camera_connect(request, &resume_token);
        }

        if let Some(index) = usize_at_paths(&request.entity_refs, &["/candidate_index"]) {
            return self.connect_camera_candidate(request, index);
        }
        if let Some(index) = usize_at_paths(&request.args, &["/candidate_index"]) {
            return self.connect_camera_candidate(request, index);
        }

        self.connect_camera_direct(request)
    }

    fn connect_camera_candidate(&self, request: &TaskRequest, index: usize) -> TaskResponse {
        let mut conversation = self.load_or_create_conversation(request);
        let pending_candidates = conversation.camera_pending_candidates();
        if pending_candidates.is_empty() {
            return self.failed(
                request,
                "camera_hub_service",
                RiskLevel::Medium,
                "当前没有可继续的候选设备列表，请先发送“扫描摄像头”。".to_string(),
            );
        }

        if index == 0 || index > pending_candidates.len() {
            return self.failed(
                request,
                "camera_hub_service",
                RiskLevel::Medium,
                "当前没有这个序号的候选设备，请先发送“扫描摄像头”刷新列表。".to_string(),
            );
        }

        let candidate = pending_candidates[index - 1].clone();
        let connect_request = candidate_to_connect_request(&candidate, None);
        match self.hub().manual_add(connect_request, None) {
            Ok(summary) => {
                conversation.set_camera_pending_connect(None);
                conversation.retain_camera_pending_candidates(|item| {
                    item.candidate_id != candidate.candidate_id
                });
                let _ = self.save_conversation(request, &conversation);
                self.completed(
                    request,
                    "camera_hub_service",
                    RiskLevel::Medium,
                    format!(
                        "已接入 {}（{}），设备库现在共有 {} 台。",
                        candidate.name,
                        candidate.ip,
                        summary.devices.len()
                    ),
                    json!({
                        "device": summary.device,
                        "devices": summary.devices.len(),
                    }),
                    Vec::new(),
                    vec!["分析客厅摄像头".to_string()],
                )
            }
            Err(error) if looks_like_auth_error(&error) => {
                let resume_token = ensure_resume_token();
                conversation.set_camera_pending_connect(Some(PendingTaskConnect {
                    resume_token: resume_token.clone(),
                    name: candidate.name.clone(),
                    ip: candidate.ip.clone(),
                    room: candidate.room.clone(),
                    port: candidate.port,
                    snapshot_url: None,
                    rtsp_paths: candidate.rtsp_paths.clone(),
                    requires_auth: true,
                    vendor: candidate.vendor.clone(),
                    model: candidate.model.clone(),
                }));
                let _ = self.save_conversation(request, &conversation);
                self.needs_input(
                    request,
                    "camera_hub_service",
                    RiskLevel::Medium,
                    "这台摄像头需要密码，请回复：密码 xxxxxx".to_string(),
                    vec!["password".to_string()],
                    resume_token,
                )
            }
            Err(error) => self.failed(request, "camera_hub_service", RiskLevel::Medium, error),
        }
    }

    fn connect_camera_direct(&self, request: &TaskRequest) -> TaskResponse {
        let Some(ip) = first_string(&[&request.entity_refs, &request.args], &["/ip"]) else {
            return self.failed(
                request,
                "camera_hub_service",
                RiskLevel::Medium,
                "缺少摄像头 IP 地址，当前最小实现只支持“接入 1”或显式提供 IP。".to_string(),
            );
        };

        let pending = PendingTaskConnect {
            resume_token: String::new(),
            name: first_string(&[&request.entity_refs, &request.args], &["/name"])
                .unwrap_or_else(|| format!("Camera {ip}")),
            ip: ip.clone(),
            room: first_string(&[&request.entity_refs, &request.args], &["/room"]),
            port: first_u16(&[&request.entity_refs, &request.args], &["/port"]).unwrap_or(554),
            snapshot_url: first_string(&[&request.entity_refs, &request.args], &["/snapshot_url"]),
            rtsp_paths: first_string_vec(
                &[&request.entity_refs, &request.args],
                &["/path_candidates", "/rtsp_paths"],
            ),
            requires_auth: false,
            vendor: first_string(&[&request.entity_refs, &request.args], &["/vendor"]),
            model: first_string(&[&request.entity_refs, &request.args], &["/model"]),
        };
        let connect_request =
            pending_connect_to_request(&pending, first_string(&[&request.args], &["/password"]));

        match self.hub().manual_add(connect_request, None) {
            Ok(summary) => self.completed(
                request,
                "camera_hub_service",
                RiskLevel::Medium,
                format!(
                    "已接入摄像头 {}，设备库现在共有 {} 台。",
                    summary.device.ip_address.clone().unwrap_or(ip),
                    summary.devices.len()
                ),
                json!({
                    "device": summary.device,
                    "devices": summary.devices.len(),
                }),
                Vec::new(),
                vec!["分析客厅摄像头".to_string()],
            ),
            Err(error) if looks_like_auth_error(&error) => {
                let mut conversation = self.load_or_create_conversation(request);
                let resume_token = ensure_resume_token();
                let mut pending_with_token = pending.clone();
                pending_with_token.resume_token = resume_token.clone();
                conversation.set_camera_pending_connect(Some(pending_with_token));
                let _ = self.save_conversation(request, &conversation);
                self.needs_input(
                    request,
                    "camera_hub_service",
                    RiskLevel::Medium,
                    "这台摄像头需要密码，请回复：密码 xxxxxx".to_string(),
                    vec!["password".to_string()],
                    resume_token,
                )
            }
            Err(error) => self.failed(request, "camera_hub_service", RiskLevel::Medium, error),
        }
    }

    fn resume_camera_connect(&self, request: &TaskRequest, resume_token: &str) -> TaskResponse {
        let Some(password) = string_at_paths(&request.args, &["/password"]) else {
            return self.failed(
                request,
                "camera_hub_service",
                RiskLevel::Medium,
                "缺少 password，无法继续接入流程。".to_string(),
            );
        };
        let mut conversation = self.load_or_create_conversation(request);
        let Some(pending) = conversation.camera_pending_connect() else {
            return self.failed(
                request,
                "camera_hub_service",
                RiskLevel::Medium,
                "当前没有待继续的接入流程，请重新发送“扫描摄像头”。".to_string(),
            );
        };
        if pending.resume_token != resume_token {
            return self.failed(
                request,
                "camera_hub_service",
                RiskLevel::Medium,
                "接入令牌已失效，请重新发送“扫描摄像头”。".to_string(),
            );
        }

        match self
            .hub()
            .manual_add(pending_connect_to_request(&pending, Some(password)), None)
        {
            Ok(summary) => {
                conversation.set_camera_pending_connect(None);
                conversation
                    .retain_camera_pending_candidates(|candidate| candidate.ip != pending.ip);
                let _ = self.save_conversation(request, &conversation);
                self.completed(
                    request,
                    "camera_hub_service",
                    RiskLevel::Medium,
                    format!(
                        "密码已收到。\n已接入摄像头 {}，设备库现在共有 {} 台。",
                        summary.device.ip_address.clone().unwrap_or(pending.ip),
                        summary.devices.len()
                    ),
                    json!({
                        "device": summary.device,
                        "devices": summary.devices.len(),
                    }),
                    Vec::new(),
                    vec!["分析客厅摄像头".to_string()],
                )
            }
            Err(error) if looks_like_auth_error(&error) => self.needs_input(
                request,
                "camera_hub_service",
                RiskLevel::Medium,
                "这个密码还是不对，请再回复一次：密码 xxxxxx".to_string(),
                vec!["password".to_string()],
                pending.resume_token,
            ),
            Err(error) => self.failed(request, "camera_hub_service", RiskLevel::Medium, error),
        }
    }

    fn handle_camera_analyze(&self, request: &TaskRequest) -> TaskResponse {
        let target = match self.resolve_camera_target(request) {
            Ok(target) => target,
            Err(error) => {
                return self.failed(request, "vision_executor", RiskLevel::Low, error);
            }
        };

        let detect_label = first_string(&[&request.args], &["/detect_label"])
            .unwrap_or_else(|| "person".to_string());
        let min_confidence = request
            .args
            .pointer("/min_confidence")
            .and_then(Value::as_f64)
            .unwrap_or(0.25);
        let prompt = first_string(&[&request.args], &["/prompt"]);

        let action = apply_governance_defaults(Action {
            domain: "vision".to_string(),
            operation: OP_ANALYZE_CAMERA.to_string(),
            resource: json!({ "device_id": target.device_id }),
            args: json!({
                "detect_label": detect_label,
                "min_confidence": min_confidence,
                "prompt": prompt,
            }),
            risk_level: RiskLevel::Low,
            requires_approval: request_requires_approval(request),
            dry_run: false,
        });
        if let Err(response) = self.ensure_action_allowed(request, &action, "vision_executor") {
            return response;
        }

        let vision = VisionExecutor::new(self.admin_store.registry_store().clone());
        match vision.execute(&action, &request.task_id, &step_id_for_request(request)) {
            Ok(result) if result.status == StepStatus::Success => {
                let summary =
                    string_at_paths(&result.result_payload, &["/summary", "/detection_summary"])
                        .unwrap_or_else(|| "分析完成".to_string());
                let mut payload = result.result_payload;
                if let Err(error) = self.persist_vision_media_assets(request, &target, &mut payload)
                {
                    return self.failed(
                        request,
                        "vision_executor",
                        RiskLevel::Low,
                        format!("分析已完成，但保存媒体记录失败: {error}"),
                    );
                }
                let artifacts = build_vision_artifacts(&payload);
                let notification_request = self.build_notification_request(
                    request,
                    "task.completed",
                    &target,
                    &payload,
                    &artifacts,
                );
                let mut events = Vec::new();
                if let Some(notification_request) = notification_request {
                    let encoded =
                        serde_json::to_value(&notification_request).unwrap_or(Value::Null);
                    if let Some(object) = payload.as_object_mut() {
                        object.insert("notification_request".to_string(), encoded.clone());
                    }
                    events.push(self.serialize_event_record(&build_task_event_record(
                        request,
                        &step_id_for_request(request),
                        "task.notification_requested",
                        EventSeverity::Info,
                        json!({
                            "executor_used": "vision_executor",
                            "notification": encoded,
                        }),
                    )));
                    let delivery_outcome = self.deliver_notification_request(&notification_request);
                    if let Some(object) = payload.as_object_mut() {
                        object.insert(
                            "notification_delivery".to_string(),
                            delivery_outcome.payload.clone(),
                        );
                        if notification_request.destination.kind
                            == NotificationDestinationKind::Conversation
                        {
                            object.insert(
                                "interaction_reply".to_string(),
                                delivery_outcome.payload.clone(),
                            );
                        }
                        if notification_request.destination.kind
                            == NotificationDestinationKind::Recipient
                            && delivery_outcome.event_type == "task.proactive_delivery_failed"
                        {
                            object.insert(
                                "proactive_delivery_failure".to_string(),
                                delivery_outcome.payload.clone(),
                            );
                        }
                    }
                    events.push(self.serialize_event_record(&build_task_event_record(
                        request,
                        &step_id_for_request(request),
                        delivery_outcome.event_type,
                        delivery_outcome.severity,
                        json!({
                            "executor_used": "vision_executor",
                            "notification_request": notification_request,
                            "delivery": delivery_outcome.payload,
                        }),
                    )));
                }
                self.completed_with_context(
                    request,
                    "vision_executor",
                    RiskLevel::Low,
                    format!("{} 分析完成：{}", target.display_name, summary),
                    payload,
                    artifacts,
                    events,
                    Vec::new(),
                )
            }
            Ok(result) => self.failed(
                request,
                "vision_executor",
                RiskLevel::Low,
                result
                    .error_message
                    .unwrap_or_else(|| "vision executor failed".to_string()),
            ),
            Err(error) => self.failed(request, "vision_executor", RiskLevel::Low, error),
        }
    }

    fn handle_general_message(&self, request: &TaskRequest) -> TaskResponse {
        let turn_started = Instant::now();
        if let Some(response) = self.handle_general_message_active_frame(request, turn_started) {
            return response;
        }

        let (plan, trace) = match self.general_message_plan(request, None) {
            Ok(plan) => plan,
            Err(error) => {
                let mut response = self.general_message_unsupported_response(request, None);
                attach_general_message_controller_trace(
                    &mut response,
                    &GeneralMessageControllerTrace {
                        controller_stage: "controller_error".to_string(),
                        fallback_reason: Some(error),
                        ..Default::default()
                    },
                    turn_started.elapsed(),
                );
                return response;
            }
        };

        let mut response = self.execute_general_message_plan(request, plan, None);
        attach_general_message_controller_trace(&mut response, &trace, turn_started.elapsed());
        response
    }

    fn handle_general_message_active_frame(
        &self,
        request: &TaskRequest,
        turn_started: Instant,
    ) -> Option<TaskResponse> {
        let conversation = self.load_or_create_conversation(request);
        let continuation_token = continuation_token_from_request(request);
        let continuation_token = continuation_token.as_deref();

        if let Some(pending) = conversation.clip_pending_confirmation() {
            return Some(self.handle_clip_confirmation_frame(
                request,
                &pending,
                continuation_token,
                turn_started,
            ));
        }
        if let Some(pending) = conversation.camera_pending_connect() {
            return Some(self.handle_camera_connect_frame(request, &pending, continuation_token));
        }
        if let Some(pending) = conversation.general_message_loop() {
            return Some(self.handle_general_message_loop_frame(
                request,
                &pending,
                continuation_token,
            ));
        }
        None
    }

    fn handle_general_message_loop_frame(
        &self,
        request: &TaskRequest,
        pending: &PendingTaskGeneralMessageLoop,
        continuation_token: Option<&str>,
    ) -> TaskResponse {
        if !active_frame_token_matches(continuation_token, &pending.resume_token) {
            return self.failed(
                request,
                "agentic_interpreter",
                RiskLevel::Low,
                "这次补充说明的令牌已失效，请重新描述你的需求。".to_string(),
            );
        }
        self.resume_general_message_loop(request, &pending.resume_token)
    }

    fn handle_camera_connect_frame(
        &self,
        request: &TaskRequest,
        pending: &PendingTaskConnect,
        continuation_token: Option<&str>,
    ) -> TaskResponse {
        if !active_frame_token_matches(continuation_token, &pending.resume_token) {
            return self.failed(
                request,
                "camera_hub_service",
                RiskLevel::Medium,
                "接入令牌已失效，请重新发送“扫描摄像头”。".to_string(),
            );
        }

        let routed = inject_password_arg_from_raw_text(request);
        if string_at_paths(&routed.args, &["/password"]).is_some() {
            return self.resume_camera_connect(&routed, &pending.resume_token);
        }

        if active_frame_cancel_requested(request.intent.raw_text.as_str()) {
            let mut conversation = self.load_or_create_conversation(request);
            conversation.set_camera_pending_connect(None);
            if let Err(error) = self.save_conversation(request, &conversation) {
                return self.failed(
                    request,
                    "camera_hub_service",
                    RiskLevel::Medium,
                    format!("无法更新摄像头接入状态: {error}"),
                );
            }
            return self.completed(
                request,
                "camera_hub_service",
                RiskLevel::Medium,
                "好的，先不继续接入这台摄像头。".to_string(),
                json!({
                    "reply_pack": {
                        "kind": "conversation_cancel",
                        "summary": "好的，先不继续接入这台摄像头。",
                        "conversation_act": "cancel",
                    }
                }),
                Vec::new(),
                Vec::new(),
            );
        }

        self.needs_input(
            request,
            "camera_hub_service",
            RiskLevel::Medium,
            "这台摄像头需要密码，请回复：密码 xxxxxx".to_string(),
            vec!["password".to_string()],
            pending.resume_token.clone(),
        )
    }

    fn handle_clip_confirmation_frame(
        &self,
        request: &TaskRequest,
        pending: &PendingTaskClipConfirmation,
        continuation_token: Option<&str>,
        turn_started: Instant,
    ) -> TaskResponse {
        if !active_frame_token_matches(continuation_token, &pending.resume_token) {
            return self.failed(
                request,
                "camera_hub_service",
                RiskLevel::Low,
                "回放确认令牌已失效，请重新发送“录一段”。".to_string(),
            );
        }

        match clip_confirmation_reply_decision(request.intent.raw_text.as_str()) {
            ClipConfirmationReplyDecision::Deliver => {
                return self.complete_clip_confirmation_delivery(request, pending);
            }
            ClipConfirmationReplyDecision::Decline => {
                return self.decline_clip_confirmation(request, pending);
            }
            ClipConfirmationReplyDecision::Unknown => {}
        }

        let (plan, trace) = match self.general_message_plan(request, None) {
            Ok(plan) => plan,
            Err(error) => {
                let mut response =
                    self.preserve_clip_confirmation_frame_response(request, pending, None);
                attach_general_message_controller_trace(
                    &mut response,
                    &GeneralMessageControllerTrace {
                        controller_stage: "controller_error".to_string(),
                        fallback_reason: Some(error),
                        ..Default::default()
                    },
                    turn_started.elapsed(),
                );
                return response;
            }
        };

        match clip_confirmation_active_frame_decision(&plan) {
            ActiveFrameDecision::Deliver => {
                self.complete_clip_confirmation_delivery(request, pending)
            }
            ActiveFrameDecision::Cancel => self.decline_clip_confirmation(request, pending),
            ActiveFrameDecision::Supersede => {
                if let Err(response) = self.clear_clip_confirmation_if_matches(request, pending) {
                    return response;
                }
                let mut response = self.execute_general_message_plan(request, plan, None);
                attach_general_message_controller_trace(
                    &mut response,
                    &trace,
                    turn_started.elapsed(),
                );
                response
            }
            ActiveFrameDecision::Preserve => {
                let mut response =
                    self.preserve_clip_confirmation_frame_response(request, pending, Some(&plan));
                attach_general_message_controller_trace(
                    &mut response,
                    &trace,
                    turn_started.elapsed(),
                );
                response
            }
        }
    }

    fn resume_general_message_loop(
        &self,
        request: &TaskRequest,
        resume_token: &str,
    ) -> TaskResponse {
        let turn_started = Instant::now();
        let conversation = self.load_or_create_conversation(request);
        let Some(pending) = conversation.general_message_loop() else {
            return self.failed(
                request,
                "agentic_interpreter",
                RiskLevel::Low,
                "当前没有待补充的自然语义流程，请直接重新描述你的需求。".to_string(),
            );
        };
        if pending.resume_token != resume_token {
            return self.failed(
                request,
                "agentic_interpreter",
                RiskLevel::Low,
                "这次补充说明的令牌已失效，请重新描述你的需求。".to_string(),
            );
        }

        if let Some(response) = self.handle_home_assistant_clarification_frame(request, &pending) {
            return response;
        }

        let (plan, trace) = match self.general_message_plan(request, Some(&pending)) {
            Ok(plan) => plan,
            Err(error) => {
                let mut response = self.general_message_unsupported_response(request, None);
                attach_general_message_controller_trace(
                    &mut response,
                    &GeneralMessageControllerTrace {
                        controller_stage: "controller_error".to_string(),
                        fallback_reason: Some(error),
                        ..Default::default()
                    },
                    turn_started.elapsed(),
                );
                return response;
            }
        };
        let mut response = self.execute_general_message_plan(request, plan, Some(&pending));
        attach_general_message_controller_trace(&mut response, &trace, turn_started.elapsed());
        response
    }

    fn handle_home_assistant_clarification_frame(
        &self,
        request: &TaskRequest,
        pending: &PendingTaskGeneralMessageLoop,
    ) -> Option<TaskResponse> {
        let pending_ha = pending.home_assistant.as_ref()?;
        if active_frame_cancel_requested(request.intent.raw_text.as_str()) {
            return Some(self.cancel_home_assistant_clarification(request, pending, pending_ha));
        }

        if infer_home_assistant_natural_action(request.intent.raw_text.as_str()).is_some() {
            if let Err(response) =
                self.clear_general_message_loop_if_matches(request, Some(pending))
            {
                return Some(response);
            }
            return Some(self.handle_general_message_home_assistant_action(request, None));
        }

        let selected = match resolve_pending_home_assistant_candidate_selection(
            pending_ha,
            request.intent.raw_text.as_str(),
        ) {
            PendingHomeAssistantCandidateSelection::Selected(candidate) => candidate,
            PendingHomeAssistantCandidateSelection::Ambiguous(candidates) => {
                return Some(
                    self.general_message_home_assistant_pending_selection_response(
                        request,
                        pending,
                        pending_ha,
                        candidates.as_slice(),
                        "ambiguous_candidate_reply",
                        "我还不能唯一确定是哪一个，请回复序号、完整名称，或“取消”。",
                    ),
                );
            }
            PendingHomeAssistantCandidateSelection::Missing => {
                return Some(
                    self.general_message_home_assistant_pending_selection_response(
                        request,
                        pending,
                        pending_ha,
                        pending_ha.candidates.as_slice(),
                        "missing_candidate_reply",
                        "我没有在候选里匹配到这个回复，请回复：第一个 / 名称 / 取消。",
                    ),
                );
            }
        };

        Some(self.execute_home_assistant_clarified_action(request, pending, pending_ha, &selected))
    }

    fn cancel_home_assistant_clarification(
        &self,
        request: &TaskRequest,
        pending: &PendingTaskGeneralMessageLoop,
        pending_ha: &PendingTaskHomeAssistantClarification,
    ) -> TaskResponse {
        if let Err(response) = self.clear_general_message_loop_if_matches(request, Some(pending)) {
            return response;
        }
        let message = "好的，这次 Home Assistant 动作先不执行。".to_string();
        let event = self.serialize_event_record(&build_task_event_record(
            request,
            &step_id_for_request(request),
            "home_assistant.service_action_clarification_cancelled",
            EventSeverity::Info,
            json!({
                "status": "cancelled",
                "metadata_only": true,
                "secret_scan": "clean",
                "domain": pending_ha.domain,
                "service": pending_ha.service,
                "candidate_count": pending_ha.candidates.len(),
                "executed": false,
            }),
        ));
        self.completed_with_context(
            request,
            "agentic_interpreter",
            RiskLevel::Low,
            message.clone(),
            json!({
                "reply_pack": {
                    "kind": "ha_action_cancel",
                    "status": "cancelled",
                    "summary": message,
                    "domain": pending_ha.domain,
                    "service": pending_ha.service,
                    "executed": false,
                    "redacted": true,
                }
            }),
            Vec::new(),
            vec![event],
            vec!["状态".to_string(), "最近事件".to_string()],
        )
    }

    fn general_message_home_assistant_pending_selection_response(
        &self,
        request: &TaskRequest,
        pending: &PendingTaskGeneralMessageLoop,
        pending_ha: &PendingTaskHomeAssistantClarification,
        candidates: &[PendingTaskHomeAssistantCandidate],
        reason: &str,
        message: &str,
    ) -> TaskResponse {
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
                "domain": pending_ha.domain,
                "service": pending_ha.service,
                "candidate_count": candidates.len(),
                "executed": false,
            }),
        ));
        self.needs_input_with_context(
            request,
            "agentic_interpreter",
            RiskLevel::Low,
            message.to_string(),
            vec!["home_assistant_entity".to_string()],
            pending.resume_token.clone(),
            json!({
                "reply_pack": {
                    "kind": "ha_action_clarify",
                    "status": "needs_input",
                    "summary": message,
                    "domain": pending_ha.domain,
                    "service": pending_ha.service,
                    "candidates": candidates.iter().map(redacted_pending_home_assistant_candidate_summary).collect::<Vec<_>>(),
                    "redacted": true,
                },
                "home_assistant_clarification": {
                    "kind": "home_assistant.service_action_clarification",
                    "reason": reason,
                    "candidate_count": candidates.len(),
                    "redacted": true,
                }
            }),
            vec![event],
            home_assistant_pending_clarification_actions(candidates),
        )
    }

    fn execute_home_assistant_clarified_action(
        &self,
        request: &TaskRequest,
        pending: &PendingTaskGeneralMessageLoop,
        pending_ha: &PendingTaskHomeAssistantClarification,
        selected: &PendingTaskHomeAssistantCandidate,
    ) -> TaskResponse {
        let action = HomeAssistantNaturalActionRequest {
            domain: pending_ha.domain.clone(),
            service: pending_ha.service.clone(),
            entity_hint: pending_ha.entity_hint.clone(),
        };
        let state = match self.admin_store.home_assistant_secret_state() {
            Ok(state) => state,
            Err(error) => {
                if let Err(response) =
                    self.clear_general_message_loop_if_matches(request, Some(pending))
                {
                    return response;
                }
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
                        domain: pending_ha.domain.clone(),
                        service: pending_ha.service.clone(),
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
                if let Err(response) =
                    self.clear_general_message_loop_if_matches(request, Some(pending))
                {
                    return response;
                }
                return self.general_message_home_assistant_action_response(
                    request,
                    "blocked",
                    false,
                    false,
                    &redact_task_api_text(&error),
                    None,
                    HomeAssistantServiceActionRequest {
                        domain: pending_ha.domain.clone(),
                        service: pending_ha.service.clone(),
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
                if let Err(response) =
                    self.clear_general_message_loop_if_matches(request, Some(pending))
                {
                    return response;
                }
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
                        domain: pending_ha.domain.clone(),
                        service: pending_ha.service.clone(),
                        fields: json!({}),
                        ..Default::default()
                    },
                    "home_assistant.service_action_failed",
                    EventSeverity::Error,
                    None,
                );
            }
        };
        let Some(entity) = entities
            .iter()
            .find(|entity| entity.entity_id.eq_ignore_ascii_case(&selected.entity_id))
            .cloned()
        else {
            let refreshed_candidates = entities
                .iter()
                .filter(|entity| entity.domain.eq_ignore_ascii_case(&pending_ha.domain))
                .take(5)
                .cloned()
                .collect::<Vec<_>>();
            if refreshed_candidates.is_empty() {
                if let Err(response) =
                    self.clear_general_message_loop_if_matches(request, Some(pending))
                {
                    return response;
                }
                return self.general_message_home_assistant_action_response(
                    request,
                    "blocked",
                    false,
                    false,
                    "候选实体已经不可用，本次没有执行。",
                    None,
                    HomeAssistantServiceActionRequest {
                        domain: pending_ha.domain.clone(),
                        service: pending_ha.service.clone(),
                        fields: json!({}),
                        ..Default::default()
                    },
                    "home_assistant.service_action_blocked",
                    EventSeverity::Warning,
                    None,
                );
            }
            return self.general_message_home_assistant_clarification_response(
                request,
                &action,
                refreshed_candidates.as_slice(),
                "candidate_stale",
                Some(pending),
            );
        };

        let action_request =
            normalize_home_assistant_service_action_request(&HomeAssistantServiceActionRequest {
                entity_id: entity.entity_id.clone(),
                domain: pending_ha.domain.clone(),
                service: pending_ha.service.clone(),
                fields: json!({}),
            });
        if let Err(message) = validate_home_assistant_service_action_request(
            &action_request,
            state.enabled,
            &state.exposed_domains,
        ) {
            if let Err(response) =
                self.clear_general_message_loop_if_matches(request, Some(pending))
            {
                return response;
            }
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
        if let Err(response) = self.clear_general_message_loop_if_matches(request, Some(pending)) {
            return response;
        }
        let response = match client.call_service(
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
        };
        response
    }

    fn execute_general_message_plan(
        &self,
        request: &TaskRequest,
        plan: GeneralMessagePlan,
        prior_pending: Option<&PendingTaskGeneralMessageLoop>,
    ) -> TaskResponse {
        match plan.kind {
            GeneralMessagePlanKind::CapabilitySummary => {
                if let Err(response) =
                    self.clear_general_message_loop_if_matches(request, prior_pending)
                {
                    return response;
                }
                self.general_message_capability_summary_response(request, None)
            }
            GeneralMessagePlanKind::Clarify => {
                self.general_message_clarification_response(request, &plan, prior_pending)
            }
            GeneralMessagePlanKind::ConversationAct => {
                self.general_message_conversation_response(request, &plan, prior_pending)
            }
            GeneralMessagePlanKind::CameraReplayRecentClip => {
                if let Err(response) =
                    self.clear_general_message_loop_if_matches(request, prior_pending)
                {
                    return response;
                }
                let Some(recent_clip) = plan.recent_clip else {
                    return self.general_message_unsupported_response(request, None);
                };
                self.complete_recent_clip_playback(request, &recent_clip)
            }
            GeneralMessagePlanKind::CameraSnapshot => {
                if let Err(response) =
                    self.clear_general_message_loop_if_matches(request, prior_pending)
                {
                    return response;
                }
                let mut routed = request.clone();
                routed.intent.domain = "camera".to_string();
                routed.intent.action = "snapshot".to_string();
                if let Some(camera_hint) = plan.camera_hint {
                    upsert_json_string(&mut routed.args, "/device_hint", &camera_hint);
                }
                self.handle_camera_snapshot(&routed)
            }
            GeneralMessagePlanKind::CameraRecordClip => {
                if let Err(response) =
                    self.clear_general_message_loop_if_matches(request, prior_pending)
                {
                    return response;
                }
                let mut routed = request.clone();
                routed.intent.domain = "camera".to_string();
                routed.intent.action = "record_clip".to_string();
                if let Some(camera_hint) = plan.camera_hint {
                    upsert_json_string(&mut routed.args, "/device_hint", &camera_hint);
                }
                self.handle_camera_record_clip(&routed)
            }
            GeneralMessagePlanKind::KnowledgeSearch => {
                if let Err(response) =
                    self.clear_general_message_loop_if_matches(request, prior_pending)
                {
                    return response;
                }
                let routed = self.routed_general_message_knowledge_request(
                    request,
                    &plan,
                    KNOWLEDGE_DOMAIN,
                    KNOWLEDGE_OP_SEARCH,
                );
                self.handle_knowledge_search(&routed)
            }
            GeneralMessagePlanKind::RagAnswer => {
                if let Err(response) =
                    self.clear_general_message_loop_if_matches(request, prior_pending)
                {
                    return response;
                }
                let routed = self.routed_general_message_knowledge_request(
                    request,
                    &plan,
                    RAG_DOMAIN,
                    RAG_OP_ANSWER,
                );
                self.handle_rag_answer(&routed)
            }
            GeneralMessagePlanKind::HomeAssistantServiceAction => {
                if let Err(response) =
                    self.clear_general_message_loop_if_matches(request, prior_pending)
                {
                    return response;
                }
                self.handle_general_message_home_assistant_action(
                    request,
                    plan.home_assistant_action.as_ref(),
                )
            }
            GeneralMessagePlanKind::VisionEventSummary => {
                if let Err(response) =
                    self.clear_general_message_loop_if_matches(request, prior_pending)
                {
                    return response;
                }
                self.handle_general_message_latest_vision_event(request)
            }
            GeneralMessagePlanKind::VisionEventNotifyLatest => {
                if let Err(response) =
                    self.clear_general_message_loop_if_matches(request, prior_pending)
                {
                    return response;
                }
                self.handle_general_message_notify_latest_vision_event(request)
            }
            GeneralMessagePlanKind::VlmDescribeLatestEvent
            | GeneralMessagePlanKind::VlmDescribeEvent => {
                if let Err(response) =
                    self.clear_general_message_loop_if_matches(request, prior_pending)
                {
                    return response;
                }
                self.handle_general_message_vlm_describe_latest_event(request, &plan)
            }
            GeneralMessagePlanKind::FamilyMemorySummary => {
                if let Err(response) =
                    self.clear_general_message_loop_if_matches(request, prior_pending)
                {
                    return response;
                }
                self.handle_general_message_family_timeline(request, &plan)
            }
            GeneralMessagePlanKind::SystemReadiness => {
                if let Err(response) =
                    self.clear_general_message_loop_if_matches(request, prior_pending)
                {
                    return response;
                }
                self.handle_general_message_system_readiness(request)
            }
            GeneralMessagePlanKind::EvtReadiness => {
                if let Err(response) =
                    self.clear_general_message_loop_if_matches(request, prior_pending)
                {
                    return response;
                }
                self.handle_general_message_evt_readiness(request)
            }
            GeneralMessagePlanKind::EvtPreflight => {
                if let Err(response) =
                    self.clear_general_message_loop_if_matches(request, prior_pending)
                {
                    return response;
                }
                self.handle_general_message_evt_preflight(request)
            }
            GeneralMessagePlanKind::EvtEvidenceBundle => {
                if let Err(response) =
                    self.clear_general_message_loop_if_matches(request, prior_pending)
                {
                    return response;
                }
                self.handle_general_message_evt_evidence_bundle(request)
            }
            GeneralMessagePlanKind::FamilyTimelineSummary
            | GeneralMessagePlanKind::FamilyTimelineQuery => {
                if let Err(response) =
                    self.clear_general_message_loop_if_matches(request, prior_pending)
                {
                    return response;
                }
                self.handle_general_message_family_timeline(request, &plan)
            }
            GeneralMessagePlanKind::GuardianRuleProposal => {
                if let Err(response) =
                    self.clear_general_message_loop_if_matches(request, prior_pending)
                {
                    return response;
                }
                self.handle_general_message_guardian_rule_proposal(request, &plan)
            }
            GeneralMessagePlanKind::GuardianRuleList | GeneralMessagePlanKind::GuardianStatus => {
                if let Err(response) =
                    self.clear_general_message_loop_if_matches(request, prior_pending)
                {
                    return response;
                }
                self.handle_general_message_guardian_status(request)
            }
            GeneralMessagePlanKind::GuardianRuleEnable => {
                if let Err(response) =
                    self.clear_general_message_loop_if_matches(request, prior_pending)
                {
                    return response;
                }
                self.handle_general_message_guardian_status_update(request, "active")
            }
            GeneralMessagePlanKind::GuardianRulePause => {
                if let Err(response) =
                    self.clear_general_message_loop_if_matches(request, prior_pending)
                {
                    return response;
                }
                self.handle_general_message_guardian_status_update(request, "paused")
            }
            GeneralMessagePlanKind::FamilyMemoryConfirm
            | GeneralMessagePlanKind::FamilyMemoryFavorite
            | GeneralMessagePlanKind::FamilyMemoryHide
            | GeneralMessagePlanKind::FamilyMemoryCorrectSummary
            | GeneralMessagePlanKind::FamilyMemoryCorrectLabels
            | GeneralMessagePlanKind::FamilyMemoryShowFavorites => {
                if let Err(response) =
                    self.clear_general_message_loop_if_matches(request, prior_pending)
                {
                    return response;
                }
                self.handle_general_message_family_memory_feedback(request, &plan)
            }
            GeneralMessagePlanKind::Unsupported => {
                if let Err(response) =
                    self.clear_general_message_loop_if_matches(request, prior_pending)
                {
                    return response;
                }
                self.general_message_unsupported_response(request, plan.reply_text.as_deref())
            }
        }
    }

    fn complete_recent_clip_playback(
        &self,
        request: &TaskRequest,
        recent_clip: &RecentClipPlaybackState,
    ) -> TaskResponse {
        self.completed(
            request,
            "camera_hub_service",
            RiskLevel::Low,
            "完整回放如下".to_string(),
            build_clip_delivery_payload(recent_clip),
            vec![build_clip_delivery_artifact(recent_clip)],
            Vec::new(),
        )
    }

    fn general_message_capability_summary_response(
        &self,
        request: &TaskRequest,
        reply_text: Option<&str>,
    ) -> TaskResponse {
        let summary = reply_text
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                general_message_support_summary_for_request(request.intent.raw_text.as_str())
            });
        let examples = general_message_supported_examples();
        self.completed(
            request,
            "agentic_interpreter",
            RiskLevel::Low,
            summary.clone(),
            json!({
                "reply_pack": {
                    "kind": "capability_summary",
                    "summary": summary,
                    "capabilities": [
                        "camera_snapshot",
                        "camera_record_clip",
                        "vision_event_summary",
                        "vision_event_notify_latest",
                        "vlm_describe_latest_event",
                        "family_memory_summary",
                        "ha_service_action",
                        "system_readiness",
                        "evt_readiness",
                        "evt_preflight",
                        "evt_evidence_bundle",
                        "family_timeline_summary",
                        "family_timeline_query",
                        "guardian_rule_proposal",
                        "guardian_status",
                        "knowledge_search",
                        "rag_answer",
                    ],
                    "examples": examples,
                }
            }),
            Vec::new(),
            vec![
                "拍一张门口".to_string(),
                "最近事件".to_string(),
                "通知最新事件".to_string(),
                "开灯".to_string(),
                "状态".to_string(),
                "压测前状态怎么样".to_string(),
                "今天家里发生了什么".to_string(),
                "以后门口有人就通知我".to_string(),
            ],
        )
    }

    fn general_message_unsupported_response(
        &self,
        request: &TaskRequest,
        reply_text: Option<&str>,
    ) -> TaskResponse {
        let summary = reply_text
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(general_message_unsupported_summary);
        let examples = general_message_supported_examples();
        self.completed(
            request,
            "agentic_interpreter",
            RiskLevel::Low,
            summary.clone(),
            json!({
                "reply_pack": {
                    "kind": "unsupported",
                    "summary": summary,
                    "capabilities": [
                        "camera_snapshot",
                        "camera_record_clip",
                        "vision_event_summary",
                        "vision_event_notify_latest",
                        "vlm_describe_latest_event",
                        "family_memory_summary",
                        "ha_service_action",
                        "system_readiness",
                        "evt_readiness",
                        "evt_preflight",
                        "evt_evidence_bundle",
                        "family_timeline_summary",
                        "family_timeline_query",
                        "guardian_rule_proposal",
                        "guardian_status",
                        "knowledge_search",
                        "rag_answer",
                    ],
                    "examples": examples,
                }
            }),
            Vec::new(),
            vec![
                "拍一张门口".to_string(),
                "最近事件".to_string(),
                "通知最新事件".to_string(),
                "开灯".to_string(),
                "状态".to_string(),
            ],
        )
    }

    fn general_message_conversation_response(
        &self,
        request: &TaskRequest,
        plan: &GeneralMessagePlan,
        prior_pending: Option<&PendingTaskGeneralMessageLoop>,
    ) -> TaskResponse {
        let act = plan.conversation_act.unwrap_or_else(|| {
            infer_general_message_conversation_act(request.intent.raw_text.as_str(), prior_pending)
        });
        if matches!(
            act,
            GeneralMessageConversationAct::Cancel | GeneralMessageConversationAct::Repair
        ) {
            if let Err(response) =
                self.clear_general_message_loop_if_matches(request, prior_pending)
            {
                return response;
            }
        }
        let summary = plan
            .reply_text
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| general_message_conversation_summary(request, prior_pending, act));
        let mut response = self.completed(
            request,
            "agentic_interpreter",
            RiskLevel::Low,
            summary.clone(),
            json!({
                "reply_pack": {
                    "kind": act.reply_pack_kind(),
                    "summary": summary,
                    "conversation_act": act.label(),
                }
            }),
            Vec::new(),
            Vec::new(),
        );
        if act == GeneralMessageConversationAct::ClarifyContinue {
            if let Some(pending) = prior_pending {
                response.resume_token = Some(pending.resume_token.clone());
                response.result.next_actions = vec![
                    "拍一张".to_string(),
                    "录一段".to_string(),
                    "搜索已有内容".to_string(),
                ];
            }
        }
        response
    }

    fn general_message_clarification_response(
        &self,
        request: &TaskRequest,
        plan: &GeneralMessagePlan,
        prior_pending: Option<&PendingTaskGeneralMessageLoop>,
    ) -> TaskResponse {
        let prompt = plan
            .reply_text
            .clone()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| {
                general_message_default_clarification_prompt(request.intent.raw_text.as_str())
            });
        let resume_token = ensure_resume_token();
        let mut conversation = self.load_or_create_conversation(request);
        conversation.set_general_message_loop(Some(PendingTaskGeneralMessageLoop {
            resume_token: resume_token.clone(),
            original_goal: prior_pending
                .and_then(|pending| {
                    (!pending.original_goal.trim().is_empty())
                        .then(|| pending.original_goal.clone())
                })
                .unwrap_or_else(|| request.intent.raw_text.trim().to_string()),
            latest_user_intent_text: request.intent.raw_text.trim().to_string(),
            last_clarification_prompt: prompt.clone(),
            selected_candidate_action: prior_pending
                .and_then(|pending| pending.selected_candidate_action.clone()),
            camera_hint: plan.camera_hint.clone(),
            query: plan.query.clone(),
            home_assistant: None,
        }));
        if let Err(error) = self.save_conversation(request, &conversation) {
            return self.failed(
                request,
                "agentic_interpreter",
                RiskLevel::Low,
                format!("无法保存自然语义澄清状态: {error}"),
            );
        }

        self.needs_input_with_context(
            request,
            "agentic_interpreter",
            RiskLevel::Low,
            prompt.clone(),
            Vec::new(),
            resume_token,
            json!({
                "reply_pack": {
                    "kind": "clarification",
                    "summary": prompt,
                },
                "general_message_loop": {
                    "kind": "general_message_loop",
                    "camera_hint": plan.camera_hint,
                    "query": plan.query,
                    "reason": plan.reason,
                }
            }),
            Vec::new(),
            vec![
                "拍一张".to_string(),
                "录一段".to_string(),
                "搜索已有内容".to_string(),
            ],
        )
    }

    fn clear_general_message_loop_if_matches(
        &self,
        request: &TaskRequest,
        pending: Option<&PendingTaskGeneralMessageLoop>,
    ) -> Result<(), TaskResponse> {
        let Some(pending) = pending else {
            return Ok(());
        };
        let mut conversation = self.load_or_create_conversation(request);
        if conversation
            .general_message_loop()
            .is_some_and(|current| current.resume_token == pending.resume_token)
        {
            conversation.set_general_message_loop(None);
            self.save_conversation(request, &conversation)
                .map_err(|error| {
                    self.failed(
                        request,
                        "agentic_interpreter",
                        RiskLevel::Low,
                        format!("无法更新自然语义流程状态: {error}"),
                    )
                })?;
        }
        Ok(())
    }

    fn general_message_plan(
        &self,
        request: &TaskRequest,
        pending_loop: Option<&PendingTaskGeneralMessageLoop>,
    ) -> Result<(GeneralMessagePlan, GeneralMessageControllerTrace), String> {
        let admin_state = self.admin_store.load_or_create_state()?;
        let selected_camera = admin_state.defaults.selected_camera_device_id.clone();
        let recent_clip = self
            .load_conversation(request)
            .and_then(|conversation| conversation.recent_clip_playback())
            .filter(recent_clip_playback_is_fresh);
        let session_recap = self.recent_general_message_session_recap(request);
        let signals = extract_general_message_signals(
            request,
            &session_recap,
            pending_loop,
            recent_clip.as_ref(),
        );
        let candidates = build_general_message_candidates(
            request,
            &signals,
            selected_camera.as_deref(),
            pending_loop,
            &session_recap,
            recent_clip.as_ref(),
        );
        let workflow_shadow_candidate =
            compile_system_diagnostics_candidate(request.intent.raw_text.as_str());
        let mut trace = GeneralMessageControllerTrace {
            controller_stage: "candidate_builder".to_string(),
            candidate_count: candidates.len(),
            ..Default::default()
        };

        if should_try_general_message_router_llm(&signals, pending_loop) {
            let router_started = Instant::now();
            let router_prompt =
                build_general_message_router_prompt(request, &session_recap, pending_loop);
            let router_options = LlmTextOptions {
                purpose: Some("router".to_string()),
                system_prompt: Some(build_general_message_router_system_prompt()),
                temperature: Some(0.0),
                max_tokens: Some(GENERAL_MESSAGE_ROUTER_MAX_TOKENS),
                timeout: Some(Duration::from_millis(
                    GENERAL_MESSAGE_ROUTER_BUDGET_MS.min(GENERAL_MESSAGE_TURN_BUDGET_MS),
                )),
            };
            let router_result = run_llm_text_with_state_and_options(
                &router_prompt,
                &admin_state.models,
                &router_options,
            );
            trace.router_llm = true;
            trace.nsp_local_only = true;
            trace.router_latency_ms = Some(router_started.elapsed().as_millis() as u64);
            if router_result.available {
                if let Some(mut plan) = parse_general_message_plan(&router_result.text) {
                    enforce_general_message_nsp_hard_guards(request, &mut plan);
                    trace.nsp_schema_valid = general_message_nsp_schema_valid(&plan);
                    record_general_message_nsp_plan_trace(&mut trace, &plan);
                    if let Some(reason) = general_message_nsp_validation_failure(request, &plan) {
                        trace.fallback_reason = Some(reason);
                    } else {
                        trace.controller_stage = "nsp_router".to_string();
                        maybe_render_general_message_reply(
                            request,
                            pending_loop,
                            &admin_state.models,
                            &mut plan,
                            &mut trace,
                        );
                        record_workflow_compiler_shadow(
                            &mut trace,
                            workflow_shadow_candidate.as_ref(),
                            &plan,
                        );
                        return Ok((plan, trace));
                    }
                } else {
                    trace.nsp_schema_valid = false;
                    trace.fallback_reason = Some("nsp_invalid_json_or_decision".to_string());
                }
            } else {
                trace.fallback_reason = Some(router_result.summary);
            }
        }

        if let Some(mut plan) =
            resolve_deterministic_general_message_plan(request, &candidates, pending_loop)
        {
            trace.controller_stage = deterministic_stage_for_plan(&plan).to_string();
            maybe_render_general_message_reply(
                request,
                pending_loop,
                &admin_state.models,
                &mut plan,
                &mut trace,
            );
            record_workflow_compiler_shadow(&mut trace, workflow_shadow_candidate.as_ref(), &plan);
            return Ok((plan, trace));
        }

        if let Some(mut plan) = fallback_general_message_plan(
            request.intent.raw_text.as_str(),
            selected_camera.as_deref(),
        ) {
            trace.controller_stage = "deterministic_fallback".to_string();
            trace
                .fallback_reason
                .get_or_insert_with(|| "deterministic_fallback".to_string());
            maybe_render_general_message_reply(
                request,
                pending_loop,
                &admin_state.models,
                &mut plan,
                &mut trace,
            );
            record_workflow_compiler_shadow(&mut trace, workflow_shadow_candidate.as_ref(), &plan);
            return Ok((plan, trace));
        }

        let mut plan = GeneralMessagePlan {
            kind: GeneralMessagePlanKind::ConversationAct,
            conversation_act: Some(infer_general_message_conversation_act(
                request.intent.raw_text.as_str(),
                pending_loop,
            )),
            reply_text: None,
            canonical_phrase: None,
            camera_hint: pending_loop.and_then(|pending| pending.camera_hint.clone()),
            query: pending_loop
                .and_then(|pending| pending.query.clone())
                .or_else(|| infer_query_from_raw_text(request.intent.raw_text.as_str())),
            home_assistant_action: None,
            guardian_rule: None,
            event_id: None,
            corrected_summary: None,
            corrected_labels: None,
            confidence: None,
            recent_clip: None,
            reason: Some("no_supported_candidate".to_string()),
        };
        trace.controller_stage = "conversation_act".to_string();
        trace
            .fallback_reason
            .get_or_insert_with(|| "no_supported_candidate".to_string());
        maybe_render_general_message_reply(
            request,
            pending_loop,
            &admin_state.models,
            &mut plan,
            &mut trace,
        );
        record_workflow_compiler_shadow(&mut trace, workflow_shadow_candidate.as_ref(), &plan);
        Ok((plan, trace))
    }

    fn recent_general_message_session_recap(&self, request: &TaskRequest) -> Vec<Value> {
        let session_id = conversation_handle_for_request(request);
        self.conversation_store
            .recent_task_runs_for_session(&session_id, GENERAL_MESSAGE_RECAP_LIMIT + 1)
            .unwrap_or_default()
            .into_iter()
            .filter(|task_run| task_run.task_id != request.task_id)
            .take(GENERAL_MESSAGE_RECAP_LIMIT)
            .map(|task_run| self.general_message_recap_entry(&task_run))
            .collect()
    }

    fn general_message_recap_entry(&self, task_run: &TaskRun) -> Value {
        let mut entry = json!({
            "intent_text": task_run.intent_text,
            "domain": task_run.domain,
            "action": task_run.action,
            "status": serde_json::to_value(task_run.status).unwrap_or(Value::Null),
        });

        if let Some(query) = first_string(
            &[&task_run.args],
            &["/query", "/search/query", "/knowledge/query"],
        ) {
            insert_string_value_if_object(&mut entry, "query", query);
        }

        let step_output = task_run
            .metadata
            .pointer("/step_id")
            .and_then(Value::as_str)
            .and_then(|step_id| {
                self.conversation_store
                    .load_task_step(step_id)
                    .ok()
                    .flatten()
            })
            .map(|step| step.output_payload)
            .unwrap_or(Value::Null);
        if let Some(kind) = string_at_paths(&step_output, &["/data/kind", "/data/reply_pack/kind"])
        {
            insert_string_value_if_object(&mut entry, "data_kind", kind);
        }
        if let Some(query) = first_string(
            &[&step_output],
            &[
                "/data/query",
                "/data/search/query",
                "/data/reply_pack/query",
                "/query",
            ],
        ) {
            insert_string_value_if_object(&mut entry, "query", query);
        }
        let video_count =
            array_len_at_paths(&step_output, &["/data/videos", "/data/search/videos"]).unwrap_or(0);
        let image_count =
            array_len_at_paths(&step_output, &["/data/images", "/data/search/images"]).unwrap_or(0);
        let document_count =
            array_len_at_paths(&step_output, &["/data/documents", "/data/search/documents"])
                .unwrap_or(0);
        if let Some(object) = entry.as_object_mut() {
            object.insert(
                "result_counts".to_string(),
                json!({
                    "documents": document_count,
                    "images": image_count,
                    "videos": video_count,
                }),
            );
        }

        if let Some(top_video_path) = self
            .conversation_store
            .artifacts_for_task(&task_run.task_id)
            .unwrap_or_default()
            .into_iter()
            .find(|artifact| artifact.artifact_kind == ArtifactKind::Video)
            .and_then(|artifact| artifact.path)
        {
            insert_string_value_if_object(&mut entry, "top_video_path", top_video_path);
        }

        entry
    }

    fn routed_general_message_knowledge_request(
        &self,
        request: &TaskRequest,
        plan: &GeneralMessagePlan,
        domain: &str,
        action: &str,
    ) -> TaskRequest {
        let mut routed = request.clone();
        routed.intent.domain = domain.to_string();
        routed.intent.action = action.to_string();

        let session_recap = self.recent_general_message_session_recap(request);
        let normalized = normalize_command_text(request.intent.raw_text.as_str());
        let contextual_follow_up = knowledge_search_contextual_follow_up(&normalized);
        let recent_query = recent_search_query_from_recap(&session_recap);
        let query = if contextual_follow_up {
            recent_query.clone().or_else(|| plan.query.clone())
        } else {
            plan.query.clone().or(recent_query)
        };
        if let Some(query) = query.filter(|value| !value.trim().is_empty()) {
            upsert_json_string(&mut routed.args, "/query", &query);
        }

        if let Some(modalities) = knowledge_follow_up_modalities(&normalized) {
            upsert_json_value(
                &mut routed.args,
                "/modalities",
                Value::Array(modalities.into_iter().map(Value::String).collect()),
            );
        }

        let focus_paths = knowledge_follow_up_focus_paths(&normalized, &session_recap);
        if !focus_paths.is_empty() {
            upsert_json_value(
                &mut routed.args,
                "/search/focus_paths",
                Value::Array(focus_paths.into_iter().map(Value::String).collect()),
            );
        }

        routed
    }

    fn handle_camera_record_clip(&self, request: &TaskRequest) -> TaskResponse {
        let target = match self.resolve_camera_target(request) {
            Ok(target) => target,
            Err(error) => {
                return self.failed(request, "camera_hub_service", RiskLevel::Low, error);
            }
        };

        let action = apply_governance_defaults(Action {
            domain: "camera".to_string(),
            operation: "record_clip".to_string(),
            resource: json!({ "device_id": target.device_id.clone() }),
            args: json!({ "device_id": target.device_id.clone() }),
            risk_level: RiskLevel::Low,
            requires_approval: request_requires_approval(request),
            dry_run: false,
        });
        if let Err(response) = self.ensure_action_allowed(request, &action, "camera_hub_service") {
            return response;
        }

        let admin_state = match self.admin_store.load_or_create_state() {
            Ok(state) => state,
            Err(error) => {
                return self.failed(request, "camera_hub_service", RiskLevel::Low, error);
            }
        };
        let recording_policy = resolved_recording_policy(&admin_state, Some(&target));
        let capture_root = match resolved_capture_directory(&admin_state, recording_policy.as_ref())
        {
            Ok(path) => path,
            Err(error) => {
                return self.failed(request, "camera_hub_service", RiskLevel::Low, error);
            }
        };
        let clip_length_seconds = recording_policy
            .as_ref()
            .and_then(RecordingPolicy::clip_length_seconds_hint)
            .unwrap_or_else(|| admin_state.defaults.clip_length_seconds)
            .clamp(3, 300);
        let keyframe_count = recording_policy
            .as_ref()
            .and_then(RecordingPolicy::keyframe_count_hint)
            .or(Some(admin_state.defaults.keyframe_count));
        let keyframe_interval_seconds = recording_policy
            .as_ref()
            .and_then(RecordingPolicy::keyframe_interval_seconds_hint)
            .or(Some(admin_state.defaults.keyframe_interval_seconds));

        let clip_path = build_clip_output_path(&capture_root, &target, current_epoch_ms());
        let adapter = CommandRtspAdapter::default();
        let clip_request = ClipCaptureRequest::new(
            target.device_id.clone(),
            target.primary_stream.url.clone(),
            clip_length_seconds,
            StorageTarget::HarborOsPool,
        )
        .with_keyframe_hints(keyframe_count, keyframe_interval_seconds);

        let clip = match adapter.capture_clip_to_path(&clip_request, &clip_path) {
            Ok(result) => result,
            Err(error) => {
                return self.failed(request, "camera_hub_service", RiskLevel::Low, error);
            }
        };
        let keyframes_dir = build_keyframe_directory(&capture_root, &clip_path);
        let keyframes = match adapter.extract_keyframes(
            &clip_path,
            &keyframes_dir,
            keyframe_count,
            keyframe_interval_seconds,
        ) {
            Ok(paths) => paths,
            Err(error) => {
                return self.failed(
                    request,
                    "camera_hub_service",
                    RiskLevel::Low,
                    format!("短视频已保存，但关键帧抽取失败: {error}"),
                );
            }
        };
        if let Err(error) = self.persist_clip_ingest(&admin_state, &target, &clip, &keyframes) {
            return self.failed(
                request,
                "camera_hub_service",
                RiskLevel::Low,
                format!("短视频已保存，但写入索引副产物失败: {error}"),
            );
        }

        let media_asset = build_clip_media_asset(request, &target, &clip);
        if let Err(error) = self.conversation_store.save_media_asset(&media_asset) {
            return self.failed(
                request,
                "camera_hub_service",
                RiskLevel::Low,
                format!("短视频已保存，但保存媒体记录失败: {error}"),
            );
        }

        let Some(cover_artifact) =
            build_clip_confirmation_cover_artifact(&clip, &keyframes, &media_asset)
        else {
            return self.failed(
                request,
                "camera_hub_service",
                RiskLevel::Low,
                "短视频已保存，但无法生成首帧预览，请稍后重试。".to_string(),
            );
        };
        let resume_token = ensure_resume_token();
        let recent_clip = recent_clip_playback_from_capture(
            &clip,
            &media_asset,
            &cover_artifact,
            &target.display_name,
        );
        let mut conversation = self.load_or_create_conversation(request);
        conversation.set_clip_pending_confirmation(Some(PendingTaskClipConfirmation {
            resume_token: resume_token.clone(),
            clip_media_asset_id: media_asset.asset_id.clone(),
            clip_path: clip.storage.relative_path.clone(),
            clip_mime_type: clip.mime_type.clone(),
            cover_path: cover_artifact.path.clone().unwrap_or_default(),
            display_name: target.display_name.clone(),
        }));
        conversation.set_recent_clip_playback(Some(recent_clip));
        if let Err(error) = self.save_conversation(request, &conversation) {
            return self.failed(
                request,
                "camera_hub_service",
                RiskLevel::Low,
                format!("短视频已保存，但保存回放确认状态失败: {error}"),
            );
        }

        let prompt = clip_confirmation_prompt(&target.display_name);
        self.needs_input_with_artifacts_context(
            request,
            "camera_hub_service",
            RiskLevel::Low,
            prompt,
            Vec::new(),
            resume_token,
            build_clip_confirmation_payload(&target, &clip, &media_asset),
            vec![cover_artifact],
            Vec::new(),
            vec!["要".to_string(), "不要".to_string()],
        )
    }

    fn complete_clip_confirmation_delivery(
        &self,
        request: &TaskRequest,
        pending: &PendingTaskClipConfirmation,
    ) -> TaskResponse {
        let conversation = self.load_or_create_conversation(request);
        let recent_clip = conversation
            .recent_clip_playback()
            .unwrap_or_else(|| recent_clip_playback_from_pending(pending, current_epoch_ms()));
        if let Err(response) = self.clear_clip_confirmation_if_matches(request, pending) {
            return response;
        }
        self.complete_recent_clip_playback(request, &recent_clip)
    }

    fn decline_clip_confirmation(
        &self,
        request: &TaskRequest,
        pending: &PendingTaskClipConfirmation,
    ) -> TaskResponse {
        if let Err(response) = self.clear_clip_confirmation_if_matches(request, pending) {
            return response;
        }
        self.completed(
            request,
            "camera_hub_service",
            RiskLevel::Low,
            "好的，这段回放先不发。".to_string(),
            json!({
                "clip_confirmation": {
                    "kind": "clip_confirmation",
                    "clip_media_asset_id": pending.clip_media_asset_id.clone(),
                    "decision": "declined",
                },
                "reply_pack": {
                    "kind": "conversation_cancel",
                    "summary": "好的，这段回放先不发。",
                    "conversation_act": "cancel",
                }
            }),
            Vec::new(),
            Vec::new(),
        )
    }

    fn preserve_clip_confirmation_frame_response(
        &self,
        request: &TaskRequest,
        pending: &PendingTaskClipConfirmation,
        plan: Option<&GeneralMessagePlan>,
    ) -> TaskResponse {
        let summary = clip_confirmation_preserve_summary(request, pending, plan);
        let mut response = self.completed(
            request,
            "camera_hub_service",
            RiskLevel::Low,
            summary.clone(),
            json!({
                "clip_confirmation": {
                    "kind": "clip_confirmation",
                    "clip_media_asset_id": pending.clip_media_asset_id.clone(),
                    "decision": "pending",
                    "preserved": true,
                },
                "reply_pack": {
                    "kind": "active_frame_preserve",
                    "summary": summary,
                }
            }),
            Vec::new(),
            vec!["要".to_string(), "不要".to_string()],
        );
        response.resume_token = Some(pending.resume_token.clone());
        response
    }

    fn clear_clip_confirmation_if_matches(
        &self,
        request: &TaskRequest,
        pending: &PendingTaskClipConfirmation,
    ) -> Result<(), TaskResponse> {
        let mut conversation = self.load_or_create_conversation(request);
        if conversation
            .clip_pending_confirmation()
            .is_some_and(|current| current.resume_token == pending.resume_token)
        {
            conversation.set_clip_pending_confirmation(None);
            self.save_conversation(request, &conversation)
                .map_err(|error| {
                    self.failed(
                        request,
                        "camera_hub_service",
                        RiskLevel::Low,
                        format!("无法更新回放确认状态: {error}"),
                    )
                })?;
        }
        Ok(())
    }

    fn handle_camera_snapshot(&self, request: &TaskRequest) -> TaskResponse {
        let target = match self.resolve_camera_target(request) {
            Ok(target) => target,
            Err(error) => {
                return self.failed(request, "camera_hub_service", RiskLevel::Low, error);
            }
        };

        let action = apply_governance_defaults(Action {
            domain: "camera".to_string(),
            operation: "snapshot".to_string(),
            resource: json!({ "device_id": target.device_id.clone() }),
            args: json!({ "device_id": target.device_id.clone() }),
            risk_level: RiskLevel::Low,
            requires_approval: request_requires_approval(request),
            dry_run: false,
        });
        if let Err(response) = self.ensure_action_allowed(request, &action, "camera_hub_service") {
            return response;
        }

        match self.hub().capture_camera_snapshot_result(&target.device_id) {
            Ok(snapshot) => {
                let admin_state = match self.admin_store.load_or_create_state() {
                    Ok(state) => state,
                    Err(error) => {
                        return self.failed(request, "camera_hub_service", RiskLevel::Low, error);
                    }
                };
                let recording_policy = resolved_recording_policy(&admin_state, Some(&target));
                let snapshot = match self.persist_snapshot_capture(
                    &admin_state,
                    recording_policy.as_ref(),
                    &target,
                    snapshot,
                ) {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        return self.failed(
                            request,
                            "camera_hub_service",
                            RiskLevel::Low,
                            format!("抓拍已完成，但保存图片失败: {error}"),
                        );
                    }
                };
                let media_asset = build_snapshot_media_asset(request, &target, &snapshot);
                if let Err(error) = self.conversation_store.save_media_asset(&media_asset) {
                    return self.failed(
                        request,
                        "camera_hub_service",
                        RiskLevel::Low,
                        format!("抓拍已完成，但保存媒体记录失败: {error}"),
                    );
                }

                self.completed(
                    request,
                    "camera_hub_service",
                    RiskLevel::Low,
                    format!("已抓拍 {} 当前画面。", target.display_name),
                    build_snapshot_payload(&target, &snapshot, &media_asset),
                    vec![build_snapshot_artifact(&snapshot, &media_asset)],
                    vec![format!("分析 {}", target.display_name)],
                )
            }
            Err(error) => self.failed(request, "camera_hub_service", RiskLevel::Low, error),
        }
    }

    fn handle_camera_share_link(&self, request: &TaskRequest) -> TaskResponse {
        let target = match self.resolve_camera_target(request) {
            Ok(target) => target,
            Err(error) => {
                return self.failed(request, "camera_hub_service", RiskLevel::Medium, error);
            }
        };

        let action = apply_governance_defaults(Action {
            domain: "camera".to_string(),
            operation: "share_link".to_string(),
            resource: json!({ "device_id": target.device_id.clone() }),
            args: json!({ "device_id": target.device_id.clone() }),
            risk_level: RiskLevel::Medium,
            requires_approval: request_requires_approval(request),
            dry_run: false,
        });
        if let Err(response) = self.ensure_action_allowed(request, &action, "camera_hub_service") {
            return response;
        }

        let remote_view_config = match self.admin_store.load_remote_view_config() {
            Ok(config) => config,
            Err(error) => {
                return self.failed(request, "camera_hub_service", RiskLevel::Medium, error);
            }
        };
        let issued = match remote_view::issue_camera_share_token(
            &remote_view_config.share_secret,
            &target.device_id,
            remote_view_config.share_link_ttl_minutes,
        ) {
            Ok(issued) => issued,
            Err(error) => {
                return self.failed(request, "camera_hub_service", RiskLevel::Medium, error);
            }
        };

        let share_link_id = new_share_link_id();
        let media_session_id = new_media_session_id();
        let media_session =
            build_share_media_session(request, &target, &media_session_id, &share_link_id);
        let share_link_record = build_share_link_record(&issued, &media_session_id, &share_link_id);
        if let Err(error) = self
            .conversation_store
            .save_share_link_bundle(&media_session, &share_link_record)
        {
            return self.failed(request, "camera_hub_service", RiskLevel::Medium, error);
        }

        let share_link =
            build_share_link_payload(&target, &issued, &media_session, &share_link_record);
        let event = self.serialize_event_record(&build_task_event_record(
            request,
            &step_id_for_request(request),
            "task.share_link_issued",
            EventSeverity::Info,
            share_link.clone(),
        ));
        self.completed_with_context(
            request,
            "camera_hub_service",
            RiskLevel::Medium,
            format!(
                "已为 {} 生成 {} 分钟共享观看链接。",
                target.display_name, issued.ttl_minutes
            ),
            json!({
                "camera_target": target,
                "share_link": share_link,
            }),
            vec![build_share_link_artifact(&share_link)],
            vec![event],
            vec!["打开共享观看页".to_string()],
        )
    }

    fn handle_knowledge_search(&self, request: &TaskRequest) -> TaskResponse {
        let action = apply_governance_defaults(Action {
            domain: KNOWLEDGE_DOMAIN.to_string(),
            operation: KNOWLEDGE_OP_SEARCH.to_string(),
            resource: json!({
                "roots": knowledge_search_roots(request),
            }),
            args: request.args.clone(),
            risk_level: RiskLevel::Low,
            requires_approval: false,
            dry_run: false,
        });
        if let Err(response) =
            self.ensure_action_allowed(request, &action, "knowledge_search_service")
        {
            return response;
        }

        let Some(query) = knowledge_search_query(request) else {
            return self.failed_knowledge_search_degraded(
                request,
                "",
                Vec::new(),
                PrivacyLevel::StrictLocal,
                RagResourceProfile::CpuOnly,
                "missing_query",
                "缺少可检索的主题，请提供 query 或更明确地说明要找什么内容。".to_string(),
            );
        };
        let (include_documents, include_images, include_videos) = knowledge_modalities(request);
        let knowledge_settings = match self.admin_store.knowledge_settings() {
            Ok(settings) => settings,
            Err(error) => {
                return self.failed_knowledge_search_degraded(
                    request,
                    &query,
                    knowledge_search_roots(request),
                    PrivacyLevel::StrictLocal,
                    RagResourceProfile::CpuOnly,
                    "knowledge_settings_unavailable",
                    error,
                )
            }
        };
        let privacy_level = match knowledge_privacy_level(request, knowledge_settings.privacy_level)
        {
            Ok(level) => level,
            Err(error) => {
                return self.failed_knowledge_search_degraded(
                    request,
                    &query,
                    knowledge_search_roots(request),
                    knowledge_settings.privacy_level,
                    knowledge_settings.default_resource_profile,
                    "invalid_privacy_level",
                    error,
                )
            }
        };
        if privacy_level_rank(privacy_level) > privacy_level_rank(knowledge_settings.privacy_level)
        {
            return self.failed_knowledge_search_degraded(
                request,
                &query,
                knowledge_search_roots(request),
                knowledge_settings.privacy_level,
                knowledge_settings.default_resource_profile,
                "privacy_policy_blocked",
                format!(
                    "请求的 privacy_level={} 超出 workspace 当前策略 {}；请先在 Harbor Assistant 调整云策略并保留审计记录。",
                    privacy_level_as_str(privacy_level),
                    privacy_level_as_str(knowledge_settings.privacy_level)
                ),
            );
        }
        let resource_profile = match knowledge_resource_profile(
            request,
            knowledge_settings.default_resource_profile,
        ) {
            Ok(profile) => profile,
            Err(error) => {
                return self.failed_knowledge_search_degraded(
                    request,
                    &query,
                    knowledge_search_roots(request),
                    privacy_level,
                    knowledge_settings.default_resource_profile,
                    "invalid_resource_profile",
                    error,
                )
            }
        };
        let search_request = KnowledgeSearchRequest {
            query,
            configured_roots: knowledge_settings.enabled_source_root_paths(),
            index_root: Some(knowledge_settings.index_root.clone()),
            roots: knowledge_search_roots(request),
            focus_paths: knowledge_focus_paths(request),
            include_documents,
            include_images,
            include_videos,
            limit: knowledge_result_limit(request),
            privacy_level,
            resource_profile,
            retrieval: knowledge_settings.retrieval.clone(),
            require_embeddings: knowledge_require_embeddings(request),
            latency_budget_ms: knowledge_latency_budget_ms(request),
        };

        match KnowledgeSearchService::search(search_request) {
            Ok(result) => {
                let message = format_knowledge_search_message(&result);
                let data = serde_json::to_value(&result).unwrap_or_else(|_| json!({}));
                if result.blockers.is_empty() {
                    self.completed(
                        request,
                        "knowledge_search_service",
                        RiskLevel::Low,
                        message,
                        data,
                        build_knowledge_search_artifacts(&result),
                        knowledge_search_next_actions(&result),
                    )
                } else {
                    self.failed_with_context(
                        request,
                        "knowledge_search_service",
                        RiskLevel::Low,
                        message,
                        data,
                        Vec::new(),
                    )
                }
            }
            Err(error) => self.failed(request, "knowledge_search_service", RiskLevel::Low, error),
        }
    }

    fn failed_knowledge_search_degraded(
        &self,
        request: &TaskRequest,
        query: impl Into<String>,
        roots: Vec<String>,
        privacy_level: PrivacyLevel,
        resource_profile: RagResourceProfile,
        reason: &str,
        message: String,
    ) -> TaskResponse {
        let degraded = KnowledgeSearchResponse::degraded(
            query,
            roots,
            privacy_level,
            resource_profile,
            reason,
            message.clone(),
        );
        self.failed_with_context(
            request,
            "knowledge_search_service",
            RiskLevel::Low,
            message,
            serde_json::to_value(&degraded).unwrap_or_else(|_| json!({})),
            Vec::new(),
        )
    }

    fn handle_rag_answer(&self, request: &TaskRequest) -> TaskResponse {
        let action = apply_governance_defaults(Action {
            domain: RAG_DOMAIN.to_string(),
            operation: RAG_OP_ANSWER.to_string(),
            resource: json!({
                "roots": knowledge_search_roots(request),
            }),
            args: request.args.clone(),
            risk_level: RiskLevel::Low,
            requires_approval: false,
            dry_run: false,
        });
        if let Err(response) = self.ensure_action_allowed(request, &action, "rag_answer_service") {
            return response;
        }

        let Some(query) = knowledge_search_query(request) else {
            return self.failed_rag_answer_degraded(
                request,
                "",
                Vec::new(),
                PrivacyLevel::StrictLocal,
                RagResourceProfile::CpuOnly,
                "missing_query",
                "缺少可回答的问题，请提供 query 或更明确地说明要回答什么。".to_string(),
            );
        };
        let (include_documents, include_images, include_videos) = knowledge_modalities(request);
        let knowledge_settings = match self.admin_store.knowledge_settings() {
            Ok(settings) => settings,
            Err(error) => {
                return self.failed_rag_answer_degraded(
                    request,
                    &query,
                    knowledge_search_roots(request),
                    PrivacyLevel::StrictLocal,
                    RagResourceProfile::CpuOnly,
                    "knowledge_settings_unavailable",
                    error,
                )
            }
        };
        let privacy_level = match knowledge_privacy_level(request, knowledge_settings.privacy_level)
        {
            Ok(level) => level,
            Err(error) => {
                return self.failed_rag_answer_degraded(
                    request,
                    &query,
                    knowledge_search_roots(request),
                    knowledge_settings.privacy_level,
                    knowledge_settings.default_resource_profile,
                    "invalid_privacy_level",
                    error,
                )
            }
        };
        if privacy_level_rank(privacy_level) > privacy_level_rank(knowledge_settings.privacy_level)
        {
            return self.failed_rag_answer_degraded(
                request,
                &query,
                knowledge_search_roots(request),
                knowledge_settings.privacy_level,
                knowledge_settings.default_resource_profile,
                "privacy_policy_blocked",
                format!(
                    "请求的 privacy_level={} 超出 workspace 当前策略 {}；请先在 Harbor Assistant 调整云策略并保留审计记录。",
                    privacy_level_as_str(privacy_level),
                    privacy_level_as_str(knowledge_settings.privacy_level)
                ),
            );
        }
        let resource_profile = match knowledge_resource_profile(
            request,
            knowledge_settings.default_resource_profile,
        ) {
            Ok(profile) => profile,
            Err(error) => {
                return self.failed_rag_answer_degraded(
                    request,
                    &query,
                    knowledge_search_roots(request),
                    privacy_level,
                    knowledge_settings.default_resource_profile,
                    "invalid_resource_profile",
                    error,
                )
            }
        };

        let search_request = KnowledgeSearchRequest {
            query: query.clone(),
            configured_roots: knowledge_settings.enabled_source_root_paths(),
            index_root: Some(knowledge_settings.index_root.clone()),
            roots: knowledge_search_roots(request),
            focus_paths: knowledge_focus_paths(request),
            include_documents,
            include_images,
            include_videos,
            limit: knowledge_result_limit(request),
            privacy_level,
            resource_profile,
            retrieval: knowledge_settings.retrieval.clone(),
            require_embeddings: knowledge_require_embeddings(request),
            latency_budget_ms: knowledge_latency_budget_ms(request),
        };

        let search_result = match KnowledgeSearchService::search(search_request) {
            Ok(result) => result,
            Err(error) => return self.failed(request, "rag_answer_service", RiskLevel::Low, error),
        };
        if !search_result.blockers.is_empty() {
            let message = search_result
                .blockers
                .first()
                .cloned()
                .unwrap_or_else(|| format_knowledge_search_message(&search_result));
            let citations = Vec::new();
            let data = build_rag_answer_data(
                &query,
                &message,
                "degraded",
                true,
                search_result
                    .degraded_reason
                    .as_deref()
                    .unwrap_or("retrieval_blocked"),
                &search_result,
                &citations,
                Value::Null,
                search_result.warnings.clone(),
                Value::Null,
            );
            return self.failed_with_context(
                request,
                "rag_answer_service",
                RiskLevel::Low,
                message,
                data,
                Vec::new(),
            );
        }

        let citations = rag_answer_context_citations(&search_result);
        if citations.is_empty() {
            let message = format!(
                "没有找到足够证据回答“{}”；请换个关键词，扩大已配置知识源，或先刷新索引。",
                query
            );
            let data = build_rag_answer_data(
                &query,
                &message,
                "degraded",
                true,
                "weak_evidence",
                &search_result,
                &citations,
                Value::Null,
                search_result.warnings.clone(),
                Value::Null,
            );
            return self.completed(
                request,
                "rag_answer_service",
                RiskLevel::Low,
                message,
                data,
                Vec::new(),
                vec!["换个关键词再问".to_string(), "先刷新知识索引".to_string()],
            );
        }

        let privacy_gateway_evaluation = PrivacyGateway::evaluate_rag_answer_cloud_context(
            &query,
            &citations,
            privacy_level,
            &workspace_id_for_request(request),
            &request.task_id,
            request.source.user_id.as_str(),
        );
        let cloud_profile_requested = resource_profile == RagResourceProfile::CloudAllowed;
        let mut capsule_prompt_used = false;
        let mut warnings = search_result.warnings.clone();
        let mut degraded_reason = search_result.degraded_reason.clone();
        let mut model = Value::Null;
        let mut answer = build_limited_rag_answer(&query, &citations);
        let mut prompt = build_rag_answer_prompt(&query, &citations);
        let mut skip_model = false;

        if cloud_profile_requested && privacy_level == PrivacyLevel::AllowRedactedCloud {
            if !privacy_gateway_evaluation.decision.cloud_allowed {
                degraded_reason.get_or_insert_with(|| "privacy_gateway_blocked".to_string());
                warnings.extend(privacy_gateway_evaluation.decision.warnings.clone());
                answer = format!(
                    "Privacy Gateway 已阻断云端回答：{}",
                    privacy_gateway_evaluation.decision.reason
                );
                skip_model = true;
            } else if let Some(capsule) = privacy_gateway_evaluation.semantic_capsule.as_ref() {
                prompt = capsule.to_cloud_prompt();
                capsule_prompt_used = true;
            } else {
                degraded_reason.get_or_insert_with(|| "privacy_gateway_blocked".to_string());
                warnings.push(
                    "Privacy Gateway 未能生成 semantic capsule，已跳过云端模型。".to_string(),
                );
                answer =
                    "Privacy Gateway 未能生成可上云的 semantic capsule，已降级为本地引用摘要。"
                        .to_string();
                skip_model = true;
            }
        }

        if !skip_model {
            match self.admin_store.load_or_create_state() {
                Ok(admin_state) => {
                    match rag_answer_model_state_for_policy(
                        &admin_state.models,
                        privacy_level,
                        resource_profile,
                    ) {
                        Ok(model_state) => {
                            let llm_result = run_llm_text_with_state_and_options(
                                &prompt,
                                &model_state,
                                &LlmTextOptions {
                                    purpose: Some("rag.answer".to_string()),
                                    system_prompt: Some(build_rag_answer_system_prompt()),
                                    temperature: Some(0.0),
                                    max_tokens: Some(rag_answer_max_tokens()),
                                    timeout: Some(Duration::from_millis(rag_answer_budget_ms())),
                                },
                            );
                            model = llm_execution_model_json(&llm_result);
                            let generated = normalize_rag_answer_text(&llm_result.text);
                            if llm_result.available && !generated.is_empty() {
                                if rag_answer_has_citation_marker(&generated, citations.len()) {
                                    answer = generated;
                                } else {
                                    let mut answered_by_cloud = false;
                                    if llm_selected_endpoint_kind(&llm_result) != Some("cloud") {
                                        if let Some(cloud_state) =
                                            cloud_only_llm_model_state_for_policy(
                                                &model_state,
                                                "retrieval.answer",
                                            )
                                        {
                                            let cloud_result = run_llm_text_with_state_and_options(
                                                &prompt,
                                                &cloud_state,
                                                &LlmTextOptions {
                                                    purpose: Some("rag.answer".to_string()),
                                                    system_prompt: Some(
                                                        build_rag_answer_system_prompt(),
                                                    ),
                                                    temperature: Some(0.0),
                                                    max_tokens: Some(rag_answer_max_tokens()),
                                                    timeout: Some(Duration::from_millis(
                                                        rag_answer_budget_ms(),
                                                    )),
                                                },
                                            );
                                            model = llm_execution_model_json(&cloud_result);
                                            let cloud_generated =
                                                normalize_rag_answer_text(&cloud_result.text);
                                            if cloud_result.available
                                                && !cloud_generated.is_empty()
                                                && rag_answer_has_citation_marker(
                                                    &cloud_generated,
                                                    citations.len(),
                                                )
                                            {
                                                answer = cloud_generated;
                                                answered_by_cloud = true;
                                            }
                                        }
                                    }
                                    if !answered_by_cloud {
                                        degraded_reason
                                            .get_or_insert_with(|| "uncited_answer".to_string());
                                        warnings.push(
                                            "LLM 输出缺少可解析 citation 标记，已降级为引用片段摘要。"
                                                .to_string(),
                                        );
                                    }
                                }
                            } else {
                                degraded_reason
                                    .get_or_insert_with(|| "llm_unavailable".to_string());
                                warnings.push(format!(
                                    "LLM 不可用，已降级为引用片段摘要：{}",
                                    llm_result.summary
                                ));
                            }
                        }
                        Err(error) => {
                            degraded_reason.get_or_insert_with(|| "llm_policy_blocked".to_string());
                            warnings.push(error);
                        }
                    }
                }
                Err(error) => {
                    degraded_reason.get_or_insert_with(|| "model_settings_unavailable".to_string());
                    warnings.push(format!("模型设置不可用，已降级为引用片段摘要：{error}"));
                }
            }
        }

        let degraded = search_result.degraded || degraded_reason.is_some();
        let status = if degraded { "degraded" } else { "completed" };
        let reason = degraded_reason.as_deref().unwrap_or("none");
        let privacy_gateway = privacy_gateway_evaluation.evidence_value(capsule_prompt_used);
        self.record_privacy_gateway_audit(
            request,
            &privacy_gateway_evaluation,
            capsule_prompt_used,
        );
        let data = build_rag_answer_data(
            &query,
            &answer,
            status,
            degraded,
            reason,
            &search_result,
            &citations,
            model,
            warnings,
            privacy_gateway,
        );
        self.completed(
            request,
            "rag_answer_service",
            RiskLevel::Low,
            answer,
            data,
            build_knowledge_search_artifacts(&search_result),
            rag_answer_next_actions(&search_result, degraded),
        )
    }

    fn failed_rag_answer_degraded(
        &self,
        request: &TaskRequest,
        query: impl Into<String>,
        roots: Vec<String>,
        privacy_level: PrivacyLevel,
        resource_profile: RagResourceProfile,
        reason: &str,
        message: String,
    ) -> TaskResponse {
        let search = KnowledgeSearchResponse::degraded(
            query,
            roots,
            privacy_level,
            resource_profile,
            reason,
            message.clone(),
        );
        let citations = Vec::new();
        let data = build_rag_answer_data(
            &search.query,
            &message,
            "degraded",
            true,
            reason,
            &search,
            &citations,
            Value::Null,
            Vec::new(),
            Value::Null,
        );
        self.failed_with_context(
            request,
            "rag_answer_service",
            RiskLevel::Low,
            message,
            data,
            Vec::new(),
        )
    }

    fn record_privacy_gateway_audit(
        &self,
        request: &TaskRequest,
        evaluation: &PrivacyGatewayEvaluation,
        capsule_prompt_used: bool,
    ) {
        let actor_id = first_non_empty(&[request.source.user_id.as_str()])
            .map(ToString::to_string)
            .unwrap_or_else(|| "system".to_string());
        let evidence = evaluation.evidence_value(capsule_prompt_used);
        let record = build_metadata_audit_record(
            evaluation.privacy_transform.workspace_id.clone(),
            "privacy_transform",
            evaluation.privacy_transform.privacy_transform_id.clone(),
            "privacy_gateway.rag_answer.evaluate",
            AuditActorKind::System,
            actor_id,
            json!({
                "task_id": request.task_id,
                "trace_id": request.trace_id,
                "purpose": evaluation.flow.purpose,
                "privacy_level": evaluation.flow.privacy_level,
                "policy_version": evaluation.flow.policy_version,
                "metadata_only": true,
            }),
            evidence,
        );
        let _ = self.admin_store.append_audit_record(record);
    }

    fn resolve_camera_target(&self, request: &TaskRequest) -> Result<ResolvedCameraTarget, String> {
        let targets = self.admin_store.registry_store().load_camera_targets()?;
        if targets.is_empty() {
            return Err("当前还没有已注册的摄像头，请先完成接入。".to_string());
        }

        if let Some(device_id) =
            first_string(&[&request.entity_refs, &request.args], &["/device_id"])
        {
            if let Some(target) = targets.iter().find(|target| target.device_id == device_id) {
                return Ok(target.clone());
            }
        }

        let hint = first_string(
            &[&request.entity_refs, &request.args],
            &["/device_hint", "/room", "/name"],
        )
        .or_else(|| {
            (!request.intent.raw_text.trim().is_empty()).then(|| request.intent.raw_text.clone())
        })
        .unwrap_or_default();
        let normalized = normalize_command_text(&hint);

        for target in &targets {
            let name = target.display_name.as_str();
            let room = target.room_name.as_deref().unwrap_or_default();
            if !name.is_empty() && normalized.contains(&name.replace(' ', "").to_lowercase()) {
                return Ok(target.clone());
            }
            if !room.is_empty() && normalized.contains(&room.replace(' ', "").to_lowercase()) {
                return Ok(target.clone());
            }
            for alias in room_aliases(name, room) {
                if normalized.contains(alias) {
                    return Ok(target.clone());
                }
            }
        }

        if let Ok(state) = self.admin_store.load_or_create_state() {
            if let Some(selected) = state.defaults.selected_camera_device_id.as_deref() {
                if let Some(target) = targets.iter().find(|target| target.device_id == selected) {
                    return Ok(target.clone());
                }
            }
        }

        targets
            .first()
            .cloned()
            .ok_or_else(|| "未找到可分析的摄像头设备。".to_string())
    }

    fn persist_snapshot_capture(
        &self,
        state: &AdminConsoleState,
        recording_policy: Option<&RecordingPolicy>,
        target: &ResolvedCameraTarget,
        snapshot: SnapshotCaptureResult,
    ) -> Result<SnapshotCaptureResult, String> {
        let capture_root = resolved_capture_directory(state, recording_policy)?;
        let image_bytes = BASE64_STANDARD
            .decode(snapshot.bytes_base64.as_bytes())
            .map_err(|error| format!("failed to decode snapshot bytes: {error}"))?;
        let output_path = build_snapshot_output_path(
            &capture_root,
            target,
            snapshot.captured_at_epoch_ms,
            snapshot.format.file_extension(),
        );
        fs::write(&output_path, &image_bytes).map_err(|error| {
            format!(
                "failed to write snapshot {}: {error}",
                output_path.display()
            )
        })?;

        let mut persisted = snapshot;
        persisted.storage.target = StorageTarget::HarborOsPool;
        persisted.storage.relative_path = output_path.to_string_lossy().to_string();
        persisted.index_sidecar_relative_path = output_path
            .with_extension("json")
            .to_string_lossy()
            .to_string();

        let ocr = run_ocr_with_state(&output_path, &state.models);
        let vlm = run_vlm_summary_with_state(&output_path, &state.models);
        let snapshot_tags = vec!["camera".to_string(), "snapshot".to_string()];
        write_media_index_sidecar(
            &output_path.with_extension("json"),
            &persisted.storage.relative_path,
            None,
            target,
            &ocr.text,
            &vlm.text,
            &snapshot_tags,
        )?;

        Ok(persisted)
    }

    fn persist_clip_ingest(
        &self,
        state: &AdminConsoleState,
        target: &ResolvedCameraTarget,
        clip: &ClipCaptureResult,
        keyframes: &[PathBuf],
    ) -> Result<(), String> {
        let clip_path = PathBuf::from(&clip.storage.relative_path);
        let clip_tags = vec!["video".to_string(), "clip".to_string()];
        write_media_index_sidecar(
            &clip_path.with_extension("json"),
            &clip.storage.relative_path,
            None,
            target,
            "",
            &format!(
                "短视频片段，时长 {} 秒，共提取 {} 张关键帧。",
                clip.clip_length_seconds,
                keyframes.len()
            ),
            &clip_tags,
        )?;

        for keyframe in keyframes {
            let ocr = run_ocr_with_state(keyframe, &state.models);
            let vlm = run_vlm_summary_with_state(keyframe, &state.models);
            let keyframe_tags = vec!["video".to_string(), "keyframe".to_string()];
            write_media_index_sidecar(
                &keyframe.with_extension("json"),
                keyframe.to_string_lossy().as_ref(),
                Some(&clip.storage.relative_path),
                target,
                &ocr.text,
                &vlm.text,
                &keyframe_tags,
            )?;
        }

        Ok(())
    }

    fn completed(
        &self,
        request: &TaskRequest,
        executor_used: &str,
        risk_level: RiskLevel,
        message: String,
        data: Value,
        artifacts: Vec<TaskArtifact>,
        next_actions: Vec<String>,
    ) -> TaskResponse {
        self.completed_with_context(
            request,
            executor_used,
            risk_level,
            message,
            data,
            artifacts,
            Vec::new(),
            next_actions,
        )
    }

    fn completed_with_context(
        &self,
        request: &TaskRequest,
        executor_used: &str,
        risk_level: RiskLevel,
        message: String,
        data: Value,
        artifacts: Vec<TaskArtifact>,
        events: Vec<Value>,
        next_actions: Vec<String>,
    ) -> TaskResponse {
        TaskResponse {
            task_id: request.task_id.clone(),
            trace_id: request.trace_id.clone(),
            status: TaskStatus::Completed,
            executor_used: executor_used.to_string(),
            risk_level,
            result: TaskResultEnvelope {
                message,
                data,
                artifacts,
                events,
                next_actions,
            },
            audit_ref: new_audit_ref(),
            missing_fields: Vec::new(),
            prompt: None,
            resume_token: None,
        }
    }

    fn needs_input(
        &self,
        request: &TaskRequest,
        executor_used: &str,
        risk_level: RiskLevel,
        prompt: String,
        missing_fields: Vec<String>,
        resume_token: String,
    ) -> TaskResponse {
        self.needs_input_with_context(
            request,
            executor_used,
            risk_level,
            prompt,
            missing_fields,
            resume_token,
            Value::Null,
            Vec::new(),
            vec!["密码 xxxxxx".to_string()],
        )
    }

    fn needs_input_with_context(
        &self,
        request: &TaskRequest,
        executor_used: &str,
        risk_level: RiskLevel,
        prompt: String,
        missing_fields: Vec<String>,
        resume_token: String,
        data: Value,
        events: Vec<Value>,
        next_actions: Vec<String>,
    ) -> TaskResponse {
        self.needs_input_with_artifacts_context(
            request,
            executor_used,
            risk_level,
            prompt,
            missing_fields,
            resume_token,
            data,
            Vec::new(),
            events,
            next_actions,
        )
    }

    fn needs_input_with_artifacts_context(
        &self,
        request: &TaskRequest,
        executor_used: &str,
        risk_level: RiskLevel,
        prompt: String,
        missing_fields: Vec<String>,
        resume_token: String,
        data: Value,
        artifacts: Vec<TaskArtifact>,
        events: Vec<Value>,
        next_actions: Vec<String>,
    ) -> TaskResponse {
        TaskResponse {
            task_id: request.task_id.clone(),
            trace_id: request.trace_id.clone(),
            status: TaskStatus::NeedsInput,
            executor_used: executor_used.to_string(),
            risk_level,
            result: TaskResultEnvelope {
                message: prompt.clone(),
                data,
                artifacts,
                events,
                next_actions,
            },
            audit_ref: new_audit_ref(),
            missing_fields,
            prompt: Some(prompt),
            resume_token: Some(resume_token),
        }
    }

    fn failed(
        &self,
        request: &TaskRequest,
        executor_used: &str,
        risk_level: RiskLevel,
        message: String,
    ) -> TaskResponse {
        self.failed_with_context(
            request,
            executor_used,
            risk_level,
            message,
            Value::Null,
            Vec::new(),
        )
    }

    fn failed_with_context(
        &self,
        request: &TaskRequest,
        executor_used: &str,
        risk_level: RiskLevel,
        message: String,
        data: Value,
        events: Vec<Value>,
    ) -> TaskResponse {
        TaskResponse {
            task_id: request.task_id.clone(),
            trace_id: request.trace_id.clone(),
            status: TaskStatus::Failed,
            executor_used: executor_used.to_string(),
            risk_level,
            result: TaskResultEnvelope {
                message,
                data,
                artifacts: Vec::new(),
                events,
                next_actions: Vec::new(),
            },
            audit_ref: new_audit_ref(),
            missing_fields: Vec::new(),
            prompt: None,
            resume_token: None,
        }
    }

    fn ensure_action_allowed(
        &self,
        request: &TaskRequest,
        action: &Action,
        executor_used: &str,
    ) -> Result<(), TaskResponse> {
        let autonomy_level = effective_autonomy_level(request);
        let approval_manager = approval_manager_for_level(autonomy_level);
        if !approval_manager.risk_allowed(effective_risk_level(action)) {
            let event = self.serialize_event_record(&build_task_event_record(
                request,
                &step_id_for_request(request),
                "task.autonomy_blocked",
                EventSeverity::Warning,
                json!({
                    "executor_used": executor_used,
                    "autonomy_level": autonomy_level_label(autonomy_level),
                    "policy_ref": format!("{}.{}", action.domain, action.operation),
                    "risk_level": serde_json::to_value(effective_risk_level(action)).unwrap_or(Value::Null),
                }),
            ));
            return Err(self.failed_with_context(
                request,
                executor_used,
                effective_risk_level(action),
                format!(
                    "当前任务处于 {} 模式，无法执行需要写入或变更的操作。",
                    autonomy_level_label(autonomy_level)
                ),
                json!({
                    "error": "AUTONOMY_BLOCKED",
                    "autonomy_level": autonomy_level_label(autonomy_level),
                    "policy_ref": format!("{}.{}", action.domain, action.operation),
                }),
                vec![event],
            ));
        }

        let approval_tickets = self
            .conversation_store
            .approvals_for_task(&request.task_id)
            .unwrap_or_default();
        let pending_approval = approval_tickets
            .iter()
            .find(|approval| approval.status == ApprovalStatus::Pending)
            .cloned();
        let approval_context = approval_context_for_request(request, pending_approval.as_ref());
        let approval_context_ref = approval_context.as_ref();

        if let Err(violation) = enforce(action, approval_context_ref) {
            let approval_id = pending_approval
                .as_ref()
                .map(|approval| approval.approval_id.clone())
                .unwrap_or_else(new_approval_id);
            let ticket = ApprovalTicket {
                approval_id: approval_id.clone(),
                task_id: request.task_id.clone(),
                trace_id: request.trace_id.clone(),
                route_key: request.source.route_key.clone(),
                policy_ref: format!("{}.{}", action.domain, action.operation),
                requester_user_id: request.source.user_id.clone(),
                approver_user_id: None,
                status: ApprovalStatus::Pending,
                reason: violation.message.clone(),
                requested_at: Some(current_timestamp()),
                decided_at: None,
            };
            let _ = self.conversation_store.save_approval(&ticket);
            let policy_ref = format!("{}.{}", action.domain, action.operation);
            let event = self.serialize_event_record(&build_task_event_record(
                request,
                &step_id_for_request(request),
                "task.approval_required",
                EventSeverity::Warning,
                json!({
                    "executor_used": executor_used,
                    "policy_violation": {
                        "code": violation.code.clone(),
                        "message": violation.message.clone(),
                    },
                    "approval_ticket": ticket.clone(),
                }),
            ));
            return Err(self.needs_input_with_context(
                request,
                executor_used,
                action.risk_level,
                "这个操作需要审批，请带 approval_token 重新提交。".to_string(),
                vec!["approval_token".to_string()],
                approval_id.clone(),
                json!({
                    "approval_ticket": ticket,
                    "policy_ref": policy_ref,
                }),
                vec![event],
                vec![format!("approval_token {approval_id}")],
            ));
        }

        if (action_requires_approval(action) || pending_approval.is_some())
            && request_approval_token(request).is_some()
        {
            let _ = self.conversation_store.resolve_pending_approvals(
                &request.task_id,
                request_approver_id(request),
                Some(current_timestamp()),
            );
        }

        Ok(())
    }

    fn append_task_lifecycle_event(
        &self,
        request: &TaskRequest,
        tracking: &TaskRuntimeTracking,
        response: &mut TaskResponse,
    ) {
        let (event_type, severity) = match response.status {
            TaskStatus::Completed => ("task.completed", EventSeverity::Info),
            TaskStatus::NeedsInput => ("task.needs_input", EventSeverity::Warning),
            TaskStatus::Failed => ("task.failed", EventSeverity::Error),
        };
        response
            .result
            .events
            .push(self.serialize_event_record(&build_task_event_record(
                request,
                &tracking.step_id,
                event_type,
                severity,
                json!({
                    "executor_used": response.executor_used.clone(),
                    "risk_level": serde_json::to_value(response.risk_level).unwrap_or(Value::Null),
                    "message": response.result.message.clone(),
                    "missing_fields": response.missing_fields.clone(),
                    "continuation_token": response.resume_token.clone(),
                    "audit_ref": response.audit_ref.clone(),
                }),
            )));
    }

    fn build_notification_request(
        &self,
        request: &TaskRequest,
        event_type: &str,
        target: &ResolvedCameraTarget,
        payload: &Value,
        artifacts: &[TaskArtifact],
    ) -> Option<NotificationRequest> {
        let route_key = first_string(
            &[&request.args],
            &["/notification/route_key", "/destination/route_key"],
        )
        .or_else(|| {
            let value = request.source.route_key.trim();
            (!value.is_empty()).then(|| value.to_string())
        })
        .or_else(|| {
            self.conversation_store
                .load_session(&conversation_handle_for_request(request))
                .ok()
                .flatten()
                .map(|session| session.route_key.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_default();
        let legacy_destination = first_string(
            &[&request.args],
            &["/notification/destination", "/notification_channel"],
        );
        let platform_hint = notification_platform_from_value(
            payload
                .pointer("/notification_channel")
                .and_then(Value::as_str)
                .unwrap_or("im_bridge"),
        );
        let payload_format = notification_payload_format_from_value(
            payload
                .pointer("/notification_format")
                .and_then(Value::as_str)
                .unwrap_or("plain_text"),
        );
        let title = string_at_paths(payload, &["/notification_card/header/title/content"])
            .unwrap_or_else(|| format!("{} AI 分析", target.display_name));
        let body = string_at_paths(payload, &["/summary", "/detection_summary"])
            .unwrap_or_else(|| format!("{} 分析完成", target.display_name));
        let requested_mode = first_string(
            &[&request.args],
            &["/notification/delivery/mode", "/notification/mode"],
        )
        .map(|value| notification_delivery_mode_from_value(&value))
        .unwrap_or(NotificationDeliveryMode::Send);
        let reply_to_message_id = first_string(
            &[&request.args],
            &[
                "/notification/delivery/reply_to_message_id",
                "/notification/reply_to_message_id",
            ],
        )
        .or_else(|| {
            let message_id = task_message_id(request);
            (!message_id.is_empty()).then_some(message_id)
        })
        .unwrap_or_default();
        let update_message_id = first_string(
            &[&request.args],
            &[
                "/notification/delivery/update_message_id",
                "/notification/update_message_id",
            ],
        )
        .unwrap_or_default();
        let (delivery_mode, reply_to_message_id, update_message_id) = match requested_mode {
            NotificationDeliveryMode::Reply if !reply_to_message_id.is_empty() => (
                NotificationDeliveryMode::Reply,
                reply_to_message_id,
                String::new(),
            ),
            NotificationDeliveryMode::Update if !update_message_id.is_empty() => (
                NotificationDeliveryMode::Update,
                String::new(),
                update_message_id,
            ),
            _ => (NotificationDeliveryMode::Send, String::new(), String::new()),
        };
        let destination = if matches!(platform_hint.as_deref(), Some("local_ui")) {
            NotificationDestination {
                kind: NotificationDestinationKind::LocalUi,
                route_key: String::new(),
                id: legacy_destination
                    .clone()
                    .unwrap_or_else(|| request.source.conversation_id.clone()),
                platform: "local_ui".to_string(),
                recipient: None,
            }
        } else if !route_key.is_empty() {
            NotificationDestination {
                kind: NotificationDestinationKind::Conversation,
                route_key,
                id: String::new(),
                platform: String::new(),
                recipient: None,
            }
        } else {
            let state = self.admin_store.load_or_create_state().ok()?;
            proactive_notification_destination(request, &state)?
        };

        let mut notification_request = NotificationRequest {
            notification_id: String::new(),
            trace_id: request.trace_id.clone(),
            source: NotificationSource {
                service: "harborbeacon".to_string(),
                module: "task_api".to_string(),
                event_type: event_type.to_string(),
            },
            destination,
            content: NotificationContent {
                title,
                body,
                payload_format,
                structured_payload: payload
                    .pointer("/notification_card")
                    .cloned()
                    .unwrap_or(Value::Null),
                attachments: artifacts
                    .iter()
                    .filter_map(task_artifact_to_notification_attachment)
                    .collect(),
            },
            delivery: NotificationDelivery {
                mode: delivery_mode,
                reply_to_message_id,
                update_message_id,
                idempotency_key: String::new(),
            },
            metadata: NotificationMetadata {
                correlation_id: request.trace_id.clone(),
            },
        };
        let notification_hash = notification_request_hash(&notification_request);
        notification_request.notification_id = format!("notif_{}", &notification_hash[..24]);
        notification_request.delivery.idempotency_key =
            format!("idem_{}", &notification_hash[..24]);
        Some(notification_request)
    }

    fn deliver_notification_request(
        &self,
        notification_request: &NotificationRequest,
    ) -> NotificationDeliveryOutcome {
        let service = match NotificationDeliveryService::new() {
            Ok(service) => service,
            Err(error) => {
                return NotificationDeliveryOutcome {
                    event_type: "task.notification_failed",
                    severity: EventSeverity::Error,
                    payload: json!({
                        "status": "failed",
                        "error": error,
                    }),
                };
            }
        };

        notification_delivery_outcome(notification_request, service.deliver(notification_request))
    }

    fn serialize_event_record(&self, event: &EventRecord) -> Value {
        serde_json::to_value(event).unwrap_or(Value::Null)
    }

    fn persist_vision_media_assets(
        &self,
        request: &TaskRequest,
        target: &ResolvedCameraTarget,
        payload: &mut Value,
    ) -> Result<(), String> {
        let snapshot_image_path = string_at_paths(payload, &["/snapshot/image_path"]);
        let annotated_image_path = string_at_paths(payload, &["/snapshot/annotated_image_path"]);
        if snapshot_image_path.is_none() && annotated_image_path.is_none() {
            return Ok(());
        }

        let snapshot_mime_type =
            string_at_paths(payload, &["/snapshot/mime_type"]).unwrap_or_else(|| {
                snapshot_image_path
                    .as_deref()
                    .and_then(mime_type_from_path)
                    .unwrap_or_else(|| "image/jpeg".to_string())
            });
        let captured_at = u64_at_paths(payload, &["/snapshot/captured_at_epoch_ms"])
            .map(|value| value.to_string())
            .unwrap_or_else(current_timestamp_millis);
        let source_storage = payload
            .pointer("/snapshot/source_storage")
            .cloned()
            .unwrap_or(Value::Null);
        let snapshot_ingest_metadata = payload
            .pointer("/snapshot/ingest_metadata")
            .cloned()
            .unwrap_or(Value::Null);
        let snapshot_byte_size = u64_at_paths(payload, &["/snapshot/byte_size"]);
        let detection_summary = string_at_paths(payload, &["/detection_summary"]);
        let summary = string_at_paths(payload, &["/summary"]);
        let summary_source = string_at_paths(payload, &["/summary_source"]);

        let snapshot_media_asset_id = if let Some(path) = snapshot_image_path.as_deref() {
            let media_asset = build_vision_image_media_asset(
                request,
                target,
                path,
                snapshot_mime_type.as_str(),
                MediaAssetKind::Snapshot,
                None,
                "analysis_snapshot",
                &captured_at,
                snapshot_byte_size,
                source_storage.clone(),
                snapshot_ingest_metadata.clone(),
                detection_summary.as_deref(),
                summary.as_deref(),
                summary_source.as_deref(),
            );
            let asset_id = media_asset.asset_id.clone();
            self.conversation_store.save_media_asset(&media_asset)?;
            Some(asset_id)
        } else {
            None
        };

        let annotated_media_asset_id = if let Some(path) = annotated_image_path.as_deref() {
            let media_asset = build_vision_image_media_asset(
                request,
                target,
                path,
                snapshot_mime_type.as_str(),
                MediaAssetKind::Derived,
                snapshot_media_asset_id.clone(),
                "analysis_annotation",
                &captured_at,
                None,
                source_storage.clone(),
                snapshot_ingest_metadata.clone(),
                detection_summary.as_deref(),
                summary.as_deref(),
                summary_source.as_deref(),
            );
            let asset_id = media_asset.asset_id.clone();
            self.conversation_store.save_media_asset(&media_asset)?;
            Some(asset_id)
        } else {
            None
        };

        if let Some(snapshot_object) = payload
            .pointer_mut("/snapshot")
            .and_then(Value::as_object_mut)
        {
            if let Some(asset_id) = snapshot_media_asset_id {
                snapshot_object.insert("media_asset_id".to_string(), Value::String(asset_id));
            }
            if let Some(asset_id) = annotated_media_asset_id {
                snapshot_object.insert(
                    "annotated_media_asset_id".to_string(),
                    Value::String(asset_id),
                );
            }
        }

        Ok(())
    }

    fn begin_task_tracking(&self, request: &TaskRequest) -> TaskRuntimeTracking {
        let started_at = current_timestamp();
        let tracking = TaskRuntimeTracking {
            session_id: conversation_handle_for_request(request),
            step_id: step_id_for_request(request),
            started_at: started_at.clone(),
        };
        let session = self.build_session_record(request, &tracking, None);
        let _ = self.conversation_store.save_session(&session);

        let mut task_run = self
            .conversation_store
            .load_task_run(&request.task_id)
            .ok()
            .flatten()
            .unwrap_or_else(|| TaskRun {
                task_id: request.task_id.clone(),
                workspace_id: workspace_id_for_request(request),
                session_id: tracking.session_id.clone(),
                source_channel: request.source.channel.clone(),
                domain: request.intent.domain.clone(),
                action: request.intent.action.clone(),
                intent_text: request.intent.raw_text.clone(),
                entity_refs: request.entity_refs.clone(),
                args: request.args.clone(),
                autonomy_level: effective_autonomy_level_for_task_run(request),
                status: TaskRunStatus::Queued,
                risk_level: expected_risk_level(request),
                requires_approval: effective_requires_approval(request),
                started_at: Some(started_at.clone()),
                completed_at: None,
                metadata: Value::Null,
            });
        task_run.workspace_id = workspace_id_for_request(request);
        task_run.session_id = tracking.session_id.clone();
        task_run.source_channel = request.source.channel.clone();
        task_run.domain = request.intent.domain.clone();
        task_run.action = request.intent.action.clone();
        task_run.intent_text = request.intent.raw_text.clone();
        task_run.entity_refs = request.entity_refs.clone();
        task_run.args = request.args.clone();
        task_run.autonomy_level = effective_autonomy_level_for_task_run(request);
        task_run.status = TaskRunStatus::Running;
        task_run.risk_level = expected_risk_level(request);
        task_run.requires_approval = effective_requires_approval(request);
        if task_run.started_at.is_none() {
            task_run.started_at = Some(started_at.clone());
        }
        task_run.completed_at = None;
        task_run.metadata = build_task_run_metadata(request, &tracking.step_id);
        let _ = self.conversation_store.save_task_run(&task_run);

        let mut task_step = self
            .conversation_store
            .load_task_step(&tracking.step_id)
            .ok()
            .flatten()
            .unwrap_or_else(|| TaskStepRun {
                step_id: tracking.step_id.clone(),
                task_id: request.task_id.clone(),
                trace_id: request.trace_id.clone(),
                route_key: request.source.route_key.clone(),
                domain: request.intent.domain.clone(),
                operation: request.intent.action.clone(),
                route: ExecutionRoute::Local,
                executor_used: "task_api_dispatch".to_string(),
                status: TaskStepRunStatus::Pending,
                input_payload: Value::Null,
                output_payload: Value::Null,
                error_code: None,
                error_message: None,
                audit_ref: None,
                started_at: Some(started_at.clone()),
                ended_at: None,
            });
        task_step.task_id = request.task_id.clone();
        task_step.trace_id = request.trace_id.clone();
        task_step.route_key = request.source.route_key.clone();
        task_step.domain = request.intent.domain.clone();
        task_step.operation = request.intent.action.clone();
        task_step.route = ExecutionRoute::Local;
        task_step.executor_used = "task_api_dispatch".to_string();
        task_step.status = TaskStepRunStatus::Executing;
        task_step.input_payload = build_step_input_payload(request);
        task_step.output_payload = Value::Null;
        task_step.error_code = None;
        task_step.error_message = None;
        task_step.audit_ref = None;
        if task_step.started_at.is_none() {
            task_step.started_at = Some(started_at);
        }
        task_step.ended_at = None;
        let _ = self.conversation_store.save_task_step(&task_step);

        tracking
    }

    fn finish_task_tracking(
        &self,
        request: &TaskRequest,
        response: &TaskResponse,
        tracking: &TaskRuntimeTracking,
    ) -> Result<(), String> {
        let finished_at = current_timestamp();
        let mut task_run = self
            .conversation_store
            .load_task_run(&request.task_id)?
            .unwrap_or_else(|| TaskRun {
                task_id: request.task_id.clone(),
                workspace_id: workspace_id_for_request(request),
                session_id: tracking.session_id.clone(),
                source_channel: request.source.channel.clone(),
                domain: request.intent.domain.clone(),
                action: request.intent.action.clone(),
                intent_text: request.intent.raw_text.clone(),
                entity_refs: request.entity_refs.clone(),
                args: request.args.clone(),
                autonomy_level: effective_autonomy_level_for_task_run(request),
                status: TaskRunStatus::Queued,
                risk_level: response.risk_level,
                requires_approval: effective_requires_approval(request),
                started_at: Some(tracking.started_at.clone()),
                completed_at: None,
                metadata: build_task_run_metadata(request, &tracking.step_id),
            });
        task_run.workspace_id = workspace_id_for_request(request);
        task_run.session_id = tracking.session_id.clone();
        task_run.source_channel = request.source.channel.clone();
        task_run.domain = request.intent.domain.clone();
        task_run.action = request.intent.action.clone();
        task_run.intent_text = request.intent.raw_text.clone();
        task_run.entity_refs = request.entity_refs.clone();
        task_run.args = request.args.clone();
        task_run.autonomy_level = effective_autonomy_level_for_task_run(request);
        task_run.status = task_run_status_from_response(response.status);
        task_run.risk_level = response.risk_level;
        task_run.requires_approval = effective_requires_approval(request);
        if task_run.started_at.is_none() {
            task_run.started_at = Some(tracking.started_at.clone());
        }
        task_run.completed_at = task_run_completed_at(response.status, &finished_at);
        task_run.metadata = build_task_run_metadata(request, &tracking.step_id);
        self.conversation_store.save_task_run(&task_run)?;

        let (step_domain, step_operation) = step_identity(request, response);
        let execution_route = execution_route_for_executor(&response.executor_used);
        let mut task_step = self
            .conversation_store
            .load_task_step(&tracking.step_id)?
            .unwrap_or_else(|| TaskStepRun {
                step_id: tracking.step_id.clone(),
                task_id: request.task_id.clone(),
                trace_id: request.trace_id.clone(),
                route_key: request.source.route_key.clone(),
                domain: step_domain.clone(),
                operation: step_operation.clone(),
                route: execution_route,
                executor_used: response.executor_used.clone(),
                status: TaskStepRunStatus::Pending,
                input_payload: build_step_input_payload(request),
                output_payload: Value::Null,
                error_code: None,
                error_message: None,
                audit_ref: Some(response.audit_ref.clone()),
                started_at: Some(tracking.started_at.clone()),
                ended_at: None,
            });
        task_step.task_id = request.task_id.clone();
        task_step.trace_id = request.trace_id.clone();
        task_step.route_key = request.source.route_key.clone();
        task_step.domain = step_domain;
        task_step.operation = step_operation;
        task_step.route = execution_route;
        task_step.executor_used = response.executor_used.clone();
        task_step.status = task_step_status_from_response(response.status);
        task_step.input_payload = build_step_input_payload(request);
        task_step.output_payload = build_step_output_payload(response);
        task_step.error_code = match response.status {
            TaskStatus::Failed => response_error_code(response)
                .or_else(|| Some(format!("{}_failed", response.executor_used))),
            _ => None,
        };
        task_step.error_message = match response.status {
            TaskStatus::Failed => Some(response.result.message.clone()),
            _ => None,
        };
        task_step.audit_ref = Some(response.audit_ref.clone());
        if task_step.started_at.is_none() {
            task_step.started_at = Some(tracking.started_at.clone());
        }
        task_step.ended_at = Some(finished_at.clone());
        self.conversation_store.save_task_step(&task_step)?;

        let artifact_records =
            build_artifact_records(request, &tracking.step_id, &response.result.artifacts);
        self.conversation_store.replace_artifacts_for_step(
            &request.task_id,
            Some(&tracking.step_id),
            &artifact_records,
        )?;
        let event_records =
            build_event_records(request, &tracking.step_id, &response.result.events);
        self.conversation_store.replace_events_for_step(
            &request.task_id,
            Some(&tracking.step_id),
            &event_records,
        )?;

        let session = self.build_session_record(request, tracking, response.resume_token.clone());
        self.conversation_store.save_session(&session)?;
        Ok(())
    }

    fn build_session_record(
        &self,
        request: &TaskRequest,
        tracking: &TaskRuntimeTracking,
        resume_token: Option<String>,
    ) -> ConversationSession {
        let mut session = self
            .conversation_store
            .load_session(&tracking.session_id)
            .ok()
            .flatten()
            .unwrap_or_else(|| ConversationSession {
                session_id: tracking.session_id.clone(),
                workspace_id: workspace_id_for_request(request),
                channel: request.source.channel.clone(),
                surface: request.source.surface.clone(),
                conversation_id: request.source.conversation_id.clone(),
                user_id: request.source.user_id.clone(),
                route_key: request.source.route_key.clone(),
                last_message_id: task_message_id(request),
                chat_type: task_chat_type(request),
                state: Value::Null,
                resume_token: None,
                expires_at: None,
            });
        session.workspace_id = workspace_id_for_request(request);
        session.channel = request.source.channel.clone();
        session.surface = request.source.surface.clone();
        session.conversation_id = request.source.conversation_id.clone();
        session.user_id = request.source.user_id.clone();
        if !request.source.route_key.trim().is_empty() {
            session.route_key = request.source.route_key.clone();
        }
        let message_id = task_message_id(request);
        if !message_id.is_empty() {
            session.last_message_id = message_id;
        }
        let chat_type = task_chat_type(request);
        if !chat_type.is_empty() {
            session.chat_type = chat_type;
        }
        session.state = self
            .load_conversation(request)
            .and_then(|conversation| {
                session_state_value_from_conversation(&conversation, Some(&session)).ok()
            })
            .unwrap_or(Value::Null);
        session.resume_token = resume_token;
        session.expires_at = None;
        session
    }

    fn hub(&self) -> CameraHubService {
        CameraHubService::new(self.admin_store.clone())
    }

    fn load_conversation(&self, request: &TaskRequest) -> Option<TaskConversationState> {
        let session_id = conversation_handle_for_request(request);
        let key = conversation_key(request).unwrap_or_else(|| session_id.clone());
        self.conversation_store
            .load_for_session(&session_id, Some(&key))
            .ok()
            .flatten()
    }

    fn load_or_create_conversation(&self, request: &TaskRequest) -> TaskConversationState {
        let session_id = conversation_handle_for_request(request);
        let key = conversation_key(request).unwrap_or(session_id);
        self.load_conversation(request)
            .unwrap_or(TaskConversationState {
                key,
                ..Default::default()
            })
    }

    fn remember_family_memory_event(
        &self,
        request: &TaskRequest,
        stored: &StoredLocalVisionEvent,
    ) -> Result<(), String> {
        let mut conversation = self.load_or_create_conversation(request);
        conversation.set_recent_family_memory_event(Some(RecentFamilyMemoryEventState {
            event_id: stored.event.event_id.clone(),
            camera_id: stored.event.camera_id.clone(),
            event_type: stored.event.event_type.clone(),
            summary: redact_task_api_text(&stored.event.summary)
                .chars()
                .take(160)
                .collect(),
            referenced_at_epoch_ms: current_epoch_ms(),
        }));
        self.save_conversation(request, &conversation)
    }

    fn recent_family_memory_event_for_request(
        &self,
        request: &TaskRequest,
    ) -> Option<RecentFamilyMemoryEventState> {
        self.load_conversation(request)
            .and_then(|conversation| conversation.recent_family_memory_event())
            .filter(|recent| {
                !recent.event_id.trim().is_empty()
                    && current_epoch_ms().saturating_sub(recent.referenced_at_epoch_ms)
                        <= RECENT_FAMILY_MEMORY_EVENT_WINDOW_MS
            })
    }

    fn save_conversation(
        &self,
        request: &TaskRequest,
        conversation: &TaskConversationState,
    ) -> Result<(), String> {
        let session_id = conversation_handle_for_request(request);
        let session = self
            .conversation_store
            .load_session(&session_id)?
            .unwrap_or_else(|| ConversationSession {
                session_id,
                workspace_id: workspace_id_for_request(request),
                channel: request.source.channel.clone(),
                surface: request.source.surface.clone(),
                conversation_id: request.source.conversation_id.clone(),
                user_id: request.source.user_id.clone(),
                route_key: request.source.route_key.clone(),
                last_message_id: task_message_id(request),
                chat_type: task_chat_type(request),
                state: Value::Null,
                resume_token: None,
                expires_at: None,
            });
        self.conversation_store
            .save_for_session(&session, conversation)
    }

    fn load_approval_context(
        &self,
        approval_id: &str,
    ) -> Result<(ApprovalTicket, TaskRun, Option<ConversationSession>), String> {
        let approval = self
            .conversation_store
            .load_approval(approval_id)?
            .ok_or_else(|| format!("approval not found: {approval_id}"))?;
        let task_run = self
            .conversation_store
            .load_task_run(&approval.task_id)?
            .ok_or_else(|| format!("task run not found for approval: {}", approval.task_id))?;
        let session = if task_run.session_id.trim().is_empty() {
            None
        } else {
            self.conversation_store.load_session(&task_run.session_id)?
        };
        Ok((approval, task_run, session))
    }

    fn load_approval_summary(
        &self,
        approval: &ApprovalTicket,
    ) -> Result<TaskApprovalSummary, String> {
        let task_run = self
            .conversation_store
            .load_task_run(&approval.task_id)?
            .ok_or_else(|| format!("task run not found for approval: {}", approval.task_id))?;
        let session = if task_run.session_id.trim().is_empty() {
            None
        } else {
            self.conversation_store.load_session(&task_run.session_id)?
        };
        Ok(build_approval_summary(
            approval,
            &task_run,
            session.as_ref(),
        ))
    }

    fn build_approval_resume_request(
        &self,
        approval: &ApprovalTicket,
        task_run: &TaskRun,
        session: Option<&ConversationSession>,
        approver_user_id: Option<String>,
    ) -> TaskRequest {
        let mut args = task_run.args.clone();
        inject_approval_args(
            &mut args,
            &approval.approval_id,
            approver_user_id.as_deref(),
        );
        let trace_id = task_run
            .metadata
            .pointer("/trace_id")
            .and_then(Value::as_str)
            .map(|value| value.to_string())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| task_run.task_id.clone());
        let surface = session
            .map(|session| session.surface.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| {
                task_run
                    .metadata
                    .pointer("/surface")
                    .and_then(Value::as_str)
                    .map(|value| value.trim().to_string())
                    .filter(|value| !value.is_empty())
            })
            .unwrap_or_else(|| "task_api".to_string());

        TaskRequest {
            task_id: task_run.task_id.clone(),
            trace_id,
            step_id: approval_resume_step_id(&approval.approval_id),
            source: TaskSource {
                channel: task_run.source_channel.clone(),
                surface,
                conversation_id: session
                    .map(|session| session.conversation_id.clone())
                    .unwrap_or_default(),
                user_id: session
                    .map(|session| session.user_id.clone())
                    .unwrap_or_else(|| approval.requester_user_id.clone()),
                session_id: task_run.session_id.clone(),
                route_key: source_route_key_from_context(task_run, session),
            },
            intent: TaskIntent {
                domain: task_run.domain.clone(),
                action: task_run.action.clone(),
                raw_text: task_run.intent_text.clone(),
            },
            entity_refs: task_run.entity_refs.clone(),
            args,
            autonomy: TaskAutonomy {
                level: normalize_task_autonomy_level(&task_run.autonomy_level),
            },
            message: None,
        }
    }

    fn record_approval_decision_event(
        &self,
        approval: &ApprovalTicket,
        task_run: &TaskRun,
        session: Option<&ConversationSession>,
        event_type: &str,
        severity: EventSeverity,
        approver_user_id: Option<String>,
    ) -> Result<(), String> {
        let request = TaskRequest {
            task_id: task_run.task_id.clone(),
            trace_id: task_run
                .metadata
                .pointer("/trace_id")
                .and_then(Value::as_str)
                .unwrap_or(task_run.task_id.as_str())
                .to_string(),
            step_id: approval_event_step_id(&approval.approval_id),
            source: TaskSource {
                channel: task_run.source_channel.clone(),
                surface: session
                    .map(|session| session.surface.clone())
                    .unwrap_or_else(|| {
                        task_run
                            .metadata
                            .pointer("/surface")
                            .and_then(Value::as_str)
                            .unwrap_or("task_api")
                            .to_string()
                    }),
                conversation_id: session
                    .map(|session| session.conversation_id.clone())
                    .unwrap_or_default(),
                user_id: session
                    .map(|session| session.user_id.clone())
                    .unwrap_or_else(|| approval.requester_user_id.clone()),
                session_id: task_run.session_id.clone(),
                route_key: source_route_key_from_context(task_run, session),
            },
            intent: TaskIntent {
                domain: task_run.domain.clone(),
                action: task_run.action.clone(),
                raw_text: task_run.intent_text.clone(),
            },
            entity_refs: task_run.entity_refs.clone(),
            args: task_run.args.clone(),
            autonomy: TaskAutonomy {
                level: normalize_task_autonomy_level(&task_run.autonomy_level),
            },
            message: None,
        };
        let step_id = approval_event_step_id(&approval.approval_id);
        let event = build_task_event_record(
            &request,
            &step_id,
            event_type,
            severity,
            json!({
                "approval_ticket": approval,
                "approver_user_id": approver_user_id,
                "policy_ref": approval.policy_ref.clone(),
            }),
        );
        self.conversation_store
            .replace_events_for_step(&task_run.task_id, Some(&step_id), &[event])
    }
}

fn pending_candidates_from_results(results: &[HubScanResultItem]) -> Vec<PendingTaskCandidate> {
    results
        .iter()
        .filter(|item| !item.reachable)
        .map(|item| PendingTaskCandidate {
            candidate_id: item.candidate_id.clone(),
            name: item.name.clone(),
            ip: item.ip.clone(),
            room: (!item.room.trim().is_empty()).then(|| item.room.clone()),
            port: item.port,
            rtsp_paths: item.rtsp_paths.clone(),
            requires_auth: item.requires_auth,
            vendor: item.vendor.clone(),
            model: item.model.clone(),
        })
        .collect()
}

fn candidate_to_connect_request(
    candidate: &PendingTaskCandidate,
    password: Option<String>,
) -> CameraConnectRequest {
    CameraConnectRequest {
        name: candidate.name.clone(),
        room: candidate.room.clone(),
        ip: candidate.ip.clone(),
        path_candidates: candidate.rtsp_paths.clone(),
        username: None,
        password,
        port: Some(candidate.port),
        snapshot_url: None,
        discovery_source: "task_api_candidate_confirm".to_string(),
        vendor: candidate.vendor.clone(),
        model: candidate.model.clone(),
    }
}

fn pending_connect_to_request(
    pending: &PendingTaskConnect,
    password: Option<String>,
) -> CameraConnectRequest {
    CameraConnectRequest {
        name: pending.name.clone(),
        room: pending.room.clone(),
        ip: pending.ip.clone(),
        path_candidates: pending.rtsp_paths.clone(),
        username: None,
        password,
        port: Some(pending.port),
        snapshot_url: pending.snapshot_url.clone(),
        discovery_source: "task_api_password_retry".to_string(),
        vendor: pending.vendor.clone(),
        model: pending.model.clone(),
    }
}

fn format_scan_message(
    cidr: &str,
    results: &[HubScanResultItem],
    pending_candidates: &[PendingTaskCandidate],
    device_count: usize,
) -> String {
    let connected = results.iter().filter(|item| item.reachable).count();
    if results.is_empty() {
        return format!(
            "已按后台默认策略扫描 {}，但当前没有发现可确认的摄像头候选设备。你也可以直接发送：添加摄像头 192.168.x.x",
            cidr
        );
    }
    if pending_candidates.is_empty() {
        if connected == 0 {
            return format!(
                "已按后台默认策略扫描 {}，共发现 {} 个候选设备，但都还不能直接接入。你也可以直接发送：添加摄像头 192.168.x.x",
                cidr,
                results.len()
            );
        }
        return format!(
            "已按后台默认策略扫描 {}，成功接入 {} 台摄像头，设备库现在共有 {} 台。接下来可以直接说：分析客厅摄像头",
            cidr,
            connected,
            device_count
        );
    }
    format!(
        "已按后台默认策略扫描 {}，共发现 {} 台候选设备，已自动接入 {} 台，还剩 {} 台待你确认：\n{}\n请直接回复：接入 1。如果提示需要密码，再回复：密码 xxxxxx。",
        cidr,
        results.len(),
        connected,
        pending_candidates.len(),
        format_pending_candidates(pending_candidates)
    )
}

fn format_pending_candidates(candidates: &[PendingTaskCandidate]) -> String {
    candidates
        .iter()
        .enumerate()
        .map(|(index, candidate)| {
            format!(
                "{}. {}（{}，{}）",
                index + 1,
                candidate.name,
                candidate.ip,
                if candidate.requires_auth {
                    "需要密码"
                } else {
                    "待确认"
                }
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn build_vision_artifacts(payload: &Value) -> Vec<TaskArtifact> {
    let mut artifacts = Vec::new();
    let snapshot_mime_type = string_at_paths(payload, &["/snapshot/mime_type"])
        .unwrap_or_else(|| "image/jpeg".to_string());
    let snapshot_media_asset_id =
        string_at_paths(payload, &["/snapshot/media_asset_id", "/snapshot/asset_id"]);
    let annotated_media_asset_id =
        string_at_paths(payload, &["/snapshot/annotated_media_asset_id"]);
    if let Some(path) = string_at_paths(payload, &["/snapshot/image_path"]) {
        artifacts.push(TaskArtifact {
            kind: "image".to_string(),
            label: "抓拍图片".to_string(),
            mime_type: snapshot_mime_type.clone(),
            media_asset_id: snapshot_media_asset_id.clone(),
            path: Some(path),
            url: None,
            metadata: json!({
                "media_asset_id": snapshot_media_asset_id,
                "artifact_role": "analysis_snapshot",
            }),
        });
    }
    if let Some(path) = string_at_paths(payload, &["/snapshot/annotated_image_path"]) {
        artifacts.push(TaskArtifact {
            kind: "image".to_string(),
            label: "标注图片".to_string(),
            mime_type: snapshot_mime_type,
            media_asset_id: annotated_media_asset_id.clone(),
            path: Some(path),
            url: None,
            metadata: json!({
                "media_asset_id": annotated_media_asset_id,
                "artifact_role": "analysis_annotation",
            }),
        });
    }
    artifacts
}

fn build_snapshot_payload(
    target: &ResolvedCameraTarget,
    snapshot: &SnapshotCaptureResult,
    media_asset: &MediaAsset,
) -> Value {
    json!({
        "camera_target": target,
        "snapshot": {
            "media_asset_id": media_asset.asset_id.clone(),
            "mime_type": snapshot.mime_type.clone(),
            "byte_size": snapshot.byte_size,
            "captured_at_epoch_ms": snapshot.captured_at_epoch_ms,
            "storage": snapshot.storage.clone(),
        }
    })
}

fn build_snapshot_artifact(
    snapshot: &SnapshotCaptureResult,
    media_asset: &MediaAsset,
) -> TaskArtifact {
    TaskArtifact {
        kind: "image".to_string(),
        label: "抓拍图片".to_string(),
        mime_type: snapshot.mime_type.clone(),
        media_asset_id: Some(media_asset.asset_id.clone()),
        path: Some(snapshot.storage.relative_path.clone()),
        url: None,
        metadata: json!({
            "media_asset_id": media_asset.asset_id.clone(),
            "storage_target": snapshot.storage.target,
            "captured_at_epoch_ms": snapshot.captured_at_epoch_ms,
            "byte_size": snapshot.byte_size,
        }),
    }
}

fn build_snapshot_media_asset(
    request: &TaskRequest,
    target: &ResolvedCameraTarget,
    snapshot: &SnapshotCaptureResult,
) -> MediaAsset {
    MediaAsset {
        asset_id: new_media_asset_id(),
        workspace_id: workspace_id_for_request(request),
        device_id: Some(target.device_id.clone()),
        asset_kind: MediaAssetKind::Snapshot,
        storage_target: storage_target_kind_from_snapshot(snapshot.storage.target),
        storage_uri: snapshot.storage.relative_path.clone(),
        mime_type: snapshot.mime_type.clone(),
        byte_size: snapshot.byte_size as u64,
        checksum: snapshot_checksum(snapshot),
        captured_at: Some(snapshot.captured_at_epoch_ms.to_string()),
        started_at: None,
        ended_at: None,
        derived_from_asset_id: None,
        tags: vec!["snapshot".to_string(), "camera".to_string()],
        metadata: json!({
            "task_id": request.task_id.clone(),
            "step_id": step_id_for_request(request),
            "trace_id": request.trace_id.clone(),
            "source_channel": request.source.channel.clone(),
            "source_surface": request.source.surface.clone(),
            "camera_display_name": target.display_name.clone(),
            "room_name": target.room_name.clone(),
            "storage_relative_path": snapshot.storage.relative_path.clone(),
            "device_ingest_metadata": snapshot.ingest_metadata.clone(),
        }),
    }
}

fn build_clip_media_asset(
    request: &TaskRequest,
    target: &ResolvedCameraTarget,
    clip: &ClipCaptureResult,
) -> MediaAsset {
    MediaAsset {
        asset_id: new_media_asset_id(),
        workspace_id: workspace_id_for_request(request),
        device_id: Some(target.device_id.clone()),
        asset_kind: MediaAssetKind::Clip,
        storage_target: storage_target_kind_from_snapshot(clip.storage.target),
        storage_uri: clip.storage.relative_path.clone(),
        mime_type: clip.mime_type.clone(),
        byte_size: clip.byte_size as u64,
        checksum: file_checksum(&clip.storage.relative_path),
        captured_at: Some(clip.captured_at_epoch_ms.to_string()),
        started_at: Some(clip.started_at_epoch_ms.to_string()),
        ended_at: Some(clip.ended_at_epoch_ms.to_string()),
        derived_from_asset_id: None,
        tags: vec!["clip".to_string(), "camera".to_string()],
        metadata: json!({
            "task_id": request.task_id.clone(),
            "step_id": step_id_for_request(request),
            "trace_id": request.trace_id.clone(),
            "source_channel": request.source.channel.clone(),
            "source_surface": request.source.surface.clone(),
            "camera_display_name": target.display_name.clone(),
            "room_name": target.room_name.clone(),
            "storage_relative_path": clip.storage.relative_path.clone(),
            "clip_length_seconds": clip.clip_length_seconds,
            "keyframe_count": clip.keyframe_count,
            "keyframe_interval_seconds": clip.keyframe_interval_seconds,
            "device_ingest_metadata": clip.ingest_metadata.clone(),
        }),
    }
}

fn build_clip_confirmation_payload(
    target: &ResolvedCameraTarget,
    clip: &ClipCaptureResult,
    media_asset: &MediaAsset,
) -> Value {
    json!({
        "camera_target": target,
        "clip": {
            "media_asset_id": media_asset.asset_id.clone(),
            "mime_type": clip.mime_type.clone(),
            "byte_size": clip.byte_size,
            "captured_at_epoch_ms": clip.captured_at_epoch_ms,
            "started_at_epoch_ms": clip.started_at_epoch_ms,
            "ended_at_epoch_ms": clip.ended_at_epoch_ms,
            "clip_length_seconds": clip.clip_length_seconds,
            "keyframe_count": clip.keyframe_count,
        },
        "clip_confirmation": {
            "kind": "clip_confirmation",
            "clip_media_asset_id": media_asset.asset_id.clone(),
            "cover_artifact_role": "video_cover_frame",
            "delivery_target": "weixin",
            "fallback_delivery": "file",
        }
    })
}

fn build_clip_confirmation_cover_artifact(
    clip: &ClipCaptureResult,
    keyframes: &[PathBuf],
    media_asset: &MediaAsset,
) -> Option<TaskArtifact> {
    let cover = keyframes.first()?;
    Some(TaskArtifact {
        kind: "image".to_string(),
        label: "视频首帧".to_string(),
        mime_type: "image/jpeg".to_string(),
        media_asset_id: None,
        path: Some(cover.to_string_lossy().to_string()),
        url: None,
        metadata: json!({
            "artifact_role": "video_cover_frame",
            "clip_media_asset_id": media_asset.asset_id.clone(),
            "source_video_path": clip.storage.relative_path.clone(),
            "captured_at_epoch_ms": clip.captured_at_epoch_ms,
        }),
    })
}

fn recent_clip_playback_from_capture(
    clip: &ClipCaptureResult,
    media_asset: &MediaAsset,
    cover_artifact: &TaskArtifact,
    display_name: &str,
) -> RecentClipPlaybackState {
    RecentClipPlaybackState {
        clip_media_asset_id: media_asset.asset_id.clone(),
        clip_path: clip.storage.relative_path.clone(),
        clip_mime_type: clip.mime_type.clone(),
        cover_path: cover_artifact.path.clone().unwrap_or_default(),
        display_name: display_name.to_string(),
        captured_at_epoch_ms: clip.captured_at_epoch_ms,
    }
}

fn recent_clip_playback_from_pending(
    pending: &PendingTaskClipConfirmation,
    captured_at_epoch_ms: u128,
) -> RecentClipPlaybackState {
    RecentClipPlaybackState {
        clip_media_asset_id: pending.clip_media_asset_id.clone(),
        clip_path: pending.clip_path.clone(),
        clip_mime_type: pending.clip_mime_type.clone(),
        cover_path: pending.cover_path.clone(),
        display_name: pending.display_name.clone(),
        captured_at_epoch_ms,
    }
}

fn build_clip_delivery_payload(recent_clip: &RecentClipPlaybackState) -> Value {
    json!({
        "clip": {
            "media_asset_id": recent_clip.clip_media_asset_id.clone(),
            "mime_type": recent_clip.clip_mime_type.clone(),
            "path": recent_clip.clip_path.clone(),
        },
        "clip_delivery": {
            "kind": "clip_delivery",
            "clip_media_asset_id": recent_clip.clip_media_asset_id.clone(),
            "preferred_transport": "native_video",
            "fallback_transport": "file",
            "caption": "完整回放如下",
        }
    })
}

fn build_clip_delivery_artifact(recent_clip: &RecentClipPlaybackState) -> TaskArtifact {
    TaskArtifact {
        kind: "video".to_string(),
        label: format!("{} 完整回放", recent_clip.display_name),
        mime_type: recent_clip.clip_mime_type.clone(),
        media_asset_id: Some(recent_clip.clip_media_asset_id.clone()),
        path: Some(recent_clip.clip_path.clone()),
        url: None,
        metadata: json!({
            "artifact_role": "video_full_clip",
            "clip_media_asset_id": recent_clip.clip_media_asset_id.clone(),
            "delivery_target": "weixin",
            "fallback_delivery": "file",
        }),
    }
}

fn active_frame_token_matches(provided: Option<&str>, expected: &str) -> bool {
    let expected = expected.trim();
    if expected.is_empty() {
        return false;
    }
    match provided.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => value == expected,
        None => true,
    }
}

fn active_frame_cancel_requested(raw_text: &str) -> bool {
    let normalized = normalize_command_text(raw_text);
    matches_any(
        &normalized,
        &["算了", "不用了", "先不用", "不要了", "别处理", "取消"],
    ) || normalized == normalize_command_text("不要")
}

fn clip_confirmation_prompt(display_name: &str) -> String {
    let display_name = display_name.trim();
    if display_name.is_empty() {
        "已录制短视频片段。是否看完整回放？回复：要 / 不要".to_string()
    } else {
        format!("已录制 {display_name} 的短视频片段。是否看完整回放？回复：要 / 不要")
    }
}

fn clip_confirmation_reanchor_prompt(display_name: &str) -> String {
    let display_name = display_name.trim();
    if display_name.is_empty() {
        "刚才那段短视频已经录好，要发完整回放吗？回复：要 / 不要".to_string()
    } else {
        format!("刚才 {display_name} 那段短视频已经录好，要发完整回放吗？回复：要 / 不要")
    }
}

fn clip_confirmation_preserve_summary(
    request: &TaskRequest,
    pending: &PendingTaskClipConfirmation,
    plan: Option<&GeneralMessagePlan>,
) -> String {
    let reanchor = clip_confirmation_reanchor_prompt(&pending.display_name);
    let Some(plan) = plan else {
        return reanchor;
    };
    let intro = match plan.kind {
        GeneralMessagePlanKind::CapabilitySummary => {
            "我可以抓拍最新画面、录短视频，也能搜索已经保存的内容。".to_string()
        }
        GeneralMessagePlanKind::Clarify => "我明白你可能想切到别的事；先把这段确认完。".to_string(),
        GeneralMessagePlanKind::ConversationAct => {
            let act = plan.conversation_act.unwrap_or_else(|| {
                infer_general_message_conversation_act(request.intent.raw_text.as_str(), None)
            });
            clip_confirmation_preserve_conversation_intro(request, act)
        }
        GeneralMessagePlanKind::Unsupported => "这句话我没有当作新的工具动作。".to_string(),
        _ => String::new(),
    };
    join_short_reply(&intro, &reanchor)
}

fn clip_confirmation_preserve_conversation_intro(
    request: &TaskRequest,
    act: GeneralMessageConversationAct,
) -> String {
    match act {
        GeneralMessageConversationAct::Continue => {
            active_frame_continue_ack(request.intent.raw_text.as_str())
        }
        GeneralMessageConversationAct::Boundary => {
            let normalized = normalize_command_text(request.intent.raw_text.as_str());
            if matches_any(&normalized, &["天气", "温度", "下雨"]) {
                "天气这类实时信息我现在不处理；先把当前回放确认完。".to_string()
            } else {
                "这类事我现在不直接处理；先把当前回放确认完。".to_string()
            }
        }
        GeneralMessageConversationAct::Repair => {
            "明白，我先不把这句话当作新的工具动作。".to_string()
        }
        GeneralMessageConversationAct::Cancel => String::new(),
        GeneralMessageConversationAct::ClarifyContinue => {
            "收到，我们先把当前回放确认完。".to_string()
        }
    }
}

fn active_frame_continue_ack(raw_text: &str) -> String {
    let normalized = normalize_command_text(raw_text);
    if matches_any(
        &normalized,
        &[
            "谢谢",
            "辛苦",
            "真棒",
            "很好",
            "非常好",
            "不错",
            "厉害",
            "太好了",
            "干得好",
            "靠谱",
        ],
    ) {
        return "谢谢认可。".to_string();
    }
    if matches_any(
        &normalized,
        &["收到", "明白", "好的", "好", "嗯", "可以", "ok", "OK"],
    ) {
        return "好，我们继续当前这件事。".to_string();
    }
    "收到，我们继续当前这件事。".to_string()
}

fn join_short_reply(intro: &str, reanchor: &str) -> String {
    let intro = intro.trim();
    let reanchor = reanchor.trim();
    if intro.is_empty() {
        return reanchor.to_string();
    }
    if reanchor.is_empty() {
        return intro.to_string();
    }
    format!("{intro}{reanchor}")
}

fn clip_confirmation_active_frame_decision(plan: &GeneralMessagePlan) -> ActiveFrameDecision {
    match plan.kind {
        GeneralMessagePlanKind::CameraReplayRecentClip => ActiveFrameDecision::Deliver,
        GeneralMessagePlanKind::CameraSnapshot
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
        | GeneralMessagePlanKind::FamilyMemoryShowFavorites => ActiveFrameDecision::Supersede,
        GeneralMessagePlanKind::ConversationAct
            if plan.conversation_act == Some(GeneralMessageConversationAct::Cancel) =>
        {
            ActiveFrameDecision::Cancel
        }
        _ => ActiveFrameDecision::Preserve,
    }
}

fn clip_confirmation_reply_decision(raw_text: &str) -> ClipConfirmationReplyDecision {
    let normalized = normalize_command_text(raw_text);
    if normalized.is_empty() {
        return ClipConfirmationReplyDecision::Unknown;
    }
    if clip_confirmation_reply_is_negative(&normalized) {
        return ClipConfirmationReplyDecision::Decline;
    }
    if clip_confirmation_reply_is_affirmative(&normalized)
        || recent_clip_playback_request_from_normalized(&normalized)
    {
        return ClipConfirmationReplyDecision::Deliver;
    }
    ClipConfirmationReplyDecision::Unknown
}

fn clip_confirmation_reply_is_affirmative(normalized: &str) -> bool {
    matches!(
        normalized,
        "要" | "要看"
            | "看"
            | "发我"
            | "发出来"
            | "给我看"
            | "给我看完整回放"
            | "好的发我"
            | "可以发我"
    )
}

fn clip_confirmation_reply_is_negative(normalized: &str) -> bool {
    matches!(
        normalized,
        "不要"
            | "不用"
            | "不用了"
            | "不看"
            | "不看了"
            | "先不要"
            | "先不用"
            | "先不看"
            | "先不发"
            | "不用发"
            | "不要发"
            | "算了"
            | "取消"
            | "别处理"
    )
}

fn recent_clip_playback_request_from_normalized(normalized: &str) -> bool {
    if normalized.is_empty() {
        return false;
    }

    if matches_any(
        normalized,
        &[
            "完整回放",
            "回放",
            "回放一下",
            "回放一段",
            "回看",
            "播放",
            "播放一下",
            "播一下",
            "播出来",
            "放一下",
            "放出来",
            "发一下视频",
            "把视频发我",
        ],
    ) {
        return true;
    }

    let has_playback_verb = ["回放", "回看", "播放", "播", "放"]
        .iter()
        .map(|token| normalize_command_text(token))
        .any(|token| normalized.contains(&token));
    let has_watch_or_delivery_target = ["一下", "给我", "出来", "完整", "视频", "片段", "回放"]
        .iter()
        .map(|token| normalize_command_text(token))
        .any(|token| normalized.contains(&token));
    if has_playback_verb && has_watch_or_delivery_target {
        return true;
    }

    (normalized.contains('看') || normalized.contains('发'))
        && ["完整", "回放", "视频", "片段"]
            .iter()
            .map(|token| normalize_command_text(token))
            .any(|token| normalized.contains(&token))
}

fn recent_clip_playback_is_fresh(recent_clip: &RecentClipPlaybackState) -> bool {
    if recent_clip.captured_at_epoch_ms == 0 {
        return true;
    }
    current_epoch_ms().saturating_sub(recent_clip.captured_at_epoch_ms)
        <= RECENT_CLIP_PLAYBACK_WINDOW_MS
}

fn resolved_recording_policy(
    state: &AdminConsoleState,
    target: Option<&ResolvedCameraTarget>,
) -> Option<RecordingPolicy> {
    state
        .platform
        .recording_policies
        .iter()
        .find(|policy| {
            target
                .and_then(|target| {
                    policy
                        .device_id
                        .as_deref()
                        .map(|device_id| device_id == target.device_id.as_str())
                })
                .unwrap_or(false)
        })
        .cloned()
        .or_else(|| state.platform.recording_policies.first().cloned())
}

fn resolved_capture_directory(
    state: &AdminConsoleState,
    recording_policy: Option<&RecordingPolicy>,
) -> Result<PathBuf, String> {
    let root = PathBuf::from(harboros_writable_root());
    ensure_safe_capture_root(&root)?;
    let subdirectory = recording_policy
        .and_then(RecordingPolicy::capture_subdirectory)
        .unwrap_or(state.defaults.capture_subdirectory.as_str());
    let subdirectory = sanitize_relative_subdirectory(subdirectory)
        .ok_or_else(|| "capture 子目录不合法，必须是 writable root 下的相对路径。".to_string())?;
    let capture_root = root.join(subdirectory);
    fs::create_dir_all(&capture_root).map_err(|error| {
        format!(
            "failed to create capture directory {}: {error}",
            capture_root.display()
        )
    })?;
    Ok(capture_root)
}

fn ensure_safe_capture_root(root: &Path) -> Result<(), String> {
    let normalized = root.to_string_lossy().replace('\\', "/");
    if normalized.starts_with("/mnt/software/harborbeacon-agent-ci") {
        Ok(())
    } else if std::env::var(ALLOW_NON_HARBOROS_CAPTURE_ROOT_ENV)
        .ok()
        .is_some_and(|value| env_flag_enabled(&value))
        && root.is_absolute()
        && normalized.ends_with("/harborbeacon-agent-ci")
    {
        Ok(())
    } else {
        Err(format!(
            "capture writable root {} is outside the approved HarborOS root",
            root.display()
        ))
    }
}

fn sanitize_relative_subdirectory(value: &str) -> Option<PathBuf> {
    let trimmed = value.trim().trim_matches('/');
    if trimmed.is_empty() {
        return None;
    }

    let candidate = Path::new(trimmed);
    if candidate.is_absolute() {
        return None;
    }
    let mut sanitized = PathBuf::new();
    for component in candidate.components() {
        match component {
            std::path::Component::Normal(segment) => sanitized.push(segment),
            _ => return None,
        }
    }
    (!sanitized.as_os_str().is_empty()).then_some(sanitized)
}

fn build_snapshot_output_path(
    capture_root: &Path,
    target: &ResolvedCameraTarget,
    captured_at_epoch_ms: u128,
    extension: &str,
) -> PathBuf {
    capture_root.join(format!(
        "{}-{}.{}",
        sanitize_path_segment(&target.device_id),
        captured_at_epoch_ms,
        extension
    ))
}

fn build_clip_output_path(
    capture_root: &Path,
    target: &ResolvedCameraTarget,
    captured_at_epoch_ms: u128,
) -> PathBuf {
    capture_root.join(format!(
        "{}-{}.mp4",
        sanitize_path_segment(&target.device_id),
        captured_at_epoch_ms
    ))
}

fn build_keyframe_directory(capture_root: &Path, clip_path: &Path) -> PathBuf {
    let stem = clip_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("clip");
    capture_root.join("keyframes").join(stem)
}

fn current_epoch_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn sanitize_path_segment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn write_media_index_sidecar(
    sidecar_path: &Path,
    media_path: &str,
    source_video_path: Option<&str>,
    target: &ResolvedCameraTarget,
    ocr_text: &str,
    vlm_summary: &str,
    tags: &[String],
) -> Result<(), String> {
    if let Some(parent) = sidecar_path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create sidecar directory {}: {error}",
                parent.display()
            )
        })?;
    }

    let searchable = [ocr_text.trim(), vlm_summary.trim()]
        .iter()
        .filter(|value| !value.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("\n");
    let content = serde_json::to_string_pretty(&json!({
        "caption": vlm_summary.trim(),
        "derived_text": searchable,
        "ocr_text": ocr_text.trim(),
        "source_video_path": source_video_path,
        "camera": {
            "device_id": target.device_id,
            "display_name": target.display_name,
            "room_name": target.room_name,
            "vendor": target.vendor,
            "model": target.model,
        },
        "source_path": media_path,
        "labels": tags,
    }))
    .map_err(|error| format!("failed to serialize media sidecar: {error}"))?;
    fs::write(sidecar_path, content).map_err(|error| {
        format!(
            "failed to write media sidecar {}: {error}",
            sidecar_path.display()
        )
    })
}

fn build_vision_image_media_asset(
    request: &TaskRequest,
    target: &ResolvedCameraTarget,
    image_path: &str,
    mime_type: &str,
    asset_kind: MediaAssetKind,
    derived_from_asset_id: Option<String>,
    artifact_role: &str,
    captured_at: &str,
    byte_size_override: Option<u64>,
    source_storage: Value,
    ingest_metadata: Value,
    detection_summary: Option<&str>,
    summary: Option<&str>,
    summary_source: Option<&str>,
) -> MediaAsset {
    let tags = if asset_kind == MediaAssetKind::Derived {
        vec![
            "derived".to_string(),
            "annotated".to_string(),
            "camera".to_string(),
            "vision_analysis".to_string(),
        ]
    } else {
        vec![
            "snapshot".to_string(),
            "camera".to_string(),
            "vision_analysis".to_string(),
        ]
    };

    MediaAsset {
        asset_id: new_media_asset_id(),
        workspace_id: workspace_id_for_request(request),
        device_id: Some(target.device_id.clone()),
        asset_kind,
        storage_target: StorageTargetKind::LocalDisk,
        storage_uri: image_path.to_string(),
        mime_type: mime_type.to_string(),
        byte_size: byte_size_override.unwrap_or_else(|| file_byte_size(image_path)),
        checksum: file_checksum(image_path),
        captured_at: Some(captured_at.to_string()),
        started_at: None,
        ended_at: None,
        derived_from_asset_id,
        tags,
        metadata: json!({
            "task_id": request.task_id.clone(),
            "step_id": step_id_for_request(request),
            "trace_id": request.trace_id.clone(),
            "source_channel": request.source.channel.clone(),
            "source_surface": request.source.surface.clone(),
            "camera_display_name": target.display_name.clone(),
            "room_name": target.room_name.clone(),
            "artifact_role": artifact_role,
            "detection_summary": detection_summary,
            "summary": summary,
            "summary_source": summary_source,
            "storage_path": image_path,
            "source_storage": source_storage,
            "ingest_metadata": ingest_metadata,
        }),
    }
}

fn storage_target_kind_from_snapshot(target: StorageTarget) -> StorageTargetKind {
    match target {
        StorageTarget::LocalDisk => StorageTargetKind::LocalDisk,
        StorageTarget::HarborOsPool => StorageTargetKind::HarborOsPool,
        StorageTarget::ExternalShare => StorageTargetKind::Nas,
    }
}

fn snapshot_checksum(snapshot: &SnapshotCaptureResult) -> Option<String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(snapshot.bytes_base64.as_bytes())
        .ok()?;
    let digest = Sha256::digest(&bytes);
    Some(format!(
        "sha256:{}",
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

fn file_byte_size(path: &str) -> u64 {
    fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

fn file_checksum(path: &str) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let digest = Sha256::digest(&bytes);
    Some(format!(
        "sha256:{}",
        digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    ))
}

fn mime_type_from_path(path: &str) -> Option<String> {
    let extension = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "jpg" | "jpeg" => Some("image/jpeg".to_string()),
        "png" => Some("image/png".to_string()),
        "webp" => Some("image/webp".to_string()),
        "mp4" | "m4v" => Some("video/mp4".to_string()),
        "mov" => Some("video/quicktime".to_string()),
        "mkv" => Some("video/x-matroska".to_string()),
        "webm" => Some("video/webm".to_string()),
        "avi" => Some("video/x-msvideo".to_string()),
        _ => None,
    }
}

fn build_share_link_payload(
    target: &ResolvedCameraTarget,
    issued: &remote_view::IssuedCameraShareToken,
    media_session: &MediaSession,
    share_link: &ShareLink,
) -> Value {
    let encoded_token = url_encode_path_segment(&issued.token);
    let relative_url = format!("/shared/cameras/{encoded_token}");
    let stream_url = format!("{relative_url}/live.mjpeg");
    json!({
        "share_link_id": share_link.share_link_id,
        "media_session_id": media_session.media_session_id,
        "device_id": target.device_id,
        "display_name": target.display_name,
        "url": relative_url,
        "stream_url": stream_url,
        "access_scope": share_link.access_scope,
        "expires_at_unix_secs": issued.expires_at_unix_secs,
        "ttl_minutes": issued.ttl_minutes,
    })
}

fn build_share_link_artifact(share_link: &Value) -> TaskArtifact {
    TaskArtifact {
        kind: "link".to_string(),
        label: "共享观看链接".to_string(),
        mime_type: "text/uri-list".to_string(),
        media_asset_id: None,
        path: None,
        url: share_link
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_string),
        metadata: json!({
            "share_link_id": share_link.get("share_link_id").cloned().unwrap_or(Value::Null),
            "media_session_id": share_link
                .get("media_session_id")
                .cloned()
                .unwrap_or(Value::Null),
            "access_scope": share_link.get("access_scope").cloned().unwrap_or(Value::Null),
            "stream_url": share_link.get("stream_url").cloned().unwrap_or(Value::Null),
            "expires_at_unix_secs": share_link
                .get("expires_at_unix_secs")
                .cloned()
                .unwrap_or(Value::Null),
            "ttl_minutes": share_link.get("ttl_minutes").cloned().unwrap_or(Value::Null),
        }),
    }
}

fn build_knowledge_search_artifacts(response: &KnowledgeSearchResponse) -> Vec<TaskArtifact> {
    response
        .documents
        .iter()
        .chain(response.images.iter())
        .chain(response.videos.iter())
        .take(6)
        .map(|hit| {
            let proxied_video_path = resolved_video_proxy_path(hit);
            let is_video_proxy = proxied_video_path.is_some();
            let path = proxied_video_path.unwrap_or_else(|| hit.path.clone());
            TaskArtifact {
                kind: if is_video_proxy {
                    "video".to_string()
                } else if hit.modality.as_str() == "video" {
                    "video".to_string()
                } else if hit.modality.as_str() == "image" {
                    "image".to_string()
                } else {
                    "text".to_string()
                },
                label: hit.title.clone(),
                mime_type: mime_type_from_path(&path).unwrap_or_else(|| {
                    if is_video_proxy {
                        "video/mp4".to_string()
                    } else if hit.modality.as_str() == "video" {
                        "video/*".to_string()
                    } else if hit.modality.as_str() == "image" {
                        "image/*".to_string()
                    } else {
                        "text/plain".to_string()
                    }
                }),
                media_asset_id: None,
                path: Some(path),
                url: None,
                metadata: json!({
                    "modality": if is_video_proxy { "video" } else { hit.modality.as_str() },
                    "display_name": hit.title.clone(),
                    "content_summary": hit.snippet.clone(),
                    "match_source": knowledge_hit_match_source(hit),
                    "segment_locator": video_segment_locator(hit),
                    "video_level_result": hit.modality.as_str() == "video" && video_segment_locator(hit).is_null(),
                    "score": hit.score,
                    "content_source_kinds": hit.content_source_kinds.clone(),
                    "content_indexed": hit.content_indexed,
                    "filename_match_used": hit.filename_match_used,
                    "content_match_used": hit.content_match_used,
                    "source_image_path": if is_video_proxy { Some(hit.path.clone()) } else { None::<String> },
                    "citation": {
                        "title": hit.title.clone(),
                        "path": hit.path.clone(),
                        "modality": hit.modality.clone(),
                        "chunk_id": hit.chunk_id.clone(),
                        "line_start": hit.line_start,
                        "line_end": hit.line_end,
                        "matched_terms": hit.matched_terms.clone(),
                        "preview": hit.snippet.clone(),
                        "score": hit.score,
                        "lexical_score": hit.lexical_score,
                        "embedding_score": hit.embedding_score,
                        "hybrid_score": hit.hybrid_score,
                        "rerank_score": hit.rerank_score,
                        "provenance": hit.provenance.clone(),
                        "match_source": knowledge_hit_match_source(hit),
                        "segment_locator": video_segment_locator(hit),
                        "source_path": hit.source_path.clone(),
                        "content_source_kinds": hit.content_source_kinds.clone(),
                        "content_indexed": hit.content_indexed,
                        "filename_match_used": hit.filename_match_used,
                        "content_match_used": hit.content_match_used,
                    },
                }),
            }
        })
        .collect()
}

fn resolved_video_proxy_path(
    hit: &crate::runtime::knowledge::KnowledgeSearchHit,
) -> Option<String> {
    if hit.modality.as_str() != "image" {
        return None;
    }
    let sidecar_path = hit
        .source_path
        .as_deref()
        .and_then(|path| {
            Path::new(path)
                .extension()
                .and_then(|extension| extension.to_str())
                .filter(|extension| {
                    matches!(
                        extension.to_ascii_lowercase().as_str(),
                        "json" | "yaml" | "yml" | "txt" | "md" | "markdown" | "csv"
                    )
                })
                .map(|_| PathBuf::from(path))
        })
        .or_else(|| {
            let candidate = Path::new(&hit.path).with_extension("json");
            candidate.exists().then_some(candidate)
        })?;
    let value = fs::read_to_string(sidecar_path).ok()?;
    let json = serde_json::from_str::<Value>(&value).ok()?;
    json.get("source_video_path")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
}

fn knowledge_hit_match_source(hit: &crate::runtime::knowledge::KnowledgeSearchHit) -> String {
    if hit.content_match_used {
        return hit
            .provenance
            .clone()
            .or_else(|| hit.content_source_kinds.first().cloned())
            .unwrap_or_else(|| "content".to_string());
    }
    if hit.filename_match_used {
        return "filename".to_string();
    }
    if hit.embedding_score.unwrap_or_default() > 0.0 {
        return "semantic_embedding".to_string();
    }
    "unknown".to_string()
}

fn video_segment_locator(hit: &crate::runtime::knowledge::KnowledgeSearchHit) -> Value {
    if hit.modality.as_str() != "video" {
        return Value::Null;
    }
    if let Some(locator) = hit
        .snippet
        .as_deref()
        .and_then(video_keyframe_percent_locator)
    {
        return locator;
    }
    if hit.provenance.as_deref() == Some("video_sidecar") {
        if let Some(locator) = hit
            .source_path
            .as_deref()
            .and_then(video_sidecar_segment_locator)
        {
            return locator;
        }
    }
    if let Some(locator) = hit.snippet.as_deref().and_then(video_timecode_locator) {
        return locator;
    }
    Value::Null
}

fn video_keyframe_percent_locator(text: &str) -> Option<Value> {
    let lower = text.to_lowercase();
    let percent_index = lower.find('%')?;
    if !lower[..percent_index].contains("keyframe") {
        return None;
    }
    let digits = lower[..percent_index]
        .chars()
        .rev()
        .skip_while(|ch| ch.is_whitespace())
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    let percent = digits.parse::<u32>().ok().filter(|value| *value <= 100)?;
    Some(json!({
        "kind": "keyframe_percent",
        "percent": percent,
        "source": "vlm_keyframe",
    }))
}

fn video_sidecar_segment_locator(source_path: &str) -> Option<Value> {
    let path = Path::new(source_path);
    if path.extension()?.to_str()?.to_ascii_lowercase() != "json" {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;
    let value = serde_json::from_str::<Value>(&text).ok()?;
    let start = first_json_scalar_string(
        &value,
        &[
            "/start_time",
            "/start",
            "/start_at",
            "/start_seconds",
            "/start_ms",
            "/segment/start",
            "/segments/0/start",
            "/segments/0/start_time",
        ],
    );
    let end = first_json_scalar_string(
        &value,
        &[
            "/end_time",
            "/end",
            "/end_at",
            "/end_seconds",
            "/end_ms",
            "/segment/end",
            "/segments/0/end",
            "/segments/0/end_time",
        ],
    );
    let timestamp = first_json_scalar_string(
        &value,
        &[
            "/timestamp",
            "/timestamp_time",
            "/timestamp_seconds",
            "/timestamp_ms",
            "/timecode",
            "/segments/0/timestamp",
            "/segments/0/timecode",
        ],
    );
    if start.is_none() && end.is_none() && timestamp.is_none() {
        return None;
    }
    Some(json!({
        "kind": if start.is_some() || end.is_some() { "time_range" } else { "timestamp" },
        "start": start,
        "end": end,
        "timestamp": timestamp,
        "source": "video_sidecar",
    }))
}

fn video_timecode_locator(text: &str) -> Option<Value> {
    text.split_whitespace()
        .map(|token| {
            token.trim_matches(|ch: char| {
                matches!(
                    ch,
                    ',' | '.'
                        | ';'
                        | ':'
                        | '，'
                        | '。'
                        | '；'
                        | '：'
                        | '('
                        | ')'
                        | '['
                        | ']'
                        | '{'
                        | '}'
                )
            })
        })
        .find(|token| looks_like_timecode(token))
        .map(|timestamp| {
            json!({
                "kind": "timestamp",
                "timestamp": timestamp,
                "source": "snippet",
            })
        })
}

fn looks_like_timecode(value: &str) -> bool {
    let parts = value.split(':').collect::<Vec<_>>();
    matches!(parts.len(), 2 | 3)
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|ch| ch.is_ascii_digit()))
}

fn first_json_scalar_string(value: &Value, paths: &[&str]) -> Option<String> {
    paths.iter().find_map(|path| {
        let value = value.pointer(path)?;
        if let Some(text) = value.as_str() {
            let text = text.trim();
            return (!text.is_empty()).then(|| text.to_string());
        }
        if let Some(number) = value.as_i64() {
            return Some(number.to_string());
        }
        value.as_f64().map(|number| number.to_string())
    })
}

fn format_knowledge_search_message(response: &KnowledgeSearchResponse) -> String {
    response.reply_pack.summary.clone()
}

fn knowledge_search_next_actions(response: &KnowledgeSearchResponse) -> Vec<String> {
    let mut actions = Vec::new();
    if !response.documents.is_empty() {
        actions.push("只看文档结果".to_string());
    }
    if !response.images.is_empty() {
        actions.push("只看图片结果".to_string());
    }
    if !response.videos.is_empty() {
        actions.push("只看视频结果".to_string());
        actions.push("只看第一个视频里的结果".to_string());
        actions.push("找同一段前后".to_string());
    }
    if actions.is_empty() {
        match response.empty_reason.as_deref() {
            Some("no_video_files") => actions.push("检查知识源视频目录".to_string()),
            Some("video_not_indexed")
            | Some("video_sidecar_or_vlm_unavailable")
            | Some("video_content_unavailable") => actions.push("先刷新视频索引".to_string()),
            Some("video_content_no_match") => actions.push("换个视频关键词再搜".to_string()),
            _ => actions.push("换个关键词再搜".to_string()),
        }
    }
    actions
}

fn rag_answer_context_citations(
    response: &KnowledgeSearchResponse,
) -> Vec<KnowledgeSearchCitation> {
    response
        .reply_pack
        .citations
        .iter()
        .filter(|citation| citation.score > 0)
        .take(RAG_ANSWER_CONTEXT_LIMIT)
        .cloned()
        .collect()
}

fn build_rag_answer_data(
    query: &str,
    answer: &str,
    status: &str,
    degraded: bool,
    degraded_reason: &str,
    search: &KnowledgeSearchResponse,
    citations: &[KnowledgeSearchCitation],
    model: Value,
    warnings: Vec<String>,
    privacy_gateway: Value,
) -> Value {
    let degraded_reason = if degraded_reason == "none" {
        None
    } else {
        Some(degraded_reason.to_string())
    };
    json!({
        "kind": "rag.answer",
        "status": status,
        "degraded": degraded,
        "degraded_reason": degraded_reason,
        "query": query,
        "answer": answer,
        "answer_citation_policy": "cited_context_only",
        "citations": citations,
        "reply_pack": {
            "kind": "rag.answer",
            "summary": answer,
            "citations": citations,
        },
        "search": search,
        "model": model,
        "privacy_gateway": privacy_gateway,
        "warnings": warnings,
        "privacy_level": search.privacy_level,
        "resource_profile": search.resource_profile,
    })
}

fn build_limited_rag_answer(query: &str, citations: &[KnowledgeSearchCitation]) -> String {
    let mut lines = vec![format!(
        "基于当前检索到的引用，关于“{}”只能给出有限回答：",
        query
    )];
    let mut added = 0usize;
    for (index, citation) in citations.iter().take(3).enumerate() {
        let preview = cleaned_citation_preview(citation);
        if preview.is_empty() {
            continue;
        }
        lines.push(format!("{}. {} [{}]", index + 1, preview, index + 1));
        added += 1;
    }
    if added == 0 {
        lines.push("已找到匹配来源，但这些来源缺少可直接引用的文本片段；需要刷新索引或换一个更具体的问题。".to_string());
    }
    lines.join("\n")
}

fn cleaned_citation_preview(citation: &KnowledgeSearchCitation) -> String {
    let preview = citation
        .preview
        .as_deref()
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if preview.trim().is_empty() {
        return String::new();
    }
    truncate_chars(&preview, 220)
}

fn build_rag_answer_prompt(query: &str, citations: &[KnowledgeSearchCitation]) -> String {
    let mut lines = vec![
        "请只根据下面的引用回答用户问题。".to_string(),
        "如果引用不足以回答，请明确说证据不足。".to_string(),
        "每个实质性陈述必须带 [n] 形式的引用编号。".to_string(),
        String::new(),
        format!("问题：{query}"),
        String::new(),
        "引用：".to_string(),
    ];
    for (index, citation) in citations.iter().enumerate() {
        let preview = cleaned_citation_preview(citation);
        lines.push(format!(
            "[{}] title={} modality={} path={} chunk={} text={}",
            index + 1,
            citation.title,
            citation.modality,
            citation.path,
            citation.chunk_id.as_deref().unwrap_or(""),
            truncate_chars(&preview, 700)
        ));
    }
    lines.join("\n")
}

fn build_rag_answer_system_prompt() -> String {
    "You are HarborBeacon RAG answerer. Answer only from the provided citations. Use citation markers like [1]. If the evidence is weak, say so instead of adding uncited facts."
        .to_string()
}

fn normalize_rag_answer_text(text: &str) -> String {
    text.trim()
        .trim_matches(|ch: char| ch == '`' || ch.is_whitespace())
        .trim()
        .to_string()
}

fn rag_answer_has_citation_marker(answer: &str, citation_count: usize) -> bool {
    (1..=citation_count).any(|index| {
        let bracket = format!("[{index}]");
        let wide_bracket = format!("【{index}】");
        answer.contains(&bracket) || answer.contains(&wide_bracket)
    })
}

fn rag_answer_model_state_for_policy(
    model_state: &AdminModelCenterState,
    privacy_level: PrivacyLevel,
    resource_profile: RagResourceProfile,
) -> Result<AdminModelCenterState, String> {
    let allowed_kinds = rag_answer_allowed_endpoint_kinds(privacy_level, resource_profile)?;
    let has_llm = model_state.endpoints.iter().any(|endpoint| {
        endpoint.model_kind == ModelKind::Llm
            && endpoint.status != ModelEndpointStatus::Disabled
            && allowed_kinds.contains(&endpoint.endpoint_kind)
    });
    if !has_llm {
        return Err(format!(
            "resource_profile={} 下没有可用的 LLM endpoint；rag.answer 已降级为引用片段摘要。",
            resource_profile.as_str()
        ));
    }

    let allowed_labels = allowed_kinds
        .iter()
        .map(|kind| kind.as_str().to_string())
        .collect::<Vec<_>>();
    let mut filtered = model_state.clone();
    filtered
        .endpoints
        .retain(|endpoint| allowed_kinds.contains(&endpoint.endpoint_kind));
    for policy in &mut filtered.route_policies {
        policy
            .fallback_order
            .retain(|kind| allowed_labels.iter().any(|allowed| allowed == kind));
        if policy.fallback_order.is_empty() {
            policy.fallback_order = allowed_labels.clone();
        }
    }
    Ok(filtered)
}

fn rag_answer_allowed_endpoint_kinds(
    privacy_level: PrivacyLevel,
    resource_profile: RagResourceProfile,
) -> Result<Vec<ModelEndpointKind>, String> {
    match resource_profile {
        RagResourceProfile::CpuOnly | RagResourceProfile::LocalGpu => {
            Ok(vec![ModelEndpointKind::Local])
        }
        RagResourceProfile::SidecarGpu => Ok(vec![ModelEndpointKind::Sidecar]),
        RagResourceProfile::CloudAllowed => {
            if privacy_level == PrivacyLevel::StrictLocal {
                Err("resource_profile=cloud_allowed 与 strict_local 隐私策略冲突；rag.answer 不会调用云端模型。".to_string())
            } else {
                Ok(vec![
                    ModelEndpointKind::Local,
                    ModelEndpointKind::Sidecar,
                    ModelEndpointKind::Cloud,
                ])
            }
        }
    }
}

fn llm_execution_model_json(result: &LlmTextExecution) -> Value {
    json!({
        "available": result.available,
        "status": result.status,
        "summary": result.summary,
        "provider_key": result.provider_key,
        "model_endpoint_id": result.model_endpoint_id,
        "selected_endpoint": result.details.get("selected_endpoint").cloned().unwrap_or(Value::Null),
        "selected_endpoint_kind": result.details.get("selected_endpoint_kind").cloned().unwrap_or(Value::Null),
        "attempted_endpoints": result.details.get("attempted_endpoints").cloned().unwrap_or_else(|| json!([])),
        "fallback_reason": result.details.get("fallback_reason").cloned().unwrap_or(Value::Null),
        "fallback_used": result.details.get("fallback_used").cloned().unwrap_or(Value::Bool(false)),
    })
}

fn llm_selected_endpoint_kind(result: &LlmTextExecution) -> Option<&str> {
    result
        .details
        .get("selected_endpoint_kind")
        .and_then(Value::as_str)
}

fn cloud_only_llm_model_state_for_policy(
    model_state: &AdminModelCenterState,
    route_policy_id: &str,
) -> Option<AdminModelCenterState> {
    let policy_allows_cloud = model_state.route_policies.iter().any(|policy| {
        policy.route_policy_id == route_policy_id
            && policy.status.eq_ignore_ascii_case("active")
            && policy.privacy_level != PrivacyLevel::StrictLocal
            && policy
                .fallback_order
                .iter()
                .any(|kind| kind.eq_ignore_ascii_case("cloud"))
    });
    if !policy_allows_cloud {
        return None;
    }

    let mut cloud_state = model_state.clone();
    cloud_state.endpoints.retain(|endpoint| {
        endpoint.model_kind == ModelKind::Llm
            && endpoint.endpoint_kind == ModelEndpointKind::Cloud
            && endpoint.status != ModelEndpointStatus::Disabled
    });
    if cloud_state.endpoints.is_empty() {
        return None;
    }
    for policy in &mut cloud_state.route_policies {
        if policy.route_policy_id == route_policy_id {
            policy.fallback_order = vec!["cloud".to_string()];
        }
    }
    Some(cloud_state)
}

fn llm_model_state_without_cloud(model_state: &AdminModelCenterState) -> AdminModelCenterState {
    let mut local_state = model_state.clone();
    local_state
        .endpoints
        .retain(|endpoint| endpoint.endpoint_kind != ModelEndpointKind::Cloud);
    for policy in &mut local_state.route_policies {
        policy
            .fallback_order
            .retain(|kind| !kind.eq_ignore_ascii_case("cloud"));
        if policy.fallback_order.is_empty() {
            policy.fallback_order = vec!["local".to_string(), "sidecar".to_string()];
        }
    }
    local_state
}

fn rag_answer_next_actions(response: &KnowledgeSearchResponse, degraded: bool) -> Vec<String> {
    let mut actions = Vec::new();
    if !response.reply_pack.citations.is_empty() {
        actions.push("查看引用来源".to_string());
        actions.push("只检索不总结".to_string());
    }
    if !response.videos.is_empty() {
        actions.push("只看视频结果".to_string());
    }
    if degraded {
        actions.push("换个关键词再问".to_string());
    }
    if actions.is_empty() {
        actions.push("先刷新知识索引".to_string());
    }
    actions
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut truncated = text.chars().take(max_chars).collect::<String>();
    truncated.push('…');
    truncated
}

fn build_share_media_session(
    request: &TaskRequest,
    target: &ResolvedCameraTarget,
    media_session_id: &str,
    share_link_id: &str,
) -> MediaSession {
    MediaSession {
        media_session_id: media_session_id.to_string(),
        device_id: target.device_id.clone(),
        stream_profile_id: format!("{}::stream::primary", target.device_id),
        session_kind: MediaSessionKind::Share,
        delivery_mode: share_delivery_mode(target),
        opened_by_user_id: (!request.source.user_id.trim().is_empty())
            .then(|| request.source.user_id.clone()),
        status: MediaSessionStatus::Active,
        share_link_id: Some(share_link_id.to_string()),
        started_at: Some(current_timestamp()),
        ended_at: None,
        metadata: json!({
            "task_id": request.task_id.clone(),
            "step_id": step_id_for_request(request),
            "source_channel": request.source.channel.clone(),
            "source_surface": request.source.surface.clone(),
            "conversation_id": request.source.conversation_id.clone(),
            "delivery_proxy": "mjpeg",
            "stream_transport": serde_json::to_value(target.primary_stream.transport).unwrap_or(Value::Null),
        }),
    }
}

fn build_share_link_record(
    issued: &remote_view::IssuedCameraShareToken,
    media_session_id: &str,
    share_link_id: &str,
) -> ShareLink {
    ShareLink {
        share_link_id: share_link_id.to_string(),
        media_session_id: media_session_id.to_string(),
        token_hash: remote_view::camera_share_token_hash(&issued.token),
        access_scope: ShareAccessScope::PublicLink,
        expires_at: Some(issued.expires_at_unix_secs.to_string()),
        revoked_at: None,
    }
}

fn share_delivery_mode(target: &ResolvedCameraTarget) -> MediaDeliveryMode {
    match target.primary_stream.transport {
        crate::runtime::registry::StreamTransport::Webrtc => MediaDeliveryMode::Webrtc,
        crate::runtime::registry::StreamTransport::Hls => MediaDeliveryMode::Hls,
        crate::runtime::registry::StreamTransport::Rtsp
        | crate::runtime::registry::StreamTransport::Unknown => MediaDeliveryMode::Hls,
    }
}

fn build_task_run_metadata(request: &TaskRequest, step_id: &str) -> Value {
    json!({
        "trace_id": request.trace_id.clone(),
        "step_id": step_id,
        "surface": request.source.surface.clone(),
        "conversation_id": request.source.conversation_id.clone(),
        "route_key": request.source.route_key.clone(),
        "message_id": request
            .message
            .as_ref()
            .map(|message| message.message_id.clone())
            .unwrap_or_default(),
        "chat_type": request
            .message
            .as_ref()
            .map(|message| message.chat_type.clone())
            .unwrap_or_default(),
        "attachments": task_attachment_transport_contract(request),
        "request_identity": task_request_identity(request),
    })
}

fn build_approval_summary(
    approval: &ApprovalTicket,
    task_run: &TaskRun,
    session: Option<&ConversationSession>,
) -> TaskApprovalSummary {
    TaskApprovalSummary {
        approval_ticket: approval.clone(),
        source_channel: task_run.source_channel.clone(),
        surface: session
            .map(|session| session.surface.clone())
            .unwrap_or_else(|| {
                task_run
                    .metadata
                    .pointer("/surface")
                    .and_then(Value::as_str)
                    .unwrap_or("task_api")
                    .to_string()
            }),
        conversation_id: session
            .map(|session| session.conversation_id.clone())
            .unwrap_or_default(),
        user_id: session
            .map(|session| session.user_id.clone())
            .unwrap_or_else(|| approval.requester_user_id.clone()),
        session_id: task_run.session_id.clone(),
        domain: task_run.domain.clone(),
        action: task_run.action.clone(),
        intent_text: task_run.intent_text.clone(),
        autonomy_level: normalize_task_autonomy_level(&task_run.autonomy_level),
        risk_level: task_run.risk_level,
    }
}

fn inject_approval_args(args: &mut Value, approval_id: &str, approver_user_id: Option<&str>) {
    if !args.is_object() {
        *args = json!({});
    }
    if let Some(object) = args.as_object_mut() {
        let approval_entry = object
            .entry("approval".to_string())
            .or_insert_with(|| json!({}));
        if !approval_entry.is_object() {
            *approval_entry = json!({});
        }
        if let Some(approval_object) = approval_entry.as_object_mut() {
            approval_object.insert("token".to_string(), Value::String(approval_id.to_string()));
            if let Some(approver_user_id) = approver_user_id {
                approval_object.insert(
                    "approver_id".to_string(),
                    Value::String(approver_user_id.to_string()),
                );
            }
        }
    }
}

fn approval_resume_step_id(approval_id: &str) -> String {
    format!("approval:{approval_id}:resume")
}

fn approval_event_step_id(approval_id: &str) -> String {
    format!("approval:{approval_id}:event")
}

fn normalize_task_autonomy_level(level: &str) -> String {
    match level.trim().to_lowercase().as_str() {
        "" => default_task_autonomy_level(),
        "readonly" | "read_only" | "read-only" => "readonly".to_string(),
        "full" => "full".to_string(),
        _ => "supervised".to_string(),
    }
}

fn task_request_from_turn_envelope(envelope: &TaskTurnEnvelope) -> TaskRequest {
    let mut turn_id = envelope.turn.turn_id.trim().to_string();
    if turn_id.is_empty() {
        turn_id = new_turn_id();
    }
    let trace_id = first_non_empty(&[envelope.turn.trace_id.as_str()])
        .map(str::to_string)
        .unwrap_or_else(|| turn_id.clone());
    let conversation_handle = canonical_conversation_handle(envelope);
    let transport_metadata = object_value_or_empty(&envelope.transport.metadata);
    let mut entity_refs = object_value_or_empty(
        transport_metadata
            .get("entity_refs")
            .unwrap_or(&Value::Null),
    );
    if !envelope.actor.workspace_id.trim().is_empty() {
        insert_string_value(
            &mut entity_refs,
            "workspace_id",
            envelope.actor.workspace_id.trim(),
        );
    }
    let mut args = object_value_or_empty(transport_metadata.get("args").unwrap_or(&Value::Null));
    if let Some(continuation) = envelope.continuation.as_ref() {
        if !continuation.token.trim().is_empty() {
            insert_string_value(&mut args, CONTINUATION_TOKEN_KEY, continuation.token.trim());
        }
        if let Ok(value) = serde_json::to_value(continuation) {
            if let Some(object) = args.as_object_mut() {
                object.insert("continuation".to_string(), value);
            }
        }
    }
    let intent = turn_intent_from_metadata(
        &transport_metadata,
        envelope.input.text.trim(),
        DEFAULT_TURN_INTENT_DOMAIN,
        DEFAULT_TURN_INTENT_ACTION,
    );

    TaskRequest {
        task_id: turn_id.clone(),
        trace_id,
        step_id: format!("turn:{turn_id}"),
        source: TaskSource {
            channel: first_non_empty(&[envelope.conversation.channel.as_str()])
                .unwrap_or("im")
                .to_string(),
            surface: first_non_empty(&[envelope.conversation.surface.as_str()])
                .unwrap_or("harborgate")
                .to_string(),
            conversation_id: conversation_handle,
            user_id: first_non_empty(&[envelope.actor.user_id.as_str()])
                .unwrap_or("unknown")
                .to_string(),
            session_id: String::new(),
            route_key: envelope.transport.route_key.trim().to_string(),
        },
        intent,
        entity_refs,
        args,
        autonomy: envelope.autonomy.clone(),
        message: Some(TaskMessage {
            message_id: envelope.transport.message_id.trim().to_string(),
            chat_type: first_non_empty(&[envelope.conversation.chat_type.as_str()])
                .unwrap_or("unknown")
                .to_string(),
            mentions: Vec::new(),
            attachments: task_turn_parts_as_attachments(&envelope.input.parts),
        }),
    }
}

fn canonical_conversation_handle(envelope: &TaskTurnEnvelope) -> String {
    envelope
        .conversation
        .handle
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            stable_prefixed_id(
                "conv_",
                &[
                    envelope.conversation.channel.as_str(),
                    envelope.conversation.surface.as_str(),
                    envelope.conversation.thread_id.as_str(),
                    envelope.actor.user_id.as_str(),
                    envelope.transport.route_key.as_str(),
                ]
                .join("|"),
                24,
            )
        })
}

fn turn_intent_from_metadata(
    metadata: &Value,
    raw_text: &str,
    default_domain: &str,
    default_action: &str,
) -> TaskIntent {
    let intent = metadata.pointer("/intent").unwrap_or(&Value::Null);
    TaskIntent {
        domain: string_at_paths(intent, &["/domain"]).unwrap_or_else(|| default_domain.to_string()),
        action: string_at_paths(intent, &["/action"]).unwrap_or_else(|| default_action.to_string()),
        raw_text: string_at_paths(intent, &["/raw_text"]).unwrap_or_else(|| raw_text.to_string()),
    }
}

fn object_value_or_empty(value: &Value) -> Value {
    if value.is_object() {
        value.clone()
    } else {
        json!({})
    }
}

fn insert_string_value(value: &mut Value, key: &str, item: &str) {
    if !value.is_object() {
        *value = json!({});
    }
    if let Some(object) = value.as_object_mut() {
        object.insert(key.to_string(), Value::String(item.to_string()));
    }
}

fn insert_string_value_if_object(value: &mut Value, key: &str, item: String) {
    if let Some(object) = value.as_object_mut() {
        object.insert(key.to_string(), Value::String(item));
    }
}

fn task_turn_parts_as_attachments(parts: &[Value]) -> Vec<TaskMessageAttachment> {
    parts
        .iter()
        .filter_map(|part| serde_json::from_value::<TaskMessageAttachment>(part.clone()).ok())
        .collect()
}

fn turn_response_from_task_response(
    envelope: &TaskTurnEnvelope,
    request: &TaskRequest,
    response: TaskResponse,
) -> TaskTurnResponse {
    let active_frame = active_frame_from_task_response(&response);
    let delivery_hints = delivery_hints_from_task_response(&response);
    let frame_id = active_frame.as_ref().map(|frame| frame.frame_id.clone());
    let error = (response.status == TaskStatus::Failed).then(|| {
        json!({
            "code": response_error_code(&response).unwrap_or_else(|| "TASK_FAILED".to_string()),
            "message": response.result.message.clone(),
        })
    });

    TaskTurnResponse {
        turn: TaskTurnStatusBlock {
            turn_id: response.task_id.clone(),
            trace_id: response.trace_id.clone(),
            status: response.status,
        },
        conversation: TaskTurnConversationResponse {
            handle: canonical_conversation_handle(envelope),
        },
        active_frame: active_frame.clone(),
        reply: TaskTurnReply {
            kind: turn_reply_kind(&response, active_frame.as_ref()),
            text: response.result.message.clone(),
        },
        artifacts: response.result.artifacts.clone(),
        delivery_hints,
        observability: json!({
            "route_key": request.source.route_key.clone(),
            "message_id": task_message_id(request),
            "frame_id": frame_id,
            "artifact_count": response.result.artifacts.len(),
        }),
        error,
    }
}

fn active_frame_from_task_response(response: &TaskResponse) -> Option<ActiveDialogueFrame> {
    let token = response.resume_token.as_deref()?.trim();
    if token.is_empty() {
        return None;
    }
    let is_clip_confirmation = response.result.data.pointer("/clip_confirmation").is_some();
    let kind = if is_clip_confirmation {
        "camera.clip_confirmation"
    } else if response.executor_used == "agentic_interpreter" {
        "conversation.clarify"
    } else {
        "task.needs_input"
    };
    let expected_reply = if response.result.next_actions.is_empty() {
        response.missing_fields.clone()
    } else {
        response.result.next_actions.clone()
    };
    Some(ActiveDialogueFrame {
        frame_id: stable_prefixed_id("frame_", token, 24),
        kind: kind.to_string(),
        state: if is_clip_confirmation {
            "awaiting_user_choice".to_string()
        } else {
            "awaiting_user_input".to_string()
        },
        expected_reply,
        continuation_token: token.to_string(),
        expires_at: None,
    })
}

fn turn_reply_kind(response: &TaskResponse, active_frame: Option<&ActiveDialogueFrame>) -> String {
    let reply_pack_kind = response
        .result
        .data
        .pointer("/reply_pack/kind")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match reply_pack_kind {
        "conversation_boundary" => "boundary",
        "conversation_repair" => "repair",
        "conversation_cancel" => "cancel",
        "clarify_continue" | "conversation_clarify_continue" => "clarify",
        "conversation_continue" => "conversation",
        _ if active_frame.is_some() => "frame_prompt",
        _ => "tool_result",
    }
    .to_string()
}

fn delivery_hints_from_task_response(response: &TaskResponse) -> Vec<TaskDeliveryHint> {
    let mut hints = response
        .result
        .data
        .pointer("/delivery_hints")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(delivery_hint_from_value)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if response.result.data.pointer("/clip_delivery").is_some()
        && !hints.iter().any(|hint| hint.kind == "native_video")
    {
        let artifact_id = response
            .result
            .artifacts
            .iter()
            .find(|artifact| artifact.kind == "video")
            .and_then(|artifact| artifact.media_asset_id.clone());
        hints.push(TaskDeliveryHint {
            kind: "native_video".to_string(),
            artifact_id,
            fallback: Some("file".to_string()),
            metadata: json!({
                "caption": response
                    .result
                    .data
                    .pointer("/clip_delivery/caption")
                    .and_then(Value::as_str)
                    .unwrap_or("完整回放如下"),
            }),
        });
    }
    if !hints
        .iter()
        .any(|hint| matches!(hint.kind.as_str(), "native_image" | "native_images"))
    {
        let image_artifacts = response
            .result
            .artifacts
            .iter()
            .filter(|artifact| {
                artifact.kind == "image"
                    && artifact
                        .path
                        .as_deref()
                        .map(|path| !path.trim().is_empty())
                        .unwrap_or(false)
            })
            .take(3)
            .collect::<Vec<_>>();
        if let Some(first) = image_artifacts.first() {
            let paths = image_artifacts
                .iter()
                .filter_map(|artifact| artifact.path.clone())
                .collect::<Vec<_>>();
            hints.push(TaskDeliveryHint {
                kind: "native_image".to_string(),
                artifact_id: first.media_asset_id.clone(),
                fallback: Some("text".to_string()),
                metadata: json!({
                    "max_items": 3,
                    "artifact_count": image_artifacts.len(),
                    "preferred_transport": "native_image",
                    "paths": paths,
                }),
            });
        }
    }
    hints
}

fn delivery_hint_from_value(value: &Value) -> Option<TaskDeliveryHint> {
    let kind = value.get("kind").and_then(Value::as_str)?.trim();
    if kind.is_empty() {
        return None;
    }
    Some(TaskDeliveryHint {
        kind: kind.to_string(),
        artifact_id: value
            .get("artifact_id")
            .and_then(Value::as_str)
            .map(str::to_string),
        fallback: value
            .get("fallback")
            .and_then(Value::as_str)
            .map(str::to_string),
        metadata: value.get("metadata").cloned().unwrap_or_else(|| json!({})),
    })
}

fn build_step_input_payload(request: &TaskRequest) -> Value {
    json!({
        "trace_id": request.trace_id.clone(),
        "source": request.source.clone(),
        "intent": request.intent.clone(),
        "entity_refs": request.entity_refs.clone(),
        "args": request.args.clone(),
        "message": request.message.clone(),
    })
}

fn build_step_output_payload(response: &TaskResponse) -> Value {
    let mut payload = json!({
        "message": response.result.message.clone(),
        "data": response.result.data.clone(),
        "events": response.result.events.clone(),
        "next_actions": response.result.next_actions.clone(),
        "missing_fields": response.missing_fields.clone(),
        "prompt": response.prompt.clone(),
        "continuation_token": response.resume_token.clone(),
    });
    if let Some(object) = payload.as_object_mut() {
        object.insert(
            LEGACY_RESUME_TOKEN_KEY.to_string(),
            serde_json::to_value(response.resume_token.clone()).unwrap_or(Value::Null),
        );
    }
    payload
}

fn is_supported_harbor_task(domain: &str, action: &str) -> bool {
    (domain == "service" && matches!(action, "status" | "start" | "stop" | "restart" | "enable"))
        || (domain == "files" && matches!(action, "list" | "copy" | "move"))
}

fn build_harbor_action_from_request(request: &TaskRequest) -> Result<Action, String> {
    let domain = request.intent.domain.trim().to_lowercase();
    let operation = request.intent.action.trim().to_lowercase();

    match (domain.as_str(), operation.as_str()) {
        ("service", _) => build_harbor_service_action(request, &operation),
        ("files", _) => build_harbor_files_action(request, &operation),
        _ => Err(format!(
            "unsupported HarborOS task action: {domain}.{operation}"
        )),
    }
}

fn build_harbor_service_action(request: &TaskRequest, operation: &str) -> Result<Action, String> {
    let service_name = first_string(
        &[&request.args, &request.entity_refs],
        &[
            "/service_name",
            "/resource/service_name",
            "/service/name",
            "/service/id_or_name",
            "/service",
            "/resource/id_or_name",
            "/resource/name",
            "/id_or_name",
            "/name",
        ],
    )
    .ok_or_else(|| "service action requires service_name or resource.id_or_name".to_string())?;

    let mut args = serde_json::Map::new();
    if operation == "enable" {
        args.insert(
            "enable".to_string(),
            json!(
                bool_at_paths(&request.args, &["/enable", "/resource/enable"])
                    .or_else(|| {
                        bool_at_paths(&request.entity_refs, &["/enable", "/resource/enable"])
                    })
                    .unwrap_or(true)
            ),
        );
    }

    Ok(apply_governance_defaults(Action {
        domain: "service".to_string(),
        operation: operation.to_string(),
        resource: json!({
            "service_name": service_name,
        }),
        args: Value::Object(args),
        risk_level: RiskLevel::Low,
        requires_approval: request_requires_approval(request),
        dry_run: request_preview_flag(request),
    }))
}

fn build_harbor_files_action(request: &TaskRequest, operation: &str) -> Result<Action, String> {
    let recursive = bool_at_paths(&request.args, &["/recursive", "/resource/recursive"])
        .or_else(|| bool_at_paths(&request.entity_refs, &["/recursive", "/resource/recursive"]))
        .unwrap_or(false);
    let overwrite = bool_at_paths(&request.args, &["/overwrite", "/resource/overwrite"])
        .or_else(|| bool_at_paths(&request.entity_refs, &["/overwrite", "/resource/overwrite"]))
        .unwrap_or(false);
    let max_bytes = u64_at_paths(&request.args, &["/max_bytes", "/resource/max_bytes"])
        .or_else(|| u64_at_paths(&request.entity_refs, &["/max_bytes", "/resource/max_bytes"]));

    let mut args = serde_json::Map::new();
    if recursive {
        args.insert("recursive".to_string(), json!(true));
    }
    if overwrite {
        args.insert("overwrite".to_string(), json!(true));
    }
    if let Some(max_bytes) = max_bytes {
        args.insert("max_bytes".to_string(), json!(max_bytes));
    }

    let resource = match operation {
        "list" => {
            let path = first_string(
                &[&request.args, &request.entity_refs],
                &[
                    "/path",
                    "/resource/path",
                    "/paths/0",
                    "/resource/paths/0",
                    "/source",
                    "/resource/source",
                    "/src",
                    "/resource/src",
                ],
            )
            .ok_or_else(|| "files.list requires path or resource.path".to_string())?;
            json!({
                "path": path.clone(),
                "paths": [path],
            })
        }
        "copy" | "move" => {
            let source = first_string(
                &[&request.args, &request.entity_refs],
                &[
                    "/source",
                    "/resource/source",
                    "/src",
                    "/resource/src",
                    "/paths/0",
                    "/resource/paths/0",
                    "/path",
                    "/resource/path",
                ],
            )
            .ok_or_else(|| "files action requires source or resource.paths[0]".to_string())?;
            let target = first_string(
                &[&request.args, &request.entity_refs],
                &[
                    "/target",
                    "/resource/target",
                    "/destination",
                    "/resource/destination",
                    "/dst",
                    "/resource/dst",
                    "/paths/1",
                    "/resource/paths/1",
                ],
            )
            .ok_or_else(|| "files action requires target or resource.destination".to_string())?;
            json!({
                "source": source.clone(),
                "target": target,
                "paths": [source],
            })
        }
        _ => return Err(format!("unsupported HarborOS files operation: {operation}")),
    };

    Ok(apply_governance_defaults(Action {
        domain: "files".to_string(),
        operation: operation.to_string(),
        resource,
        args: Value::Object(args),
        risk_level: RiskLevel::Low,
        requires_approval: request_requires_approval(request),
        dry_run: request_preview_flag(request),
    }))
}

fn request_preview_flag(request: &TaskRequest) -> bool {
    bool_at_paths(
        &request.args,
        &[
            "/dry_run",
            "/preview",
            "/resource/dry_run",
            "/resource/preview",
        ],
    )
    .or_else(|| {
        bool_at_paths(
            &request.entity_refs,
            &[
                "/dry_run",
                "/preview",
                "/resource/dry_run",
                "/resource/preview",
            ],
        )
    })
    .unwrap_or(false)
}

fn harbor_execution_is_preview(payload: &Value) -> bool {
    bool_at_paths(payload, &["/dry_run"]).unwrap_or(false)
        || matches!(payload.pointer("/passthrough"), Some(&Value::Bool(false)))
        || string_at_paths(payload, &["/note"])
            .map(|value| value.to_ascii_lowercase().contains("preview"))
            .unwrap_or(false)
}

fn non_empty_audit_ref(audit_ref: &str) -> String {
    let trimmed = audit_ref.trim();
    if trimmed.is_empty() {
        new_audit_ref()
    } else {
        trimmed.to_string()
    }
}

fn build_artifact_records(
    request: &TaskRequest,
    step_id: &str,
    artifacts: &[TaskArtifact],
) -> Vec<ArtifactRecord> {
    artifacts
        .iter()
        .enumerate()
        .map(|(index, artifact)| ArtifactRecord {
            artifact_id: format!("{}:{}:artifact-{}", request.task_id, step_id, index + 1),
            task_id: request.task_id.clone(),
            trace_id: request.trace_id.clone(),
            step_id: Some(step_id.to_string()),
            route_key: request.source.route_key.clone(),
            artifact_kind: artifact_kind_from_name(&artifact.kind),
            label: artifact.label.clone(),
            mime_type: artifact.mime_type.clone(),
            media_asset_id: artifact.media_asset_id.clone(),
            path: artifact.path.clone(),
            url: artifact.url.clone(),
            metadata: artifact.metadata.clone(),
        })
        .collect()
}

fn build_event_records(request: &TaskRequest, step_id: &str, events: &[Value]) -> Vec<EventRecord> {
    events
        .iter()
        .filter_map(|event| serde_json::from_value::<EventRecord>(event.clone()).ok())
        .map(|mut event| {
            if event.workspace_id.trim().is_empty() {
                event.workspace_id = workspace_id_for_request(request);
            }
            if event.source_id.trim().is_empty() {
                event.source_id = request.task_id.clone();
            }
            if event.correlation_id.is_none() && !request.trace_id.trim().is_empty() {
                event.correlation_id = Some(request.trace_id.clone());
            }
            if event.causation_id.is_none() {
                event.causation_id = Some(step_id.to_string());
            }
            if event.occurred_at.is_none() {
                event.occurred_at = Some(current_timestamp());
            }
            if event.ingested_at.is_none() {
                event.ingested_at = event.occurred_at.clone();
            }
            event
        })
        .collect()
}

fn build_task_event_record(
    request: &TaskRequest,
    step_id: &str,
    event_type: &str,
    severity: EventSeverity,
    payload: Value,
) -> EventRecord {
    let occurred_at = current_timestamp();
    EventRecord {
        event_id: new_event_id(),
        workspace_id: workspace_id_for_request(request),
        source_kind: EventSourceKind::Task,
        source_id: request.task_id.clone(),
        event_type: event_type.to_string(),
        severity,
        payload,
        correlation_id: (!request.trace_id.trim().is_empty()).then(|| request.trace_id.clone()),
        causation_id: Some(step_id.to_string()),
        occurred_at: Some(occurred_at.clone()),
        ingested_at: Some(occurred_at),
    }
}

fn artifact_kind_from_name(kind: &str) -> ArtifactKind {
    match kind.trim().to_lowercase().as_str() {
        "image" => ArtifactKind::Image,
        "video" => ArtifactKind::Video,
        "link" => ArtifactKind::Link,
        "card" => ArtifactKind::Card,
        "json" => ArtifactKind::Json,
        _ => ArtifactKind::Text,
    }
}

fn task_artifact_from_record(record: ArtifactRecord) -> TaskArtifact {
    TaskArtifact {
        kind: task_artifact_kind_name(record.artifact_kind).to_string(),
        label: record.label,
        mime_type: record.mime_type,
        media_asset_id: record.media_asset_id,
        path: record.path,
        url: record.url,
        metadata: record.metadata,
    }
}

fn task_artifact_kind_name(kind: ArtifactKind) -> &'static str {
    match kind {
        ArtifactKind::Text => "text",
        ArtifactKind::Image => "image",
        ArtifactKind::Video => "video",
        ArtifactKind::Link => "link",
        ArtifactKind::Card => "card",
        ArtifactKind::Json => "json",
    }
}

fn task_request_identity(request: &TaskRequest) -> Value {
    json!({
        "route_key": request.source.route_key.trim(),
        "conversation_id": request.source.conversation_id.trim(),
        "message_id": task_message_id(request),
        "intent": {
            "domain": request.intent.domain.trim(),
            "action": request.intent.action.trim(),
            "raw_text": request.intent.raw_text.trim(),
        },
        "entity_refs": normalized_contract_value(&request.entity_refs),
        "args": normalized_contract_value(&request.args),
    })
}

fn persisted_task_request_identity(task_run: &TaskRun) -> Value {
    if let Some(identity) = task_run.metadata.pointer("/request_identity") {
        return identity.clone();
    }

    json!({
        "route_key": task_run
            .metadata
            .pointer("/route_key")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        "conversation_id": task_run
            .metadata
            .pointer("/conversation_id")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        "message_id": task_run
            .metadata
            .pointer("/message_id")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        "intent": {
            "domain": task_run.domain.trim(),
            "action": task_run.action.trim(),
            "raw_text": task_run.intent_text.trim(),
        },
        "entity_refs": normalized_contract_value(&task_run.entity_refs),
        "args": normalized_contract_value(&task_run.args),
    })
}

fn normalized_contract_value(value: &Value) -> Value {
    match value {
        Value::Null => json!({}),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(normalized_contract_value)
                .collect::<Vec<_>>(),
        ),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), normalized_contract_value(value)))
                .collect(),
        ),
        Value::String(value) => Value::String(value.trim().to_string()),
        _ => value.clone(),
    }
}

fn upsert_json_string(target: &mut Value, pointer: &str, value: &str) {
    ensure_json_pointer_parent(target, pointer);
    if let Some((parent_pointer, leaf)) = split_json_pointer(pointer) {
        if let Some(parent) = target.pointer_mut(parent_pointer) {
            if let Some(map) = parent.as_object_mut() {
                map.insert(leaf.to_string(), Value::String(value.to_string()));
            }
        }
    }
}

fn upsert_json_value(target: &mut Value, pointer: &str, value: Value) {
    ensure_json_pointer_parent(target, pointer);
    if let Some((parent_pointer, leaf)) = split_json_pointer(pointer) {
        if let Some(parent) = target.pointer_mut(parent_pointer) {
            if let Some(map) = parent.as_object_mut() {
                map.insert(leaf.to_string(), value);
            }
        }
    }
}

fn ensure_json_pointer_parent(target: &mut Value, pointer: &str) {
    if !target.is_object() {
        *target = json!({});
    }
    let Some((parent_pointer, _)) = split_json_pointer(pointer) else {
        return;
    };
    let mut current = target;
    for segment in parent_pointer
        .split('/')
        .filter(|segment| !segment.is_empty())
    {
        let segment = segment.replace("~1", "/").replace("~0", "~");
        if !current.is_object() {
            *current = json!({});
        }
        let map = current.as_object_mut().expect("object");
        current = map.entry(segment).or_insert_with(|| json!({}));
    }
}

fn split_json_pointer(pointer: &str) -> Option<(&str, &str)> {
    pointer.rsplit_once('/')
}

fn task_message_id(request: &TaskRequest) -> String {
    request
        .message
        .as_ref()
        .map(|message| message.message_id.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
}

fn task_chat_type(request: &TaskRequest) -> String {
    request
        .message
        .as_ref()
        .map(|message| message.chat_type.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_default()
}

fn task_attachment_transport_contract(request: &TaskRequest) -> Value {
    let Some(message) = request.message.as_ref() else {
        return Value::Array(Vec::new());
    };

    Value::Array(
        message
            .attachments
            .iter()
            .map(|attachment| {
                let download = attachment
                    .download
                    .as_ref()
                    .map(|download| {
                        json!({
                            "mode": download.mode.trim(),
                            "url": download.url.trim(),
                            "method": download.method.trim(),
                            "headers": normalized_contract_value(&download.headers),
                            "auth": download
                                .auth
                                .as_ref()
                                .map(|auth| json!({"type": auth.kind.trim()}))
                                .unwrap_or(Value::Null),
                            "expires_at": download.expires_at.trim(),
                            "max_size_bytes": download.max_size_bytes,
                        })
                    })
                    .unwrap_or(Value::Null);

                json!({
                    "attachment_id": attachment.attachment_id.trim(),
                    "type": attachment.attachment_type.trim(),
                    "name": attachment.name.trim(),
                    "mime_type": attachment.mime_type.trim(),
                    "size_bytes": attachment.size_bytes,
                    "download": download,
                    "metadata": normalized_contract_value(&attachment.metadata),
                })
            })
            .collect(),
    )
}

fn string_vec_at_paths(value: &Value, paths: &[&str]) -> Vec<String> {
    paths
        .iter()
        .find_map(|path| {
            value.pointer(path).and_then(Value::as_array).map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|item| item.trim().to_string())
                    .filter(|item| !item.is_empty())
                    .collect::<Vec<_>>()
            })
        })
        .unwrap_or_default()
}

fn source_route_key_from_context(
    task_run: &TaskRun,
    session: Option<&ConversationSession>,
) -> String {
    session
        .map(|session| session.route_key.trim().to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            task_run
                .metadata
                .pointer("/route_key")
                .and_then(Value::as_str)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_default()
}

fn task_status_from_task_run_status(status: TaskRunStatus) -> TaskStatus {
    match status {
        TaskRunStatus::Completed => TaskStatus::Completed,
        TaskRunStatus::NeedsInput | TaskRunStatus::Blocked => TaskStatus::NeedsInput,
        TaskRunStatus::Queued | TaskRunStatus::Running | TaskRunStatus::Failed => {
            TaskStatus::Failed
        }
    }
}

fn conversation_handle_for_request(request: &TaskRequest) -> String {
    let TaskSource {
        session_id,
        conversation_id,
        user_id,
        ..
    } = &request.source;
    first_non_empty(&[
        session_id.as_str(),
        conversation_id.as_str(),
        user_id.as_str(),
    ])
    .map(|value| value.to_string())
    .unwrap_or_else(|| format!("task-{}", request.task_id))
}

fn step_id_for_request(request: &TaskRequest) -> String {
    let step_id = first_non_empty(&[request.step_id.as_str()])
        .map(|value| value.to_string())
        .unwrap_or_else(|| "s1".to_string());
    if looks_like_turn_local_step_id(&step_id) {
        format!("{}:{step_id}", request.task_id)
    } else {
        step_id
    }
}

fn looks_like_turn_local_step_id(step_id: &str) -> bool {
    step_id
        .strip_prefix("step_")
        .is_some_and(|suffix| !suffix.is_empty() && suffix.chars().all(|ch| ch.is_ascii_digit()))
}

fn workspace_id_for_request(request: &TaskRequest) -> String {
    first_string(&[&request.entity_refs, &request.args], &["/workspace_id"])
        .unwrap_or_else(|| "home-1".to_string())
}

fn url_encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            use std::fmt::Write as _;
            let _ = write!(&mut encoded, "%{byte:02X}");
        }
    }
    encoded
}

fn default_task_autonomy_level() -> String {
    "supervised".to_string()
}

fn expected_risk_level(request: &TaskRequest) -> RiskLevel {
    effective_risk_level(&Action {
        domain: request.intent.domain.trim().to_lowercase(),
        operation: request.intent.action.trim().to_lowercase(),
        resource: Value::Null,
        args: request.args.clone(),
        risk_level: RiskLevel::Low,
        requires_approval: request_requires_approval(request),
        dry_run: false,
    })
}

fn effective_autonomy_level(request: &TaskRequest) -> AutonomyLevel {
    let normalized = request.autonomy.level.trim().to_lowercase();
    match normalized.as_str() {
        "" | "supervised" => AutonomyLevel::Supervised,
        "readonly" | "read_only" | "read-only" => AutonomyLevel::ReadOnly,
        "full" => AutonomyLevel::Full,
        _ => AutonomyLevel::Supervised,
    }
}

fn effective_autonomy_level_for_task_run(request: &TaskRequest) -> String {
    autonomy_level_label(effective_autonomy_level(request)).to_string()
}

fn autonomy_level_label(level: AutonomyLevel) -> &'static str {
    match level {
        AutonomyLevel::ReadOnly => "readonly",
        AutonomyLevel::Supervised => "supervised",
        AutonomyLevel::Full => "full",
    }
}

fn approval_manager_for_level(level: AutonomyLevel) -> ApprovalManager {
    ApprovalManager::for_non_interactive(&AutonomyConfig {
        level,
        ..AutonomyConfig::default()
    })
}

fn effective_requires_approval(request: &TaskRequest) -> bool {
    let action = apply_governance_defaults(Action {
        domain: request.intent.domain.trim().to_lowercase(),
        operation: request.intent.action.trim().to_lowercase(),
        resource: Value::Null,
        args: request.args.clone(),
        risk_level: RiskLevel::Low,
        requires_approval: request_requires_approval(request),
        dry_run: false,
    });
    action.requires_approval
}

fn request_requires_approval(request: &TaskRequest) -> bool {
    bool_at_paths(&request.args, &["/approval/required", "/requires_approval"]).unwrap_or(false)
}

fn request_approval_token(request: &TaskRequest) -> Option<String> {
    first_string(
        &[&request.args, &request.entity_refs],
        &["/approval/token", "/approval_token"],
    )
}

fn continuation_token_from_request(request: &TaskRequest) -> Option<String> {
    first_string(
        &[&request.args, &request.entity_refs],
        &[CONTINUATION_TOKEN_POINTER, LEGACY_RESUME_TOKEN_POINTER],
    )
}

fn request_approver_id(request: &TaskRequest) -> Option<String> {
    first_string(
        &[&request.args, &request.entity_refs],
        &["/approval/approver_id", "/approver_id"],
    )
}

fn approval_context_for_request(
    request: &TaskRequest,
    pending_approval: Option<&ApprovalTicket>,
) -> Option<ApprovalContext> {
    let token = request_approval_token(request);
    let required_token = pending_approval.map(|approval| approval.approval_id.clone());
    let approver_id = request_approver_id(request);
    if token.is_none() && required_token.is_none() && approver_id.is_none() {
        return None;
    }
    Some(ApprovalContext {
        token,
        required_token,
        approver_id,
    })
}

fn task_run_status_from_response(status: TaskStatus) -> TaskRunStatus {
    match status {
        TaskStatus::Completed => TaskRunStatus::Completed,
        TaskStatus::NeedsInput => TaskRunStatus::NeedsInput,
        TaskStatus::Failed => TaskRunStatus::Failed,
    }
}

fn task_step_status_from_response(status: TaskStatus) -> TaskStepRunStatus {
    match status {
        TaskStatus::Completed => TaskStepRunStatus::Success,
        TaskStatus::NeedsInput => TaskStepRunStatus::Blocked,
        TaskStatus::Failed => TaskStepRunStatus::Failed,
    }
}

fn execution_route_for_executor(executor_used: &str) -> ExecutionRoute {
    match executor_used.trim().to_lowercase().as_str() {
        "middleware_api" => ExecutionRoute::MiddlewareApi,
        "midcli" => ExecutionRoute::Midcli,
        "browser" => ExecutionRoute::Browser,
        "mcp" => ExecutionRoute::Mcp,
        _ => ExecutionRoute::Local,
    }
}

fn response_error_code(response: &TaskResponse) -> Option<String> {
    string_at_paths(
        &response.result.data,
        &[
            "/error_code",
            "/error/code",
            "/result/error_code",
            "/result/error/code",
        ],
    )
}

fn task_run_completed_at(status: TaskStatus, finished_at: &str) -> Option<String> {
    match status {
        TaskStatus::Completed | TaskStatus::Failed => Some(finished_at.to_string()),
        TaskStatus::NeedsInput => None,
    }
}

fn step_identity(request: &TaskRequest, response: &TaskResponse) -> (String, String) {
    if response.executor_used == "vision_executor" {
        return ("vision".to_string(), OP_ANALYZE_CAMERA.to_string());
    }
    (request.intent.domain.clone(), request.intent.action.clone())
}

fn protocol_string(args: &Value) -> Option<String> {
    if let Some(value) = string_at_paths(args, &["/protocol"]) {
        return Some(value);
    }
    args.pointer("/protocols")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
        })
        .filter(|values| !values.is_empty())
        .map(|values| values.join(" + "))
}

fn conversation_key(request: &TaskRequest) -> Option<String> {
    let TaskSource {
        conversation_id,
        session_id,
        user_id,
        ..
    } = &request.source;
    first_non_empty(&[
        conversation_id.as_str(),
        session_id.as_str(),
        user_id.as_str(),
    ])
    .map(|value| value.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HomeAssistantNaturalAction {
    Request(HomeAssistantNaturalActionRequest),
    Blocked { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HomeAssistantNaturalActionRequest {
    domain: String,
    service: String,
    entity_hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
enum HomeAssistantEntityResolution {
    Resolved(HomeAssistantEntity),
    Clarify(Vec<HomeAssistantEntity>),
    Missing(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PendingHomeAssistantCandidateSelection {
    Selected(PendingTaskHomeAssistantCandidate),
    Ambiguous(Vec<PendingTaskHomeAssistantCandidate>),
    Missing,
}

fn normalize_command_text(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .filter(|ch| {
            !ch.is_whitespace() && !matches!(ch, '，' | '。' | ',' | '.' | '？' | '?' | '！' | '!')
        })
        .collect()
}

fn infer_home_assistant_natural_action(raw_text: &str) -> Option<HomeAssistantNaturalAction> {
    let normalized = normalize_command_text(raw_text);
    if normalized.is_empty() {
        return None;
    }
    if looks_like_unsafe_home_assistant_request(&normalized) {
        return Some(HomeAssistantNaturalAction::Blocked {
            message: "这类 Home Assistant 动作不在低风险 allowlist 中，本次没有执行。".to_string(),
        });
    }
    if matches_any(&normalized, &["场景", "scene"]) {
        return Some(HomeAssistantNaturalAction::Request(
            HomeAssistantNaturalActionRequest {
                domain: "scene".to_string(),
                service: "turn_on".to_string(),
                entity_hint: infer_home_assistant_entity_hint(&normalized, "scene"),
            },
        ));
    }
    if matches_any(
        &normalized,
        &["inputboolean", "input_boolean", "布尔", "输入布尔"],
    ) {
        return power_home_assistant_action(&normalized, "input_boolean");
    }
    if matches_any(&normalized, &["灯", "电灯", "灯光", "light"]) {
        return power_home_assistant_action(&normalized, "light");
    }
    if matches_any(&normalized, &["开关", "switch"]) {
        return power_home_assistant_action(&normalized, "switch");
    }
    None
}

fn power_home_assistant_action(
    normalized: &str,
    domain: &str,
) -> Option<HomeAssistantNaturalAction> {
    let service = if matches_any(normalized, &["切换", "toggle", "反转"]) {
        "toggle"
    } else if matches_any(normalized, &["关闭", "关掉", "关灯", "关开关", "turnoff"]) {
        "turn_off"
    } else if matches_any(
        normalized,
        &["打开", "开启", "开灯", "打开开关", "开开关", "turnon"],
    ) {
        "turn_on"
    } else {
        return None;
    };
    Some(HomeAssistantNaturalAction::Request(
        HomeAssistantNaturalActionRequest {
            domain: domain.to_string(),
            service: service.to_string(),
            entity_hint: infer_home_assistant_entity_hint(normalized, domain),
        },
    ))
}

fn looks_like_unsafe_home_assistant_request(normalized: &str) -> bool {
    matches_any(
        normalized,
        &[
            "重启homeassistant",
            "重启ha",
            "重启系统",
            "重启网关",
            "解锁",
            "开锁",
            "开门",
            "打开门锁",
            "设置温度",
            "设温度",
            "调温度",
            "打开空调",
            "关闭空调",
            "开空调",
            "关空调",
            "打开阀门",
            "关闭阀门",
        ],
    )
}

fn infer_home_assistant_entity_hint(normalized: &str, domain: &str) -> Option<String> {
    let tokens = [
        "门口",
        "玄关",
        "客厅",
        "厨房",
        "卧室",
        "书房",
        "阳台",
        "车库",
        "测试",
        "餐厅",
        "kitchen",
        "livingroom",
        "frontdoor",
        "porch",
        "test",
    ];
    tokens
        .iter()
        .find(|token| normalized.contains(*token))
        .map(|token| token.to_string())
        .or_else(|| {
            (domain == "scene" && normalized.contains("测试场景")).then(|| "测试".to_string())
        })
}

fn resolve_home_assistant_action_entity(
    action: &HomeAssistantNaturalActionRequest,
    entities: &[HomeAssistantEntity],
) -> HomeAssistantEntityResolution {
    let domain_entities = entities
        .iter()
        .filter(|entity| entity.domain.eq_ignore_ascii_case(&action.domain))
        .cloned()
        .collect::<Vec<_>>();
    if domain_entities.is_empty() {
        return HomeAssistantEntityResolution::Missing(format!(
            "没有找到 {} 实体，本次没有执行。",
            home_assistant_domain_label(&action.domain)
        ));
    }
    let matched = action
        .entity_hint
        .as_deref()
        .map(|hint| {
            let hint_terms = normalized_home_assistant_hint_terms(hint);
            domain_entities
                .iter()
                .filter(|entity| home_assistant_entity_matches_terms(entity, &hint_terms))
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| domain_entities.clone());

    if matched.len() == 1 {
        return HomeAssistantEntityResolution::Resolved(matched[0].clone());
    }
    if matched.is_empty() && domain_entities.len() == 1 {
        return HomeAssistantEntityResolution::Resolved(domain_entities[0].clone());
    }
    if matched.is_empty() {
        return HomeAssistantEntityResolution::Missing(format!(
            "没有找到匹配“{}”的 {} 实体，本次没有执行。",
            action.entity_hint.clone().unwrap_or_default(),
            home_assistant_domain_label(&action.domain)
        ));
    }
    HomeAssistantEntityResolution::Clarify(matched)
}

fn redacted_home_assistant_entity_summary(entity: &HomeAssistantEntity) -> Value {
    json!({
        "entity_id": entity.entity_id,
        "domain": entity.domain,
        "display_name": entity.display_name,
        "state": entity.state,
        "area_id": entity.area_id,
        "device_class": entity.device_class,
        "redacted": true,
    })
}

fn pending_home_assistant_candidates_from_entities(
    candidates: &[HomeAssistantEntity],
) -> Vec<PendingTaskHomeAssistantCandidate> {
    candidates
        .iter()
        .take(5)
        .map(|entity| PendingTaskHomeAssistantCandidate {
            entity_id: entity.entity_id.clone(),
            domain: entity.domain.clone(),
            display_name: entity.display_name.clone(),
            state: entity.state.clone(),
            area_id: entity.area_id.clone(),
            device_class: entity.device_class.clone(),
        })
        .collect()
}

fn redacted_pending_home_assistant_candidate_summary(
    candidate: &PendingTaskHomeAssistantCandidate,
) -> Value {
    json!({
        "entity_id": candidate.entity_id,
        "domain": candidate.domain,
        "display_name": candidate.display_name,
        "state": candidate.state,
        "area_id": candidate.area_id,
        "device_class": candidate.device_class,
        "redacted": true,
    })
}

fn home_assistant_pending_clarification_actions(
    candidates: &[PendingTaskHomeAssistantCandidate],
) -> Vec<String> {
    let mut actions = vec!["第一个".to_string(), "取消".to_string()];
    actions.extend(
        candidates
            .iter()
            .take(3)
            .map(|candidate| candidate.display_name.clone())
            .filter(|name| !name.trim().is_empty()),
    );
    actions
}

fn home_assistant_clarification_prompt(
    action: &HomeAssistantNaturalActionRequest,
    candidates: &[PendingTaskHomeAssistantCandidate],
) -> String {
    let candidate_lines = candidates
        .iter()
        .enumerate()
        .map(|entity| {
            format!(
                "{}. {} ({})",
                entity.0 + 1,
                entity.1.display_name,
                entity.1.entity_id
            )
        })
        .collect::<Vec<_>>()
        .join("；");
    if candidate_lines.is_empty() {
        return format!(
            "{} 有多个可操作实体，请回复更具体的名称，或“取消”。",
            home_assistant_domain_label(&action.domain)
        );
    }
    format!(
        "{} 有多个可操作实体，请回复：第一个 / 名称 / 取消。{}",
        home_assistant_domain_label(&action.domain),
        candidate_lines
    )
}

fn resolve_pending_home_assistant_candidate_selection(
    pending: &PendingTaskHomeAssistantClarification,
    raw_text: &str,
) -> PendingHomeAssistantCandidateSelection {
    let normalized = normalize_command_text(raw_text);
    if normalized.is_empty() {
        return PendingHomeAssistantCandidateSelection::Missing;
    }
    if let Some(index) = parse_home_assistant_candidate_ordinal(&normalized) {
        return pending
            .candidates
            .get(index.saturating_sub(1))
            .cloned()
            .map(PendingHomeAssistantCandidateSelection::Selected)
            .unwrap_or(PendingHomeAssistantCandidateSelection::Missing);
    }

    let matches = pending
        .candidates
        .iter()
        .filter(|candidate| pending_home_assistant_candidate_matches(candidate, &normalized))
        .cloned()
        .collect::<Vec<_>>();
    match matches.len() {
        0 => PendingHomeAssistantCandidateSelection::Missing,
        1 => PendingHomeAssistantCandidateSelection::Selected(matches[0].clone()),
        _ => PendingHomeAssistantCandidateSelection::Ambiguous(matches),
    }
}

fn parse_home_assistant_candidate_ordinal(normalized: &str) -> Option<usize> {
    match normalized {
        "1" | "一" | "第一个" | "第一" | "第1个" | "第1" | "1号" => Some(1),
        "2" | "二" | "第二个" | "第二" | "第2个" | "第2" | "2号" => Some(2),
        "3" | "三" | "第三个" | "第三" | "第3个" | "第3" | "3号" => Some(3),
        "4" | "四" | "第四个" | "第四" | "第4个" | "第4" | "4号" => Some(4),
        "5" | "五" | "第五个" | "第五" | "第5个" | "第5" | "5号" => Some(5),
        _ => None,
    }
}

fn pending_home_assistant_candidate_matches(
    candidate: &PendingTaskHomeAssistantCandidate,
    normalized_reply: &str,
) -> bool {
    let reply_terms = normalized_home_assistant_hint_terms(normalized_reply);
    let candidate_text = normalize_command_text(&format!(
        "{}{}{}{}",
        candidate.entity_id,
        candidate.display_name,
        candidate.area_id.as_deref().unwrap_or_default(),
        candidate.device_class.as_deref().unwrap_or_default()
    ));
    reply_terms
        .iter()
        .any(|term| candidate_text == *term || candidate_text.contains(term))
}

fn home_assistant_entity_matches_terms(entity: &HomeAssistantEntity, terms: &[String]) -> bool {
    let entity_text = normalize_command_text(&format!(
        "{}{}{}{}",
        entity.entity_id,
        entity.display_name,
        entity.area_id.as_deref().unwrap_or_default(),
        entity.device_class.as_deref().unwrap_or_default()
    ));
    terms
        .iter()
        .any(|term| entity_text == *term || entity_text.contains(term))
}

fn normalized_home_assistant_hint_terms(hint: &str) -> Vec<String> {
    let normalized = normalize_command_text(hint);
    if normalized.is_empty() {
        return Vec::new();
    }
    let mut terms = Vec::new();
    push_unique_normalized_term(&mut terms, &normalized);
    for alias in home_assistant_hint_aliases(&normalized) {
        push_unique_normalized_term(&mut terms, alias);
    }
    terms
}

fn push_unique_normalized_term(terms: &mut Vec<String>, raw: &str) {
    let normalized = normalize_command_text(raw);
    if !normalized.is_empty() && !terms.iter().any(|term| term == &normalized) {
        terms.push(normalized);
    }
}

fn home_assistant_hint_aliases(normalized: &str) -> Vec<&'static str> {
    let mut aliases = Vec::new();
    if normalized.contains("测试") {
        aliases.extend(["test", "harbordocktest"]);
    }
    if normalized.contains("门口") {
        aliases.extend(["frontdoor", "porch", "entry", "entrance"]);
    }
    if normalized.contains("玄关") {
        aliases.extend(["entry", "entrance", "foyer", "porch"]);
    }
    if normalized.contains("客厅") {
        aliases.extend(["livingroom", "living", "lounge"]);
    }
    if normalized.contains("厨房") {
        aliases.push("kitchen");
    }
    if normalized.contains("卧室") {
        aliases.push("bedroom");
    }
    if normalized.contains("书房") {
        aliases.extend(["study", "office"]);
    }
    if normalized.contains("阳台") {
        aliases.push("balcony");
    }
    if normalized.contains("车库") {
        aliases.push("garage");
    }
    if normalized.contains("餐厅") {
        aliases.extend(["diningroom", "dining"]);
    }
    aliases
}

fn home_assistant_domain_label(domain: &str) -> &'static str {
    match domain {
        "light" => "灯",
        "switch" => "开关",
        "input_boolean" => "输入布尔",
        "scene" => "场景",
        _ => "Home Assistant",
    }
}

fn home_assistant_client_from_admin_state(
    state: &crate::runtime::admin_console::HomeAssistantAdminState,
) -> Result<HomeAssistantClient, String> {
    if !state.enabled {
        return Err("Home Assistant integration is disabled".to_string());
    }
    let config = HomeAssistantClientConfig::new(&state.base_url, &state.access_token);
    if !config.configured() {
        return Err("Home Assistant base URL and access token are required".to_string());
    }
    HomeAssistantClient::new(config)
}

fn default_notification_target_record(
    targets: &[NotificationTargetRecord],
) -> Option<&NotificationTargetRecord> {
    targets
        .iter()
        .find(|target| target.is_default && !target.route_key.trim().is_empty())
        .or_else(|| {
            targets
                .iter()
                .find(|target| !target.route_key.trim().is_empty())
        })
}

fn notification_delivery_record_summary(record: &NotificationDeliveryRecord) -> Value {
    json!({
        "delivery_id": record.delivery_id,
        "notification_id": record.notification_id,
        "trace_id": record.trace_id,
        "ok": record.ok,
        "status": serde_json::to_value(record.status).unwrap_or(Value::Null),
        "platform": record.platform,
        "retryable": record.retryable,
        "error": record.error.as_ref().map(|error| json!({
            "code": error.code,
            "message": redact_task_api_text(&error.message),
        })),
        "redacted": true,
    })
}

fn classify_task_notification_delivery_error(
    error: NotificationDeliveryError,
) -> (&'static str, String, &'static str, EventSeverity) {
    match error {
        NotificationDeliveryError::MissingConfiguration(message) => (
            "blocked",
            redact_task_api_text(&message),
            "missing_configuration",
            EventSeverity::Warning,
        ),
        NotificationDeliveryError::Transport(message) => (
            "failed",
            redact_task_api_text(&message),
            "gateway_unreachable",
            EventSeverity::Error,
        ),
        NotificationDeliveryError::InvalidResponse(message) => (
            "failed",
            redact_task_api_text(&message),
            "invalid_response",
            EventSeverity::Error,
        ),
        NotificationDeliveryError::RequestRejected {
            status_code,
            envelope,
        } => (
            "failed",
            redact_task_api_text(&format!(
                "HarborGate rejected the notification with HTTP {status_code}: {} ({})",
                envelope.error.message, envelope.error.code
            )),
            "request_rejected",
            EventSeverity::Error,
        ),
    }
}

fn build_task_default_notification_target_readiness(
    targets: &[NotificationTargetRecord],
    gateway_status: Option<&Value>,
) -> Value {
    let target = default_notification_target_record(targets);
    let gateway_connected = task_gateway_connected(gateway_status);
    let gateway_configured = task_gateway_configured(gateway_status);
    let status = if target.is_none() {
        "not_configured"
    } else if gateway_connected {
        "available"
    } else if gateway_configured {
        "degraded"
    } else {
        "blocked"
    };
    json!({
        "status": status,
        "target_configured": target.is_some(),
        "target_label": target.map(|target| target.label.clone()),
        "platform_hint": target.and_then(|target| non_empty_task_string(&target.platform_hint)),
        "gateway_configured": gateway_configured,
        "gateway_connected": gateway_connected,
        "route_key_redacted": target.is_some(),
    })
}

fn build_task_weixin_gateway_status(gateway_status: Option<&Value>) -> Value {
    let Some(payload) = gateway_status else {
        return json!({
            "status": "unavailable",
            "configured": false,
            "connected": false,
            "context_token_count": Value::Null,
            "redacted": true,
        });
    };
    let channel = find_gateway_channel(payload, "weixin");
    let configured = channel
        .and_then(|value| value.get("configured").and_then(Value::as_bool))
        .or_else(|| {
            payload
                .pointer("/weixin/configured")
                .and_then(Value::as_bool)
        })
        .or_else(|| {
            payload
                .pointer("/bridge_provider/configured")
                .and_then(Value::as_bool)
        })
        .unwrap_or(false);
    let connected = channel
        .and_then(|value| value.get("connected").and_then(Value::as_bool))
        .or_else(|| {
            payload
                .pointer("/weixin/connected")
                .and_then(Value::as_bool)
        })
        .or_else(|| {
            payload
                .pointer("/bridge_provider/connected")
                .and_then(Value::as_bool)
        })
        .unwrap_or(false);
    let context_token_count = payload
        .pointer("/context_tokens/count")
        .and_then(Value::as_u64)
        .or_else(|| {
            payload
                .pointer("/route_context/token_count")
                .and_then(Value::as_u64)
        })
        .or_else(|| {
            payload
                .pointer("/context_token_count")
                .and_then(Value::as_u64)
        });
    json!({
        "status": if connected {
            "connected"
        } else if configured {
            "configured"
        } else {
            "not_configured"
        },
        "configured": configured,
        "connected": connected,
        "context_token_count": context_token_count,
        "context_token_redacted": context_token_count.is_some(),
        "redacted": true,
    })
}

fn find_gateway_channel<'a>(payload: &'a Value, platform: &str) -> Option<&'a Value> {
    payload
        .get("channels")
        .or_else(|| payload.pointer("/gateway_status/channels"))
        .and_then(Value::as_array)
        .and_then(|channels| {
            channels.iter().find(|channel| {
                channel
                    .get("platform")
                    .or_else(|| channel.get("name"))
                    .or_else(|| channel.get("channel"))
                    .and_then(Value::as_str)
                    .is_some_and(|value| value.eq_ignore_ascii_case(platform))
            })
        })
}

fn task_gateway_connected(gateway_status: Option<&Value>) -> bool {
    gateway_status
        .and_then(|value| value.get("connected").and_then(Value::as_bool))
        .or_else(|| {
            gateway_status
                .and_then(|value| value.pointer("/bridge_provider/connected"))
                .and_then(Value::as_bool)
        })
        .or_else(|| {
            find_gateway_channel(gateway_status?, "weixin")
                .and_then(|value| value.get("connected"))
                .and_then(Value::as_bool)
        })
        .unwrap_or(false)
}

fn task_gateway_configured(gateway_status: Option<&Value>) -> bool {
    gateway_status
        .and_then(|value| value.get("configured").and_then(Value::as_bool))
        .or_else(|| {
            gateway_status
                .and_then(|value| value.pointer("/bridge_provider/configured"))
                .and_then(Value::as_bool)
        })
        .or_else(|| {
            find_gateway_channel(gateway_status?, "weixin")
                .and_then(|value| value.get("configured"))
                .and_then(Value::as_bool)
        })
        .unwrap_or(false)
}

fn fetch_task_gateway_status() -> Result<Value, String> {
    let config = NotificationGatewayConfig::from_env()
        .map_err(|error| format!("HarborGate status configuration error: {error}"))?;
    let endpoint = gateway_status_endpoint(&config.base_url);
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .map_err(|error| format!("failed to build HarborGate status client: {error}"))?;
    let response = client
        .get(endpoint)
        .header("Authorization", format!("Bearer {}", config.bearer_token))
        .header("X-Contract-Version", "2.0")
        .send()
        .map_err(|error| format!("HarborGate status request failed: {error}"))?;
    let status = response.status();
    let body = response
        .text()
        .map_err(|error| format!("failed to read HarborGate status response: {error}"))?;
    if !status.is_success() {
        return Err(format!(
            "HarborGate status request failed with HTTP {}",
            status.as_u16()
        ));
    }
    serde_json::from_str::<Value>(&body)
        .map_err(|error| format!("failed to parse HarborGate status response: {error}"))
}

fn gateway_status_endpoint(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.ends_with("/api/gateway/status") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/api/gateway/status")
    }
}

fn non_empty_task_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn redact_task_api_text(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let lower = trimmed.to_lowercase();
    if lower.contains("rtsp://")
        || lower.contains("bearer ")
        || lower.contains("token=")
        || lower.contains("password=")
        || lower.contains("passwd=")
        || lower.contains("api_key=")
        || lower.contains("access_token=")
        || lower.contains("-----begin ")
    {
        return "[redacted sensitive detail]".to_string();
    }
    redact_local_path_occurrences(trimmed)
}

fn redact_local_path_occurrences(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut index = 0;
    while index < value.len() {
        let remaining = &value[index..];
        if remaining.starts_with('/') && is_local_path_boundary(value, index) {
            let end = consume_local_path(value, index);
            if end > index + 1 {
                output.push_str("[redacted local path]");
                index = end;
                continue;
            }
        }
        if looks_like_windows_absolute_path(value, index) && is_local_path_boundary(value, index) {
            let end = consume_local_path(value, index);
            if end > index + 2 {
                output.push_str("[redacted local path]");
                index = end;
                continue;
            }
        }
        let ch = remaining.chars().next().unwrap_or_default();
        output.push(ch);
        index += ch.len_utf8();
    }
    output
}

fn is_local_path_boundary(value: &str, index: usize) -> bool {
    if index == 0 {
        return true;
    }
    value[..index]
        .chars()
        .last()
        .map(|ch| ch.is_whitespace() || matches!(ch, '"' | '\'' | '(' | '[' | '{'))
        .unwrap_or(true)
}

fn looks_like_windows_absolute_path(value: &str, index: usize) -> bool {
    let bytes = value.as_bytes();
    bytes
        .get(index)
        .copied()
        .map(|byte| byte.is_ascii_alphabetic())
        .unwrap_or(false)
        && bytes.get(index + 1) == Some(&b':')
        && matches!(bytes.get(index + 2), Some(b'\\' | b'/'))
}

fn consume_local_path(value: &str, start: usize) -> usize {
    let mut saw_separator = false;
    for (offset, ch) in value[start..].char_indices() {
        if matches!(ch, '/' | '\\') {
            saw_separator = true;
        }
        if ch.is_whitespace() || matches!(ch, '"' | '\'' | ')' | ']' | '}' | ',' | ';') {
            return if saw_separator { start + offset } else { start };
        }
    }
    if saw_separator {
        value.len()
    } else {
        start
    }
}

fn looks_like_vision_event_notify_request(normalized: &str) -> bool {
    matches_any(
        normalized,
        &[
            "通知最新事件",
            "通知最近事件",
            "发送最新事件",
            "转发最新事件",
            "把最新事件发出去",
            "发最新摄像头事件",
        ],
    )
}

fn looks_like_vision_event_summary_request(normalized: &str) -> bool {
    matches_any(
        normalized,
        &[
            "最近事件",
            "最新事件",
            "最新摄像头事件",
            "摄像头事件",
            "门口最近事件",
            "有什么事件",
        ],
    )
}

fn looks_like_system_readiness_request(normalized: &str) -> bool {
    matches_any(
        normalized,
        &[
            "状态",
            "诊断",
            "微信状态",
            "系统状态",
            "当前状态",
            "查看状态",
            "健康状态",
        ],
    )
}

fn maybe_render_general_message_reply(
    request: &TaskRequest,
    pending_loop: Option<&PendingTaskGeneralMessageLoop>,
    model_state: &crate::runtime::admin_console::AdminModelCenterState,
    plan: &mut GeneralMessagePlan,
    trace: &mut GeneralMessageControllerTrace,
) {
    if plan
        .reply_text
        .as_ref()
        .is_some_and(|value| !value.trim().is_empty())
    {
        return;
    }
    if !matches!(
        plan.kind,
        GeneralMessagePlanKind::Clarify
            | GeneralMessagePlanKind::ConversationAct
            | GeneralMessagePlanKind::Unsupported
    ) {
        return;
    }

    let default_text = match plan.kind {
        GeneralMessagePlanKind::Clarify => {
            general_message_default_clarification_prompt(request.intent.raw_text.as_str())
        }
        GeneralMessagePlanKind::ConversationAct => general_message_conversation_summary(
            request,
            pending_loop,
            plan.conversation_act.unwrap_or_else(|| {
                infer_general_message_conversation_act(
                    request.intent.raw_text.as_str(),
                    pending_loop,
                )
            }),
        ),
        GeneralMessagePlanKind::Unsupported => general_message_unsupported_summary(),
        _ => return,
    };
    let remaining_budget_ms = GENERAL_MESSAGE_TURN_BUDGET_MS
        .saturating_sub(trace.router_latency_ms.unwrap_or(0))
        .min(GENERAL_MESSAGE_RENDERER_BUDGET_MS);
    if remaining_budget_ms < 600 {
        return;
    }
    let started = Instant::now();
    let prompt =
        build_general_message_renderer_prompt(request, pending_loop, plan, default_text.as_str());
    let render_model_state = llm_model_state_without_cloud(model_state);
    let render_result = run_llm_text_with_state_and_options(
        &prompt,
        &render_model_state,
        &LlmTextOptions {
            purpose: Some("renderer".to_string()),
            system_prompt: Some(build_general_message_renderer_system_prompt()),
            temperature: Some(0.2),
            max_tokens: Some(GENERAL_MESSAGE_RENDERER_MAX_TOKENS),
            timeout: Some(Duration::from_millis(remaining_budget_ms)),
        },
    );
    trace.renderer_latency_ms = Some(started.elapsed().as_millis() as u64);
    if render_result.available {
        if let Some(parsed) = parse_general_message_plan(&render_result.text)
            .and_then(|parsed| parsed.reply_text)
            .filter(|value| !value.trim().is_empty())
        {
            plan.reply_text = Some(parsed);
            return;
        }

        let rendered = render_result.text.trim();
        if !rendered.is_empty()
            && parse_general_message_router_decision(rendered).is_none()
            && !rendered.contains('|')
            && !rendered.starts_with('{')
            && !rendered
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || matches!(ch, '_' | '-' | ' '))
        {
            plan.reply_text = Some(rendered.to_string());
        }
    }
}

fn record_workflow_compiler_shadow(
    trace: &mut GeneralMessageControllerTrace,
    candidate: Option<&WorkflowCandidate>,
    plan: &GeneralMessagePlan,
) {
    let current_plan = general_message_plan_decision_label(plan);
    let shadow = match candidate {
        Some(candidate) => GeneralMessageWorkflowCompilerShadowTrace {
            workflow_id: Some(candidate.workflow_id.clone()),
            node_id: Some(candidate.node_id.clone()),
            candidate_kind: Some(candidate.candidate_kind.clone()),
            confidence: Some(candidate.confidence),
            matched_current_plan: candidate.candidate_kind == current_plan,
            read_only: candidate.read_only,
            unsafe_action: candidate.has_unsafe_action(),
            reason: Some(candidate.reason.clone()),
        },
        None => GeneralMessageWorkflowCompilerShadowTrace {
            workflow_id: None,
            node_id: None,
            candidate_kind: None,
            confidence: None,
            matched_current_plan: false,
            read_only: true,
            unsafe_action: false,
            reason: Some("no workflow compiler candidate".to_string()),
        },
    };
    trace.workflow_compiler_shadow = Some(shadow);
}

fn attach_general_message_controller_trace(
    response: &mut TaskResponse,
    trace: &GeneralMessageControllerTrace,
    elapsed: Duration,
) {
    let previous = std::mem::replace(&mut response.result.data, Value::Null);
    let mut payload = if previous.is_object() {
        previous
    } else if previous.is_null() {
        json!({})
    } else {
        json!({ "payload": previous })
    };
    if let Some(map) = payload.as_object_mut() {
        let fallback_reason = trace.fallback_reason.as_deref().map(redact_task_api_text);
        let nsp_route = json!({
            "stage": trace.controller_stage,
            "schema_valid": trace.nsp_schema_valid,
            "local_only": trace.nsp_local_only,
            "decision": trace.nsp_decision,
            "confidence": trace.nsp_confidence,
            "canonical_phrase": trace
                .nsp_canonical_phrase
                .as_deref()
                .map(redact_task_api_text),
            "fallback_reason": fallback_reason.clone(),
        });
        let workflow_compiler_shadow = trace.workflow_compiler_shadow.as_ref().map(|shadow| {
            json!({
                "workflow_id": shadow.workflow_id,
                "node_id": shadow.node_id,
                "candidate_kind": shadow.candidate_kind,
                "confidence": shadow.confidence,
                "matched_current_plan": shadow.matched_current_plan,
                "read_only": shadow.read_only,
                "unsafe_action": shadow.unsafe_action,
                "reason": shadow.reason.as_deref().map(redact_task_api_text),
            })
        });
        map.insert(
            "general_message_controller".to_string(),
            json!({
                "controller_stage": trace.controller_stage,
                "router_llm": trace.router_llm,
                "router_latency_ms": trace.router_latency_ms,
                "renderer_latency_ms": trace.renderer_latency_ms,
                "candidate_count": trace.candidate_count,
                "fallback_reason": fallback_reason.clone(),
                "nsp_route": nsp_route.clone(),
                "workflow_compiler_shadow": workflow_compiler_shadow,
                "total_turn_latency_ms": elapsed.as_millis() as u64,
            }),
        );
        if let Some(reply_pack) = map.get_mut("reply_pack").and_then(Value::as_object_mut) {
            reply_pack.insert("nsp_route".to_string(), nsp_route);
        }
    }
    response.result.data = payload;
}

#[cfg(test)]
fn should_route_general_message_to_knowledge(request: &TaskRequest) -> bool {
    fallback_general_message_plan(
        request.intent.raw_text.as_str(),
        first_string(&[&request.args], &["/device_hint"]).as_deref(),
    )
    .is_some_and(|plan| matches!(plan.kind, GeneralMessagePlanKind::KnowledgeSearch))
}

fn extract_password_from_raw_text(raw_text: &str) -> Option<String> {
    for prefix in ["密码是", "密码", "password", "passwd"] {
        if let Some(rest) = raw_text.trim().strip_prefix(prefix) {
            let password = rest
                .trim_start_matches(|ch: char| ch.is_whitespace() || matches!(ch, ':' | '：'))
                .trim();
            if !password.is_empty() {
                return Some(password.to_string());
            }
        }
    }
    None
}

fn inject_password_arg_from_raw_text(request: &TaskRequest) -> TaskRequest {
    if string_at_paths(&request.args, &["/password"]).is_some() {
        return request.clone();
    }
    let Some(password) = extract_password_from_raw_text(request.intent.raw_text.as_str()) else {
        return request.clone();
    };
    let mut routed = request.clone();
    upsert_json_string(&mut routed.args, "/password", &password);
    routed
}

fn knowledge_search_query(request: &TaskRequest) -> Option<String> {
    first_string(
        &[&request.args],
        &[
            "/query",
            "/keyword",
            "/keywords/0",
            "/search/query",
            "/knowledge/query",
        ],
    )
    .or_else(|| infer_query_from_raw_text(&request.intent.raw_text))
}

fn infer_query_from_raw_text(raw_text: &str) -> Option<String> {
    let trimmed = raw_text
        .trim()
        .trim_matches(|ch: char| {
            ch.is_whitespace()
                || matches!(
                    ch,
                    '，' | '。' | ',' | '.' | '？' | '?' | '！' | '!' | '：' | ':'
                )
        })
        .to_string();
    if trimmed.is_empty() {
        return None;
    }

    let mut candidate = trimmed.clone();
    for pattern in [
        "请帮我",
        "帮我",
        "找到",
        "找一下",
        "找出",
        "找",
        "查一下",
        "查找",
        "查",
        "搜索",
        "搜一下",
        "搜",
        "检索",
        "和",
        "根据",
        "基于",
        "关于",
        "有关的",
        "相关的",
        "有关",
        "只看",
        "仅看",
        "只要",
        "结果",
        "排除",
        "不要",
        "不看",
        "出现",
        "回答",
        "总结",
        "概括",
        "说明",
        "是什么",
        "有哪些",
        "为什么",
        "怎么",
        "如何",
        "文件",
        "文档",
        "图片",
        "照片",
        "视频里",
        "录像里",
        "片段里",
        "视频",
        "录像",
        "回放",
        "片段",
        "相邻",
        "同一段",
        "第一个",
        "前后",
        "资料",
        "内容",
        "file",
        "files",
        "document",
        "documents",
        "image",
        "images",
        "photo",
        "photos",
        "picture",
        "pictures",
        "search for",
        "search",
        "find",
        "lookup",
        "look up",
    ] {
        candidate = candidate.replace(pattern, " ");
    }

    let candidate = candidate
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|ch| matches!(ch, '的' | '了'))
        .trim()
        .to_string();

    if candidate.is_empty() {
        Some(trimmed)
    } else {
        Some(candidate)
    }
}

fn json_string_at_paths_task(value: &Value, paths: &[&str]) -> Option<String> {
    paths.iter().find_map(|path| {
        value
            .pointer(path)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    })
}

fn normalize_optional_general_message_plan_field(value: Option<String>) -> Option<String> {
    value.map(|value| value.trim().to_string()).filter(|value| {
        !value.is_empty() && !matches!(value.to_ascii_lowercase().as_str(), "null" | "none" | "n/a")
    })
}

fn parse_json_object_from_text(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return Some(value);
    }

    if let Some(value) = trimmed
        .split("```")
        .map(str::trim)
        .filter(|candidate| !candidate.is_empty())
        .find_map(|candidate| {
            let candidate = candidate
                .strip_prefix("json")
                .map(str::trim)
                .unwrap_or(candidate);
            serde_json::from_str::<Value>(candidate).ok()
        })
    {
        return Some(value);
    }

    extract_first_balanced_json_object(trimmed)
        .and_then(|candidate| serde_json::from_str::<Value>(candidate).ok())
}

fn extract_first_balanced_json_object(text: &str) -> Option<&str> {
    let mut depth = 0usize;
    let mut start_index = None;
    let mut in_string = false;
    let mut escaped = false;

    for (index, ch) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }

        match ch {
            '\\' if in_string => {
                escaped = true;
            }
            '"' => {
                in_string = !in_string;
            }
            '{' if !in_string => {
                if depth == 0 {
                    start_index = Some(index);
                }
                depth += 1;
            }
            '}' if !in_string => {
                if depth == 0 {
                    continue;
                }
                depth -= 1;
                if depth == 0 {
                    let start = start_index?;
                    return Some(&text[start..=index]);
                }
            }
            _ => {}
        }
    }

    None
}

fn looks_like_rag_answer_request(normalized: &str) -> bool {
    let mentions_knowledge_context = matches_any(
        normalized,
        &[
            "根据资料",
            "根据文档",
            "根据知识库",
            "基于资料",
            "基于文档",
            "知识库",
            "资料",
            "文档",
            "记录",
            "已保存",
            "保存的内容",
            "本地内容",
        ],
    );
    let asks_for_answer = matches_any(
        normalized,
        &[
            "回答",
            "总结",
            "概括",
            "说明",
            "是什么",
            "有哪些",
            "为什么",
            "怎么",
            "如何",
        ],
    );
    mentions_knowledge_context && asks_for_answer
}

fn looks_like_video_search_request(normalized: &str) -> bool {
    let mentions_video = matches_any(
        normalized,
        &[
            "视频", "录像", "回放", "片段", "video", "videos", "clip", "clips",
        ],
    );
    if !mentions_video {
        return false;
    }
    matches_any(
        normalized,
        &[
            "找", "查", "搜", "检索", "搜索", "已有", "保存", "历史", "本地", "只看", "仅看",
            "筛选", "排除", "不要", "不看",
        ],
    )
}

fn knowledge_search_contextual_follow_up(normalized: &str) -> bool {
    matches_any(
        normalized,
        &[
            "只看视频",
            "只看视频结果",
            "仅看视频",
            "排除图片和文档",
            "不要图片和文档",
            "不看图片和文档",
            "只看第一个视频",
            "第一个视频",
            "同一段",
            "相邻片段",
            "前后片段",
            "前后一段",
            "再搜一下",
            "再查一下",
            "再找找",
            "搜已有内容",
        ],
    )
}

fn knowledge_follow_up_modalities(normalized: &str) -> Option<Vec<String>> {
    if knowledge_video_only_request_from_normalized(normalized) {
        return Some(vec!["video".to_string()]);
    }
    None
}

fn knowledge_video_only_request_from_normalized(normalized: &str) -> bool {
    matches_any(
        normalized,
        &[
            "只看视频",
            "只看视频结果",
            "仅看视频",
            "只要视频",
            "排除图片和文档",
            "排除文档和图片",
            "不要图片和文档",
            "不要文档和图片",
            "不看图片和文档",
            "不看文档和图片",
        ],
    )
}

fn recent_search_query_from_recap(session_recap: &[Value]) -> Option<String> {
    session_recap.iter().find_map(|entry| {
        string_at_paths(entry, &["/query"])
            .or_else(|| string_at_paths(entry, &["/search/query", "/data/query"]))
    })
}

fn knowledge_follow_up_focus_paths(normalized: &str, session_recap: &[Value]) -> Vec<String> {
    if !matches_any(
        normalized,
        &[
            "只看第一个视频",
            "第一个视频",
            "同一段",
            "相邻片段",
            "前后片段",
            "前后一段",
        ],
    ) {
        return Vec::new();
    }
    session_recap
        .iter()
        .find_map(|entry| string_at_paths(entry, &["/top_video_path"]))
        .map(|path| vec![path])
        .unwrap_or_default()
}

fn matches_any(normalized: &str, candidates: &[&str]) -> bool {
    candidates
        .iter()
        .map(|candidate| normalize_command_text(candidate))
        .any(|candidate| normalized.contains(&candidate))
}

fn knowledge_search_roots(request: &TaskRequest) -> Vec<String> {
    first_string_vec(
        &[&request.args],
        &["/roots", "/search/roots", "/knowledge/roots"],
    )
}

fn knowledge_focus_paths(request: &TaskRequest) -> Vec<String> {
    first_string_vec(
        &[&request.args],
        &[
            "/focus_paths",
            "/focusPaths",
            "/search/focus_paths",
            "/knowledge/focus_paths",
        ],
    )
}

fn knowledge_result_limit(request: &TaskRequest) -> usize {
    usize_at_paths(
        &request.args,
        &["/limit", "/search/limit", "/knowledge/limit"],
    )
    .unwrap_or(5)
    .clamp(1, 10)
}

fn knowledge_privacy_level(
    request: &TaskRequest,
    default_level: PrivacyLevel,
) -> Result<PrivacyLevel, String> {
    match string_at_paths(
        &request.args,
        &[
            "/privacy_level",
            "/privacyLevel",
            "/search/privacy_level",
            "/knowledge/privacy_level",
        ],
    ) {
        Some(value) => parse_privacy_level(&value)
            .ok_or_else(|| format!("不支持的 knowledge privacy_level：{value}")),
        None => Ok(default_level),
    }
}

fn knowledge_resource_profile(
    request: &TaskRequest,
    default_profile: RagResourceProfile,
) -> Result<RagResourceProfile, String> {
    match string_at_paths(
        &request.args,
        &[
            "/resource_profile",
            "/resourceProfile",
            "/search/resource_profile",
            "/knowledge/resource_profile",
        ],
    ) {
        Some(value) => parse_resource_profile(&value)
            .ok_or_else(|| format!("不支持的 RAG resource_profile：{value}")),
        None => Ok(default_profile),
    }
}

fn knowledge_require_embeddings(request: &TaskRequest) -> bool {
    bool_at_paths(
        &request.args,
        &[
            "/require_embeddings",
            "/require_embedding",
            "/search/require_embeddings",
            "/knowledge/require_embeddings",
        ],
    )
    .unwrap_or(false)
}

fn knowledge_latency_budget_ms(request: &TaskRequest) -> Option<u64> {
    u64_at_paths(
        &request.args,
        &[
            "/latency_budget_ms",
            "/latencyBudgetMs",
            "/search/latency_budget_ms",
            "/knowledge/latency_budget_ms",
        ],
    )
}

fn rag_answer_budget_ms() -> u64 {
    positive_env_u64(RAG_ANSWER_BUDGET_MS_ENV, RAG_ANSWER_BUDGET_MS)
}

fn rag_answer_max_tokens() -> u32 {
    positive_env_u32(RAG_ANSWER_MAX_TOKENS_ENV, RAG_ANSWER_MAX_TOKENS)
}

fn positive_env_u64(name: &str, default: u64) -> u64 {
    positive_u64_value(std::env::var(name).ok().as_deref(), default)
}

fn positive_env_u32(name: &str, default: u32) -> u32 {
    positive_u32_value(std::env::var(name).ok().as_deref(), default)
}

fn positive_u64_value(value: Option<&str>, default: u64) -> u64 {
    value
        .and_then(|raw| raw.trim().parse::<u64>().ok())
        .filter(|parsed| *parsed > 0)
        .unwrap_or(default)
}

fn positive_u32_value(value: Option<&str>, default: u32) -> u32 {
    value
        .and_then(|raw| raw.trim().parse::<u32>().ok())
        .filter(|parsed| *parsed > 0)
        .unwrap_or(default)
}

fn parse_privacy_level(value: &str) -> Option<PrivacyLevel> {
    match value.trim().to_lowercase().as_str() {
        "strict_local" | "strict-local" | "local" => Some(PrivacyLevel::StrictLocal),
        "allow_redacted_cloud" | "allow-redacted-cloud" | "redacted_cloud" => {
            Some(PrivacyLevel::AllowRedactedCloud)
        }
        "allow_cloud" | "allow-cloud" | "cloud" => Some(PrivacyLevel::AllowCloud),
        _ => None,
    }
}

fn parse_resource_profile(value: &str) -> Option<RagResourceProfile> {
    match value.trim().to_lowercase().as_str() {
        "cpu_only" | "cpu-only" | "cpu" => Some(RagResourceProfile::CpuOnly),
        "local_gpu" | "local-gpu" | "gpu" => Some(RagResourceProfile::LocalGpu),
        "sidecar_gpu" | "sidecar-gpu" | "sidecar" => Some(RagResourceProfile::SidecarGpu),
        "cloud_allowed" | "cloud-allowed" | "cloud" => Some(RagResourceProfile::CloudAllowed),
        _ => None,
    }
}

fn privacy_level_rank(level: PrivacyLevel) -> u8 {
    match level {
        PrivacyLevel::StrictLocal => 0,
        PrivacyLevel::AllowRedactedCloud => 1,
        PrivacyLevel::AllowCloud => 2,
    }
}

fn privacy_level_as_str(level: PrivacyLevel) -> &'static str {
    match level {
        PrivacyLevel::StrictLocal => "strict_local",
        PrivacyLevel::AllowRedactedCloud => "allow_redacted_cloud",
        PrivacyLevel::AllowCloud => "allow_cloud",
    }
}

fn knowledge_modalities(request: &TaskRequest) -> (bool, bool, bool) {
    let requested = first_string_vec(
        &[&request.args],
        &["/modalities", "/search/modalities", "/knowledge/modalities"],
    )
    .into_iter()
    .map(|item| item.to_lowercase())
    .collect::<Vec<_>>();
    if !requested.is_empty() {
        let include_documents = requested.iter().any(|item| {
            matches!(
                item.as_str(),
                "document" | "documents" | "doc" | "docs" | "text"
            )
        });
        let include_images = requested.iter().any(|item| {
            matches!(
                item.as_str(),
                "image" | "images" | "photo" | "photos" | "picture" | "pictures"
            )
        });
        let include_videos = requested.iter().any(|item| {
            matches!(
                item.as_str(),
                "video" | "videos" | "clip" | "clips" | "movie" | "movies"
            )
        });
        return (include_documents, include_images, include_videos);
    }

    let normalized_command = normalize_command_text(request.intent.raw_text.as_str());
    if knowledge_video_only_request_from_normalized(&normalized_command) {
        return (false, false, true);
    }

    let normalized = request.intent.raw_text.to_lowercase();
    let asks_for_documents = ["文档", "document", "documents"]
        .iter()
        .any(|token| normalized.contains(token));
    let asks_for_images = [
        "图片", "照片", "image", "images", "photo", "photos", "picture",
    ]
    .iter()
    .any(|token| normalized.contains(token));
    let asks_for_videos = [
        "视频", "录像", "回放", "片段", "video", "videos", "clip", "clips", "movie",
    ]
    .iter()
    .any(|token| normalized.contains(token));

    if asks_for_documents || asks_for_images || asks_for_videos {
        return (asks_for_documents, asks_for_images, asks_for_videos);
    }

    if request
        .intent
        .domain
        .trim()
        .eq_ignore_ascii_case(KNOWLEDGE_DOMAIN)
        && request
            .intent
            .action
            .trim()
            .eq_ignore_ascii_case(KNOWLEDGE_OP_SEARCH)
    {
        return (true, true, true);
    }

    (true, true, true)
}

fn room_aliases<'a>(name: &'a str, room: &'a str) -> Vec<&'static str> {
    let normalized = format!("{} {}", name.to_lowercase(), room.to_lowercase());
    let mut aliases = Vec::new();
    if normalized.contains("living room") {
        aliases.extend(["客厅", "大厅", "起居室"]);
    }
    if normalized.contains("front door") || normalized.contains("entry") {
        aliases.extend(["门口", "玄关", "入户"]);
    }
    if normalized.contains("garage") {
        aliases.extend(["车库"]);
    }
    aliases
}

fn string_at_paths(value: &Value, paths: &[&str]) -> Option<String> {
    paths.iter().find_map(|path| {
        value
            .pointer(path)
            .and_then(Value::as_str)
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
    })
}

fn array_len_at_paths(value: &Value, paths: &[&str]) -> Option<usize> {
    paths
        .iter()
        .find_map(|path| value.pointer(path).and_then(Value::as_array).map(Vec::len))
}

fn usize_at_paths(value: &Value, paths: &[&str]) -> Option<usize> {
    paths.iter().find_map(|path| {
        let item = value.pointer(path)?;
        if let Some(number) = item.as_u64() {
            return usize::try_from(number).ok();
        }
        item.as_str()?.trim().parse::<usize>().ok()
    })
}

fn u64_at_paths(value: &Value, paths: &[&str]) -> Option<u64> {
    paths.iter().find_map(|path| {
        let item = value.pointer(path)?;
        if let Some(number) = item.as_u64() {
            return Some(number);
        }
        item.as_str()?.trim().parse::<u64>().ok()
    })
}

fn bool_at_paths(value: &Value, paths: &[&str]) -> Option<bool> {
    paths.iter().find_map(|path| {
        let item = value.pointer(path)?;
        if let Some(flag) = item.as_bool() {
            return Some(flag);
        }
        match item.as_str()?.trim().to_lowercase().as_str() {
            "true" | "1" | "yes" => Some(true),
            "false" | "0" | "no" => Some(false),
            _ => None,
        }
    })
}

fn first_string(values: &[&Value], paths: &[&str]) -> Option<String> {
    values
        .iter()
        .find_map(|value| string_at_paths(value, paths))
}

fn first_u16(values: &[&Value], paths: &[&str]) -> Option<u16> {
    values.iter().find_map(|value| {
        paths.iter().find_map(|path| {
            let item = value.pointer(path)?;
            if let Some(number) = item.as_u64() {
                return u16::try_from(number).ok();
            }
            item.as_str()?.trim().parse::<u16>().ok()
        })
    })
}

fn first_string_vec(values: &[&Value], paths: &[&str]) -> Vec<String> {
    for value in values {
        for path in paths {
            if let Some(array) = value.pointer(path).and_then(Value::as_array) {
                let collected = array
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|item| item.trim().to_string())
                    .filter(|item| !item.is_empty())
                    .collect::<Vec<_>>();
                if !collected.is_empty() {
                    return collected;
                }
            }
        }
    }
    Vec::new()
}

fn first_non_empty<'a>(values: &[&'a str]) -> Option<&'a str> {
    values
        .iter()
        .copied()
        .find(|value| !value.trim().is_empty())
}

fn notification_platform_from_value(value: &str) -> Option<String> {
    match value.trim().to_lowercase().as_str() {
        "im_bridge" | "feishu" => Some("feishu".to_string()),
        "wecom" => Some("wecom".to_string()),
        "telegram" => Some("telegram".to_string()),
        "webhook" => Some("webhook".to_string()),
        "local_ui" => Some("local_ui".to_string()),
        _ => None,
    }
}

fn notification_delivery_mode_from_value(value: &str) -> NotificationDeliveryMode {
    match value.trim().to_lowercase().as_str() {
        "reply" => NotificationDeliveryMode::Reply,
        "update" => NotificationDeliveryMode::Update,
        _ => NotificationDeliveryMode::Send,
    }
}

fn notification_payload_format_from_value(value: &str) -> NotificationPayloadFormat {
    match value.trim().to_lowercase().as_str() {
        "markdown" => NotificationPayloadFormat::Markdown,
        "lark_card" | "card" => NotificationPayloadFormat::LarkCard,
        "json" => NotificationPayloadFormat::Json,
        _ => NotificationPayloadFormat::PlainText,
    }
}

fn task_artifact_to_notification_attachment(
    artifact: &TaskArtifact,
) -> Option<NotificationAttachment> {
    let kind = match artifact.kind.trim().to_lowercase().as_str() {
        "image" => NotificationAttachmentKind::Image,
        "video" => NotificationAttachmentKind::Video,
        "link" => NotificationAttachmentKind::Link,
        "json" | "card" | "text" => NotificationAttachmentKind::Json,
        _ => return None,
    };
    Some(NotificationAttachment {
        kind,
        label: artifact.label.clone(),
        mime_type: artifact.mime_type.clone(),
        path: artifact.path.clone(),
        url: artifact.url.clone(),
        metadata: artifact.metadata.clone(),
    })
}

#[cfg(test)]
fn resolve_notification_recipient(
    destination: &str,
    state: &AdminConsoleState,
    requester_user_id: &str,
) -> Option<NotificationRecipient> {
    let bindings = resolved_identity_binding_records(state);
    if destination.trim().is_empty() {
        return None;
    }

    if let Some(recipient) = recipient_from_literal_destination(destination, &bindings) {
        return Some(recipient);
    }

    if let Some(recipient) = recipient_from_binding_match(destination, &bindings) {
        return Some(recipient);
    }

    if !requester_user_id.trim().is_empty() {
        if let Some(binding) = bindings.iter().find(|binding| {
            binding
                .user_id
                .as_deref()
                .map(|value| value == requester_user_id)
                .unwrap_or(false)
        }) {
            if let Some(recipient) = recipient_from_binding(binding) {
                return Some(recipient);
            }
        }
    }

    let chat_bindings = bindings
        .iter()
        .filter_map(recipient_from_binding)
        .collect::<Vec<_>>();
    if chat_bindings.len() == 1 {
        return chat_bindings.into_iter().next();
    }

    None
}

fn proactive_notification_destination(
    _request: &TaskRequest,
    state: &AdminConsoleState,
) -> Option<NotificationDestination> {
    let target = default_notification_target(state)?;
    Some(NotificationDestination {
        kind: NotificationDestinationKind::Conversation,
        route_key: target.route_key.clone(),
        id: String::new(),
        platform: target.platform_hint.clone(),
        recipient: None,
    })
}

fn default_notification_target(state: &AdminConsoleState) -> Option<&NotificationTargetRecord> {
    state
        .notification_targets
        .iter()
        .find(|target| target.is_default)
        .or_else(|| state.notification_targets.first())
        .filter(|target| !target.route_key.trim().is_empty())
}

#[cfg(test)]
fn recipient_from_literal_destination(
    destination: &str,
    bindings: &[IdentityBindingRecord],
) -> Option<NotificationRecipient> {
    if destination.starts_with("oc_") {
        return Some(NotificationRecipient {
            recipient_id: destination.to_string(),
            recipient_type: NotificationRecipientIdType::ChatId,
        });
    }
    if destination.starts_with("ou_") {
        let _label = bindings
            .iter()
            .find(|binding| binding.open_id == destination)
            .map(|binding| binding.display_name.clone())
            .unwrap_or_else(|| destination.to_string());
        return Some(NotificationRecipient {
            recipient_id: destination.to_string(),
            recipient_type: NotificationRecipientIdType::OpenId,
        });
    }
    None
}

#[cfg(test)]
fn recipient_from_binding_match(
    destination: &str,
    bindings: &[IdentityBindingRecord],
) -> Option<NotificationRecipient> {
    let normalized = destination.trim();
    bindings
        .iter()
        .find(|binding| {
            binding.display_name == normalized
                || binding.open_id == normalized
                || binding
                    .chat_id
                    .as_deref()
                    .map(|value| value == normalized)
                    .unwrap_or(false)
                || binding
                    .user_id
                    .as_deref()
                    .map(|value| value == normalized)
                    .unwrap_or(false)
        })
        .and_then(recipient_from_binding)
}

#[cfg(test)]
fn recipient_from_binding(binding: &IdentityBindingRecord) -> Option<NotificationRecipient> {
    if let Some(chat_id) = binding
        .chat_id
        .as_ref()
        .filter(|value| !value.trim().is_empty())
    {
        return Some(NotificationRecipient {
            recipient_id: chat_id.clone(),
            recipient_type: NotificationRecipientIdType::ChatId,
        });
    }
    if !binding.open_id.trim().is_empty() {
        return Some(NotificationRecipient {
            recipient_id: binding.open_id.clone(),
            recipient_type: NotificationRecipientIdType::OpenId,
        });
    }
    None
}

fn notification_request_hash(request: &NotificationRequest) -> String {
    let identity = notification_request_identity(request);
    let bytes = serde_json::to_vec(&identity).unwrap_or_default();
    let digest = Sha256::digest(&bytes);
    digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
}

fn notification_request_identity(request: &NotificationRequest) -> Value {
    json!({
        "trace_id": request.trace_id.trim(),
        "source": {
            "service": request.source.service.trim(),
            "module": request.source.module.trim(),
            "event_type": request.source.event_type.trim(),
        },
        "destination": {
            "kind": serde_json::to_value(request.destination.kind).unwrap_or(Value::Null),
            "route_key": request.destination.route_key.trim(),
            "id": request.destination.id.trim(),
            "platform": request.destination.platform.trim(),
            "recipient": request.destination.recipient.as_ref().map(|recipient| json!({
                "recipient_id": recipient.recipient_id.trim(),
                "recipient_type": serde_json::to_value(recipient.recipient_type).unwrap_or(Value::Null),
            })).unwrap_or(Value::Null),
        },
        "content": {
            "title": request.content.title.trim(),
            "body": request.content.body.trim(),
            "payload_format": serde_json::to_value(request.content.payload_format).unwrap_or(Value::Null),
            "structured_payload": normalized_contract_value(&request.content.structured_payload),
            "attachments": request.content.attachments.iter().map(|attachment| {
                json!({
                    "kind": serde_json::to_value(attachment.kind).unwrap_or(Value::Null),
                    "label": attachment.label.trim(),
                    "mime_type": attachment.mime_type.trim(),
                    "path": attachment.path.clone().unwrap_or_default(),
                    "url": attachment.url.clone().unwrap_or_default(),
                    "metadata": normalized_contract_value(&attachment.metadata),
                })
            }).collect::<Vec<_>>(),
        },
        "delivery": {
            "mode": serde_json::to_value(request.delivery.mode).unwrap_or(Value::Null),
            "reply_to_message_id": request.delivery.reply_to_message_id.trim(),
            "update_message_id": request.delivery.update_message_id.trim(),
        },
        "metadata": {
            "correlation_id": request.metadata.correlation_id.trim(),
        },
    })
}

fn notification_delivery_outcome(
    notification_request: &NotificationRequest,
    result: Result<
        crate::connectors::notifications::NotificationDeliveryRecord,
        NotificationDeliveryError,
    >,
) -> NotificationDeliveryOutcome {
    let is_proactive = notification_request.destination.kind
        == NotificationDestinationKind::Recipient
        || (notification_request.destination.kind == NotificationDestinationKind::Conversation
            && !notification_request.destination.route_key.trim().is_empty()
            && notification_request.destination.recipient.is_none()
            && notification_request.destination.id.trim().is_empty()
            && !notification_request.destination.platform.trim().is_empty()
            && notification_request
                .delivery
                .reply_to_message_id
                .trim()
                .is_empty()
            && notification_request
                .delivery
                .update_message_id
                .trim()
                .is_empty());
    match result {
        Ok(record) if record.ok => NotificationDeliveryOutcome {
            event_type: "task.notification_delivered",
            severity: EventSeverity::Info,
            payload: serde_json::to_value(record).unwrap_or(Value::Null),
        },
        Ok(record) => NotificationDeliveryOutcome {
            event_type: if is_proactive {
                "task.proactive_delivery_failed"
            } else {
                "task.notification_failed"
            },
            severity: EventSeverity::Warning,
            payload: serde_json::to_value(record).unwrap_or(Value::Null),
        },
        Err(NotificationDeliveryError::RequestRejected {
            status_code,
            envelope,
        }) => NotificationDeliveryOutcome {
            event_type: if is_proactive {
                "task.proactive_delivery_failed"
            } else {
                "task.notification_rejected"
            },
            severity: if status_code >= 500 {
                EventSeverity::Error
            } else {
                EventSeverity::Warning
            },
            payload: json!({
                "status": "rejected",
                "http_status": status_code,
                "notification_id": notification_request.notification_id,
                "idempotency_key": notification_request.delivery.idempotency_key,
                "destination": notification_request.destination,
                "route_mode": if is_proactive { "proactive" } else { "source_bound" },
                "error": envelope.error,
                "trace_id": envelope.trace_id,
            }),
        },
        Err(error) => NotificationDeliveryOutcome {
            event_type: if is_proactive {
                "task.proactive_delivery_failed"
            } else {
                "task.notification_failed"
            },
            severity: EventSeverity::Error,
            payload: json!({
                "status": "failed",
                "notification_id": notification_request.notification_id,
                "idempotency_key": notification_request.delivery.idempotency_key,
                "destination": notification_request.destination,
                "route_mode": if is_proactive { "proactive" } else { "source_bound" },
                "error": error.to_string(),
            }),
        },
    }
}

fn env_flag_enabled(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn current_timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

fn current_timestamp_millis() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .to_string()
}

fn new_event_id() -> String {
    Uuid::new_v4().as_simple().to_string()
}

fn new_approval_id() -> String {
    Uuid::new_v4().as_simple().to_string()
}

fn ensure_resume_token() -> String {
    Uuid::new_v4().as_simple().to_string()
}

fn new_task_id() -> String {
    Uuid::new_v4().as_simple().to_string()
}

fn new_turn_id() -> String {
    format!("turn_{}", Uuid::new_v4().as_simple())
}

fn new_audit_ref() -> String {
    Uuid::new_v4().as_simple().to_string()[..12].to_string()
}

fn new_media_asset_id() -> String {
    format!("asset-{}", Uuid::new_v4().as_simple())
}

fn new_media_session_id() -> String {
    format!("media-session-{}", Uuid::new_v4().as_simple())
}

fn new_share_link_id() -> String {
    format!("share-link-{}", Uuid::new_v4().as_simple())
}

fn stable_prefixed_id(prefix: &str, payload: &str, length: usize) -> String {
    let digest = Sha256::digest(payload.as_bytes());
    let hex = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("{prefix}{}", &hex[..length.min(hex.len())])
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    use base64::Engine as _;
    use serde_json::{json, Value};
    use tiny_http::{Header, Method, Response, Server};

    use super::{
        artifact_kind_from_name, build_artifact_records, build_general_message_readiness_summary,
        build_knowledge_search_artifacts, build_redacted_vision_event_summary, conversation_key,
        delivery_hints_from_task_response, effective_autonomy_level,
        effective_autonomy_level_for_task_run, effective_requires_approval,
        ensure_safe_capture_root, env_flag_enabled, fallback_general_message_plan,
        format_pending_candidates, general_message_requests_capability_summary,
        infer_home_assistant_natural_action, infer_query_from_raw_text, knowledge_modalities,
        normalize_command_text, notification_delivery_outcome, parse_general_message_plan,
        pending_candidates_from_results, protocol_string, resolve_home_assistant_action_entity,
        resolve_notification_recipient, room_aliases, should_route_general_message_to_knowledge,
        GeneralMessageConversationAct, GeneralMessagePlanKind, HomeAssistantEntityResolution,
        HomeAssistantNaturalAction, HomeAssistantNaturalActionRequest, PendingTaskCandidate,
        TaskApiService, TaskArtifact, TaskIntent, TaskMessage, TaskRequest, TaskRequestAcceptance,
        TaskResponse, TaskResultEnvelope, TaskSource, TaskStatus, TaskTurnActor, TaskTurnBlock,
        TaskTurnContinuation, TaskTurnConversation, TaskTurnEnvelope, TaskTurnInput,
        TaskTurnTransport, ALLOW_NON_HARBOROS_CAPTURE_ROOT_ENV, KNOWLEDGE_DOMAIN,
        KNOWLEDGE_OP_SEARCH,
    };
    use crate::connectors::home_assistant::HomeAssistantEntity;
    use crate::connectors::notifications::{
        NotificationContent, NotificationDelivery, NotificationDeliveryError,
        NotificationDeliveryMode, NotificationDestination, NotificationDestinationKind,
        NotificationMetadata, NotificationPayloadFormat, NotificationRecipientIdType,
        NotificationRequest, NotificationSource, SharedHttpErrorDetail, SharedHttpErrorEnvelope,
    };
    use crate::connectors::storage::StorageTarget;
    use crate::control_plane::approvals::ApprovalStatus;
    use crate::control_plane::auth::{AuthSource, IdentityBinding};
    use crate::control_plane::media::{MediaAssetKind, StorageTargetKind};
    use crate::control_plane::models::{
        ModelEndpoint, ModelEndpointKind, ModelEndpointStatus, ModelKind, PrivacyLevel,
    };
    use crate::control_plane::tasks::{
        ArtifactKind, ConversationSession, ExecutionRoute, TaskRunStatus, TaskStepRunStatus,
    };
    use crate::orchestrator::contracts::RiskLevel;
    use crate::runtime::admin_console::{
        AdminConsoleState, AdminConsoleStore, HomeAssistantConfigUpdate, IdentityBindingRecord,
        KnowledgeSettings, KnowledgeSourceRoot, NotificationTargetRecord, RagResourceProfile,
        RemoteViewConfig,
    };
    use crate::runtime::hub::HubScanResultItem;
    use crate::runtime::knowledge::{
        KnowledgeSearchHit, KnowledgeSearchReplyPack, KnowledgeSearchResponse,
    };
    use crate::runtime::knowledge_index::{KnowledgeIndexConfig, KnowledgeIndexService};
    use crate::runtime::media::{SnapshotCaptureResult, SnapshotFormat};
    use crate::runtime::registry::{
        CameraCapabilities, CameraDevice, CameraStreamRef, DeviceRegistryStore, DeviceStatus,
        ResolvedCameraTarget, StreamTransport,
    };
    use crate::runtime::task_session::{
        PendingTaskClipConfirmation, PendingTaskConnect, RecentClipPlaybackState,
        TaskConversationState, TaskConversationStore,
    };
    use crate::runtime::vision_event::{
        LocalVisionEvent, LocalVisionEventArtifact, LocalVisionEventVlmSummary, SnapshotArtifact,
        StoredLocalVisionEvent,
    };

    static RETRIEVAL_GATE_TEST_LOCK: Mutex<()> = Mutex::new(());
    static HARBOROS_TASK_API_TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn rag_answer_env_overrides_accept_only_positive_numbers() {
        assert_eq!(super::positive_u64_value(Some("9000"), 6_000), 9_000);
        assert_eq!(super::positive_u64_value(Some("0"), 6_000), 6_000);
        assert_eq!(super::positive_u64_value(Some("nope"), 6_000), 6_000);
        assert_eq!(super::positive_u64_value(None, 6_000), 6_000);

        assert_eq!(super::positive_u32_value(Some("64"), 256), 64);
        assert_eq!(super::positive_u32_value(Some("0"), 256), 256);
        assert_eq!(super::positive_u32_value(Some("4294967296"), 256), 256);
        assert_eq!(super::positive_u32_value(None, 256), 256);
    }

    #[test]
    fn knowledge_search_image_artifacts_include_content_index_audit_metadata() {
        let response = KnowledgeSearchResponse {
            query: "春天".to_string(),
            roots: vec!["/mnt/photos".to_string()],
            total_matches: 1,
            documents: Vec::new(),
            images: vec![KnowledgeSearchHit {
                modality: "image".to_string(),
                path: "/mnt/photos/neutral-name.jpg".to_string(),
                title: "neutral-name.jpg".to_string(),
                score: 820,
                lexical_score: Some(0.82),
                embedding_score: None,
                hybrid_score: Some(0.82),
                rerank_score: None,
                chunk_id: Some("chunk-0001".to_string()),
                line_start: Some(1),
                line_end: Some(1),
                snippet: Some("春天的公园里有绿色草地和盛开的花".to_string()),
                matched_terms: vec!["春天".to_string()],
                provenance: Some("vlm".to_string()),
                source_path: None,
                content_source_kinds: vec!["vlm".to_string()],
                content_indexed: true,
                filename_match_used: false,
                content_match_used: true,
            }],
            videos: Vec::new(),
            reply_pack: KnowledgeSearchReplyPack::default(),
            supported_modalities: vec!["vlm".to_string()],
            pending_modalities: Vec::new(),
            status: "ok".to_string(),
            degraded: false,
            degraded_reason: None,
            blockers: Vec::new(),
            warnings: Vec::new(),
            source_scope: Vec::new(),
            privacy_level: "strict_local".to_string(),
            resource_profile: "cpu_only".to_string(),
            empty_reason: None,
            empty_guidance: None,
        };

        let artifacts = build_knowledge_search_artifacts(&response);

        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].kind, "image");
        assert_eq!(artifacts[0].metadata["content_indexed"], true);
        assert_eq!(artifacts[0].metadata["filename_match_used"], false);
        assert_eq!(artifacts[0].metadata["content_source_kinds"][0], "vlm");
        assert_eq!(artifacts[0].metadata["citation"]["content_indexed"], true);
        assert_eq!(
            artifacts[0].metadata["citation"]["filename_match_used"],
            false
        );
        assert_eq!(
            artifacts[0].metadata["citation"]["content_source_kinds"][0],
            "vlm"
        );
        assert_eq!(artifacts[0].metadata["content_match_used"], true);
        assert_eq!(
            artifacts[0].metadata["citation"]["content_match_used"],
            true
        );
    }

    #[test]
    fn knowledge_search_video_artifacts_are_native_video_results() {
        let response = KnowledgeSearchResponse {
            query: "快递".to_string(),
            roots: vec!["/mnt/videos".to_string()],
            total_matches: 1,
            documents: Vec::new(),
            images: Vec::new(),
            videos: vec![KnowledgeSearchHit {
                modality: "video".to_string(),
                path: "/mnt/videos/porch-clip.mp4".to_string(),
                title: "porch-clip.mp4".to_string(),
                score: 760,
                lexical_score: Some(0.76),
                embedding_score: None,
                hybrid_score: Some(0.76),
                rerank_score: None,
                chunk_id: Some("chunk-0001".to_string()),
                line_start: Some(1),
                line_end: Some(1),
                snippet: Some("keyframe 30%: 门口有一个快递箱".to_string()),
                matched_terms: vec!["快递".to_string()],
                provenance: Some("vlm_keyframe".to_string()),
                source_path: Some("/tmp/keyframes/frame-02.jpg".to_string()),
                content_source_kinds: vec!["vlm_keyframe".to_string()],
                content_indexed: true,
                filename_match_used: false,
                content_match_used: true,
            }],
            reply_pack: KnowledgeSearchReplyPack::default(),
            supported_modalities: vec!["video".to_string(), "vlm_keyframe".to_string()],
            pending_modalities: Vec::new(),
            status: "ok".to_string(),
            degraded: false,
            degraded_reason: None,
            blockers: Vec::new(),
            warnings: Vec::new(),
            source_scope: Vec::new(),
            privacy_level: "strict_local".to_string(),
            resource_profile: "cpu_only".to_string(),
            empty_reason: None,
            empty_guidance: None,
        };

        let artifacts = build_knowledge_search_artifacts(&response);

        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].kind, "video");
        assert_eq!(artifacts[0].mime_type, "video/mp4");
        assert_eq!(
            artifacts[0].path.as_deref(),
            Some("/mnt/videos/porch-clip.mp4")
        );
        assert_eq!(artifacts[0].metadata["modality"], "video");
        assert_eq!(artifacts[0].metadata["content_indexed"], true);
        assert_eq!(artifacts[0].metadata["filename_match_used"], false);
        assert_eq!(artifacts[0].metadata["content_match_used"], true);
        assert_eq!(artifacts[0].metadata["match_source"], "vlm_keyframe");
        assert_eq!(
            artifacts[0].metadata["segment_locator"],
            json!({
                "kind": "keyframe_percent",
                "percent": 30,
                "source": "vlm_keyframe"
            })
        );
        assert_eq!(artifacts[0].metadata["video_level_result"], false);
        assert_eq!(
            artifacts[0].metadata["citation"]["source_path"],
            "/tmp/keyframes/frame-02.jpg"
        );
    }

    #[test]
    fn knowledge_search_video_artifacts_surface_sidecar_time_range_when_present() {
        let sidecar_path = unique_path("harborbeacon-video-sidecar");
        fs::write(
            &sidecar_path,
            r#"{"summary":"门口有人拿起快递箱","start_time":"00:00:05","end_time":"00:00:12"}"#,
        )
        .expect("write sidecar");
        let response = KnowledgeSearchResponse {
            query: "快递".to_string(),
            roots: vec!["/mnt/videos".to_string()],
            total_matches: 1,
            documents: Vec::new(),
            images: Vec::new(),
            videos: vec![KnowledgeSearchHit {
                modality: "video".to_string(),
                path: "/mnt/videos/porch-clip.mp4".to_string(),
                title: "porch-clip.mp4".to_string(),
                score: 760,
                lexical_score: Some(0.76),
                embedding_score: None,
                hybrid_score: Some(0.76),
                rerank_score: None,
                chunk_id: Some("chunk-0001".to_string()),
                line_start: Some(1),
                line_end: Some(1),
                snippet: Some("门口有人拿起快递箱".to_string()),
                matched_terms: vec!["快递".to_string()],
                provenance: Some("video_sidecar".to_string()),
                source_path: Some(sidecar_path.to_string_lossy().into_owned()),
                content_source_kinds: vec!["video_sidecar".to_string()],
                content_indexed: true,
                filename_match_used: false,
                content_match_used: true,
            }],
            reply_pack: KnowledgeSearchReplyPack::default(),
            supported_modalities: vec!["video".to_string()],
            pending_modalities: Vec::new(),
            status: "ok".to_string(),
            degraded: false,
            degraded_reason: None,
            blockers: Vec::new(),
            warnings: Vec::new(),
            source_scope: Vec::new(),
            privacy_level: "strict_local".to_string(),
            resource_profile: "cpu_only".to_string(),
            empty_reason: None,
            empty_guidance: None,
        };

        let artifacts = build_knowledge_search_artifacts(&response);

        assert_eq!(
            artifacts[0].metadata["segment_locator"],
            json!({
                "kind": "time_range",
                "start": "00:00:05",
                "end": "00:00:12",
                "timestamp": null,
                "source": "video_sidecar"
            })
        );
        assert_eq!(artifacts[0].metadata["video_level_result"], false);

        let _ = fs::remove_file(sidecar_path);
    }

    fn unique_path(prefix: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{unique}.json"))
    }

    fn unique_dir(prefix: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{unique}"))
    }

    fn restore_env_var(name: &str, previous: Option<std::ffi::OsString>) {
        if let Some(value) = previous {
            std::env::set_var(name, value);
        } else {
            std::env::remove_var(name);
        }
    }

    fn configure_knowledge_source(store: &AdminConsoleStore, root: &Path, index_root: &Path) {
        configure_knowledge_source_with_policy(
            store,
            root,
            index_root,
            PrivacyLevel::StrictLocal,
            RagResourceProfile::CpuOnly,
        );
    }

    fn configure_knowledge_source_with_policy(
        store: &AdminConsoleStore,
        root: &Path,
        index_root: &Path,
        privacy_level: PrivacyLevel,
        default_resource_profile: RagResourceProfile,
    ) {
        fs::create_dir_all(index_root).expect("create knowledge index root");
        store
            .save_knowledge_settings(KnowledgeSettings {
                source_roots: vec![KnowledgeSourceRoot {
                    root_id: "test-root".to_string(),
                    label: "Test knowledge root".to_string(),
                    path: root.to_string_lossy().into_owned(),
                    enabled: true,
                    include: Vec::new(),
                    exclude: Vec::new(),
                    last_indexed_at: None,
                }],
                index_root: index_root.to_string_lossy().into_owned(),
                privacy_level,
                default_resource_profile,
                retrieval: Default::default(),
            })
            .expect("save knowledge settings");
        KnowledgeIndexService::from_config(
            KnowledgeIndexConfig::new(index_root.to_path_buf()).expect("knowledge index config"),
        )
        .expect("knowledge index service")
        .load_or_refresh(root)
        .expect("build knowledge index");
    }

    fn reset_harbor_task_api_env() {
        for name in [
            "HARBOR_FORCE_MIDDLEWARE_ERROR",
            "HARBOR_URL",
            "HARBOR_MIDDLEWARE_URL",
            "HARBOR_API_KEY",
            "HARBOR_MIDDLEWARE_API_KEY",
            "HARBOR_USER",
            "HARBOR_PASSWORD",
            "HARBOR_MIDCLI_URL",
            "HARBOR_MIDCLI_USER",
            "HARBOR_MIDCLI_PASSWORD",
            "HARBOR_DISABLE_MIDDLEWARE",
            "HARBOR_DISABLE_MIDCLI",
            "HARBOR_MIDCLI_BIN",
            "HARBOR_MIDCLI_PASSTHROUGH",
        ] {
            std::env::remove_var(name);
        }
    }

    fn build_task_api_service(
        prefix: &str,
    ) -> (
        TaskApiService,
        TaskConversationStore,
        std::path::PathBuf,
        std::path::PathBuf,
        std::path::PathBuf,
    ) {
        let admin_path = unique_path(&format!("{prefix}-admin"));
        let registry_path = unique_path(&format!("{prefix}-registry"));
        let conversation_path = unique_path(&format!("{prefix}-conversation"));
        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        let conversation_store = TaskConversationStore::new(conversation_path.clone());
        let service = TaskApiService::new(admin_store, conversation_store.clone());
        (
            service,
            conversation_store,
            admin_path,
            registry_path,
            conversation_path,
        )
    }

    fn configure_mock_general_message_llm(service: &TaskApiService, mock_text: &str) {
        service
            .clone()
            .admin_store
            .save_model_endpoint(ModelEndpoint {
                model_endpoint_id: "llm-local-openai-compatible".to_string(),
                workspace_id: Some("home-1".to_string()),
                provider_account_id: None,
                model_kind: ModelKind::Llm,
                endpoint_kind: ModelEndpointKind::Local,
                provider_key: "openai_compatible".to_string(),
                model_name: "harbor-local-chat".to_string(),
                capability_tags: vec!["chat".to_string(), "local_first".to_string()],
                cost_policy: json!({}),
                status: ModelEndpointStatus::Active,
                metadata: json!({
                    "mock_text": mock_text,
                }),
            })
            .expect("save mock llm endpoint");
    }

    fn configure_mock_cloud_llm(service: &TaskApiService, mock_text: &str) {
        service
            .clone()
            .admin_store
            .save_model_endpoint(ModelEndpoint {
                model_endpoint_id: "llm-cloud-openai-compatible".to_string(),
                workspace_id: Some("home-1".to_string()),
                provider_account_id: None,
                model_kind: ModelKind::Llm,
                endpoint_kind: ModelEndpointKind::Cloud,
                provider_key: "openai_compatible".to_string(),
                model_name: "harbor-cloud-chat".to_string(),
                capability_tags: vec!["chat".to_string(), "cloud".to_string()],
                cost_policy: json!({}),
                status: ModelEndpointStatus::Active,
                metadata: json!({
                    "mock_text": mock_text,
                }),
            })
            .expect("save mock cloud llm endpoint");
    }

    fn configure_mock_embedding(service: &TaskApiService) {
        service
            .clone()
            .admin_store
            .save_model_endpoint(ModelEndpoint {
                model_endpoint_id: "embedder-local-mock".to_string(),
                workspace_id: Some("home-1".to_string()),
                provider_account_id: None,
                model_kind: ModelKind::Embedder,
                endpoint_kind: ModelEndpointKind::Local,
                provider_key: "mock".to_string(),
                model_name: "harbor-mock-embedder".to_string(),
                capability_tags: vec!["embedding".to_string(), "local_first".to_string()],
                cost_policy: json!({}),
                status: ModelEndpointStatus::Active,
                metadata: json!({
                    "mock_embedding_dimensions": 4,
                }),
            })
            .expect("save mock embedding endpoint");
    }

    fn cleanup_task_api_service(
        admin_path: std::path::PathBuf,
        registry_path: std::path::PathBuf,
        conversation_path: std::path::PathBuf,
    ) {
        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    fn configure_mock_home_assistant(service: &TaskApiService, base_url: &str) {
        service
            .clone()
            .admin_store
            .save_home_assistant_config(HomeAssistantConfigUpdate {
                enabled: true,
                base_url: base_url.to_string(),
                access_token: Some("ha-test-token".to_string()),
                exposed_domains: vec!["light".to_string(), "input_boolean".to_string()],
            })
            .expect("save HA config");
    }

    fn start_mock_home_assistant_server(
        states_sequence: Vec<Value>,
        expected_requests: usize,
    ) -> (String, Arc<Mutex<Vec<Value>>>, thread::JoinHandle<()>) {
        let server = Server::http("127.0.0.1:0").expect("HA mock server");
        let base_url = format!("http://{}", server.server_addr());
        let posts = Arc::new(Mutex::new(Vec::new()));
        let post_records = posts.clone();
        let json_header =
            Header::from_bytes(b"Content-Type", b"application/json").expect("json header");
        let server_thread = thread::spawn(move || {
            let mut state_index = 0usize;
            for _ in 0..expected_requests {
                let mut request = server.recv().expect("HA request");
                let method = request.method().clone();
                let url = request.url().to_string();
                match (method, url.as_str()) {
                    (Method::Get, "/api/states") => {
                        let index = state_index.min(states_sequence.len().saturating_sub(1));
                        state_index = state_index.saturating_add(1);
                        let body = states_sequence
                            .get(index)
                            .cloned()
                            .unwrap_or_else(|| json!([]))
                            .to_string();
                        request
                            .respond(Response::from_string(body).with_header(json_header.clone()))
                            .expect("states response");
                    }
                    (Method::Post, path) if path.starts_with("/api/services/") => {
                        let mut body = String::new();
                        request
                            .as_reader()
                            .read_to_string(&mut body)
                            .expect("read service body");
                        let body_json =
                            serde_json::from_str::<Value>(&body).unwrap_or_else(|_| json!({}));
                        post_records
                            .lock()
                            .expect("post records")
                            .push(json!({ "path": path, "body": body_json }));
                        request
                            .respond(Response::from_string("[]").with_header(json_header.clone()))
                            .expect("service response");
                    }
                    _ => {
                        request
                            .respond(Response::from_string("not found").with_status_code(404))
                            .expect("not found response");
                    }
                }
            }
        });
        (base_url, posts, server_thread)
    }

    fn home_assistant_light_entities() -> Value {
        json!([
            home_assistant_entity_state(
                "light.kitchen",
                "off",
                "厨房灯",
                Some("kitchen"),
                Some("light")
            ),
            home_assistant_entity_state(
                "light.porch",
                "on",
                "门口灯",
                Some("porch"),
                Some("light")
            )
        ])
    }

    fn home_assistant_entity_state(
        entity_id: &str,
        state: &str,
        friendly_name: &str,
        area_id: Option<&str>,
        device_class: Option<&str>,
    ) -> Value {
        json!({
            "entity_id": entity_id,
            "state": state,
            "attributes": {
                "friendly_name": friendly_name,
                "area_id": area_id,
                "device_class": device_class,
                "access_token": "must-not-leak",
            },
            "last_changed": "2026-06-03T01:00:00Z",
            "last_updated": "2026-06-03T01:00:00Z"
        })
    }

    fn general_message_test_request(prefix: &str, raw_text: &str, args: Value) -> TaskRequest {
        TaskRequest {
            task_id: format!("task-{prefix}"),
            trace_id: format!("trace-{prefix}"),
            step_id: format!("step-{prefix}"),
            source: TaskSource {
                channel: "weixin".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: format!("chat-{prefix}"),
                user_id: "user-1".to_string(),
                session_id: format!("session-{prefix}"),
                route_key: format!("gw_route_{prefix}"),
            },
            intent: TaskIntent {
                domain: "general".to_string(),
                action: "message".to_string(),
                raw_text: raw_text.to_string(),
            },
            entity_refs: Value::Null,
            args,
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: format!("om_{prefix}"),
                chat_type: "p2p".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        }
    }

    fn general_message_followup_request(
        prefix: &str,
        raw_text: &str,
        first_request: &TaskRequest,
        resume_token: &str,
    ) -> TaskRequest {
        let mut request = general_message_test_request(
            prefix,
            raw_text,
            json!({ "continuation_token": resume_token }),
        );
        request.source.session_id = first_request.source.session_id.clone();
        request.source.conversation_id = first_request.source.conversation_id.clone();
        request
    }

    fn general_message_turn_envelope(
        prefix: &str,
        raw_text: &str,
        handle: Option<String>,
        continuation: Option<TaskTurnContinuation>,
    ) -> TaskTurnEnvelope {
        TaskTurnEnvelope {
            turn: TaskTurnBlock {
                turn_id: format!("turn-{prefix}"),
                trace_id: format!("trace-{prefix}"),
                occurred_at: "2026-04-26T00:00:00Z".to_string(),
                retry_of: None,
            },
            actor: TaskTurnActor {
                user_id: "user-1".to_string(),
                workspace_id: "home-1".to_string(),
                account_id: None,
            },
            conversation: TaskTurnConversation {
                handle,
                channel: "weixin".to_string(),
                surface: "harborgate".to_string(),
                thread_id: "thread-general-turn".to_string(),
                chat_type: "p2p".to_string(),
            },
            transport: TaskTurnTransport {
                route_key: "gw_route_general_turn".to_string(),
                message_id: format!("om_{prefix}"),
                capabilities: json!({
                    "text": true,
                    "image": true,
                    "file": true,
                    "video": true,
                }),
                metadata: Value::Null,
            },
            input: TaskTurnInput {
                text: raw_text.to_string(),
                parts: Vec::new(),
            },
            continuation,
            autonomy: Default::default(),
        }
    }

    fn seed_clip_confirmation_turn_state(
        conversation_store: &TaskConversationStore,
        handle: &str,
        token: &str,
    ) {
        let session = ConversationSession {
            session_id: handle.to_string(),
            workspace_id: "home-1".to_string(),
            channel: "weixin".to_string(),
            surface: "harborgate".to_string(),
            conversation_id: handle.to_string(),
            user_id: "user-1".to_string(),
            route_key: "gw_route_clip_frame".to_string(),
            last_message_id: "om_clip_frame".to_string(),
            chat_type: "p2p".to_string(),
            state: Value::Null,
            resume_token: None,
            expires_at: None,
        };
        let mut conversation = TaskConversationState {
            key: handle.to_string(),
            ..Default::default()
        };
        conversation.set_clip_pending_confirmation(Some(PendingTaskClipConfirmation {
            resume_token: token.to_string(),
            clip_media_asset_id: format!("asset-{token}"),
            clip_path: format!("captures/clips/{token}.mp4"),
            clip_mime_type: "video/mp4".to_string(),
            cover_path: format!("captures/keyframes/{token}.jpg"),
            display_name: "Tapo 231".to_string(),
        }));
        conversation.set_recent_clip_playback(Some(RecentClipPlaybackState {
            clip_media_asset_id: format!("asset-{token}"),
            clip_path: format!("captures/clips/{token}.mp4"),
            clip_mime_type: "video/mp4".to_string(),
            cover_path: format!("captures/keyframes/{token}.jpg"),
            display_name: "Tapo 231".to_string(),
            captured_at_epoch_ms: super::current_epoch_ms(),
        }));
        conversation_store
            .save_for_session(&session, &conversation)
            .expect("save clip confirmation state");
    }

    #[test]
    fn conversation_key_prefers_conversation_id() {
        let request = TaskRequest {
            task_id: "task-1".to_string(),
            trace_id: "trace-1".to_string(),
            step_id: "step-1".to_string(),
            source: TaskSource {
                channel: "feishu".to_string(),
                surface: "harborbeacon".to_string(),
                conversation_id: "chat-1".to_string(),
                user_id: "user-1".to_string(),
                session_id: "session-1".to_string(),
                route_key: String::new(),
            },
            intent: TaskIntent::default(),
            entity_refs: Value::Null,
            args: Value::Null,
            autonomy: Default::default(),
            message: None,
        };

        assert_eq!(conversation_key(&request), Some("chat-1".to_string()));
    }

    #[test]
    fn handle_task_persists_route_key_and_message_summary() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-route-message");
        let service = TaskApiService::new(
            AdminConsoleStore::new(
                admin_path.clone(),
                DeviceRegistryStore::new(registry_path.clone()),
            ),
            TaskConversationStore::new(conversation_path.clone()),
        );
        let request = TaskRequest {
            task_id: "task-route-message".to_string(),
            trace_id: "trace-route-message".to_string(),
            step_id: "step-route-message".to_string(),
            source: TaskSource {
                channel: "im_bridge".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-route-message".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-route-message".to_string(),
                route_key: "gw_route_01".to_string(),
            },
            intent: TaskIntent {
                domain: "system".to_string(),
                action: "ping".to_string(),
                raw_text: "ping".to_string(),
            },
            entity_refs: Value::Null,
            args: Value::Null,
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: "om_01".to_string(),
                chat_type: "group".to_string(),
                mentions: Vec::new(),
                attachments: vec![super::TaskMessageAttachment {
                    attachment_id: "att_01".to_string(),
                    attachment_type: "file".to_string(),
                    name: "front-door.jpg".to_string(),
                    mime_type: "image/jpeg".to_string(),
                    size_bytes: Some(2048),
                    download: Some(super::TaskMessageAttachmentDownload {
                        mode: "proxy".to_string(),
                        url: "https://gateway.local/files/att_01".to_string(),
                        method: "GET".to_string(),
                        headers: json!({
                            "Authorization": "Bearer opaque-download-token"
                        }),
                        auth: Some(super::TaskMessageAttachmentDownloadAuth {
                            kind: "bearer".to_string(),
                        }),
                        expires_at: "2026-04-18T12:00:00Z".to_string(),
                        max_size_bytes: Some(4096),
                    }),
                    metadata: json!({
                        "transport": "opaque",
                        "provider_file_key": "file_key_01"
                    }),
                }],
            }),
        };

        let response = service.handle_task(request);
        assert_eq!(response.status, TaskStatus::Failed);

        let session = service
            .conversation_store()
            .load_session("sess-route-message")
            .expect("load session")
            .expect("session");
        assert_eq!(session.route_key, "gw_route_01");
        assert_eq!(session.last_message_id, "om_01");
        assert_eq!(session.chat_type, "group");

        let task_run = service
            .conversation_store()
            .load_task_run("task-route-message")
            .expect("load task run")
            .expect("task run");
        assert_eq!(task_run.metadata["route_key"], "gw_route_01");
        assert_eq!(task_run.metadata["message_id"], "om_01");
        assert_eq!(task_run.metadata["chat_type"], "group");
        assert_eq!(
            task_run.metadata["attachments"][0]["attachment_id"],
            "att_01"
        );
        assert_eq!(
            task_run.metadata["attachments"][0]["download"]["headers"]["Authorization"],
            "Bearer opaque-download-token"
        );
        assert_eq!(
            task_run.metadata["attachments"][0]["metadata"]["provider_file_key"],
            "file_key_01"
        );

        let task_step = service
            .conversation_store()
            .load_task_step("step-route-message")
            .expect("load task step")
            .expect("task step");
        assert_eq!(task_step.trace_id, "trace-route-message");
        assert_eq!(task_step.route_key, "gw_route_01");
        assert_eq!(
            task_step.input_payload["source"]["route_key"],
            "gw_route_01"
        );
        assert_eq!(task_step.input_payload["message"]["message_id"], "om_01");
        assert_eq!(task_step.input_payload["message"]["chat_type"], "group");
        assert_eq!(
            task_step.input_payload["message"]["attachments"][0]["download"]["mode"],
            "proxy"
        );

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    #[test]
    fn accept_or_replay_task_returns_replayed_response_for_identical_task_id() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-idempotent-replay");
        let service = TaskApiService::new(
            AdminConsoleStore::new(
                admin_path.clone(),
                DeviceRegistryStore::new(registry_path.clone()),
            ),
            TaskConversationStore::new(conversation_path.clone()),
        );
        let request = TaskRequest {
            task_id: "task-idempotent".to_string(),
            trace_id: "trace-idempotent".to_string(),
            step_id: "step-idempotent".to_string(),
            source: TaskSource {
                channel: "im_bridge".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-idempotent".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-idempotent".to_string(),
                route_key: "gw_route_idempotent".to_string(),
            },
            intent: TaskIntent {
                domain: "system".to_string(),
                action: "ping".to_string(),
                raw_text: "ping".to_string(),
            },
            entity_refs: json!({}),
            args: json!({}),
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: "om_idempotent".to_string(),
                chat_type: "group".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        };

        let initial = service.handle_task(request.clone());
        assert_eq!(initial.status, TaskStatus::Failed);

        let replay = service
            .accept_or_replay_task(&request)
            .expect("idempotency decision");
        match replay {
            TaskRequestAcceptance::Replay(response) => {
                assert_eq!(response.task_id, "task-idempotent");
                assert_eq!(response.trace_id, "trace-idempotent");
                assert_eq!(response.status, TaskStatus::Failed);
                assert_eq!(response.executor_used, initial.executor_used);
            }
            other => panic!("expected replay, got {other:?}"),
        }

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    #[test]
    fn accept_or_replay_task_rejects_conflicting_task_identity() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-idempotent-conflict");
        let service = TaskApiService::new(
            AdminConsoleStore::new(
                admin_path.clone(),
                DeviceRegistryStore::new(registry_path.clone()),
            ),
            TaskConversationStore::new(conversation_path.clone()),
        );
        let request = TaskRequest {
            task_id: "task-idempotent-conflict".to_string(),
            trace_id: "trace-idempotent-conflict".to_string(),
            step_id: "step-idempotent-conflict".to_string(),
            source: TaskSource {
                channel: "im_bridge".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-idempotent-conflict".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-idempotent-conflict".to_string(),
                route_key: "gw_route_conflict".to_string(),
            },
            intent: TaskIntent {
                domain: "system".to_string(),
                action: "ping".to_string(),
                raw_text: "ping".to_string(),
            },
            entity_refs: json!({}),
            args: json!({}),
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: "om_conflict".to_string(),
                chat_type: "group".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        };
        let conflicting = TaskRequest {
            intent: TaskIntent {
                domain: "system".to_string(),
                action: "ping".to_string(),
                raw_text: "ping again".to_string(),
            },
            ..request.clone()
        };

        let initial = service.handle_task(request);
        assert_eq!(initial.status, TaskStatus::Failed);

        let replay = service
            .accept_or_replay_task(&conflicting)
            .expect("idempotency decision");
        match replay {
            TaskRequestAcceptance::Conflict(message) => {
                assert!(message.contains("different request identity"));
            }
            other => panic!("expected conflict, got {other:?}"),
        }

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    #[test]
    fn accept_or_replay_task_preserves_original_response_when_turn_local_step_id_is_reused() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-idempotent-step-scope");
        let service = TaskApiService::new(
            AdminConsoleStore::new(
                admin_path.clone(),
                DeviceRegistryStore::new(registry_path.clone()),
            ),
            TaskConversationStore::new(conversation_path.clone()),
        );
        let first = TaskRequest {
            task_id: "task-step-scope-a".to_string(),
            trace_id: "trace-step-scope-a".to_string(),
            step_id: "step_01".to_string(),
            source: TaskSource {
                channel: "im_bridge".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-step-scope".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-step-scope".to_string(),
                route_key: "gw_route_step_scope".to_string(),
            },
            intent: TaskIntent {
                domain: "system".to_string(),
                action: "ping".to_string(),
                raw_text: "ping".to_string(),
            },
            entity_refs: json!({}),
            args: json!({}),
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: "om_step_scope_a".to_string(),
                chat_type: "group".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        };
        let second = TaskRequest {
            task_id: "task-step-scope-b".to_string(),
            trace_id: "trace-step-scope-b".to_string(),
            step_id: "step_01".to_string(),
            source: TaskSource {
                channel: "im_bridge".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-step-scope".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-step-scope".to_string(),
                route_key: "gw_route_step_scope".to_string(),
            },
            intent: TaskIntent {
                domain: "system".to_string(),
                action: "status".to_string(),
                raw_text: "status".to_string(),
            },
            entity_refs: json!({}),
            args: json!({}),
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: "om_step_scope_b".to_string(),
                chat_type: "group".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        };

        let first_response = service.handle_task(first.clone());
        assert_eq!(first_response.status, TaskStatus::Failed);
        assert!(first_response.result.message.contains("system.ping"));

        let second_response = service.handle_task(second);
        assert_eq!(second_response.status, TaskStatus::Failed);
        assert!(second_response.result.message.contains("system.status"));

        assert!(service
            .conversation_store()
            .load_task_step("step_01")
            .expect("load raw step id")
            .is_none());
        let first_step = service
            .conversation_store()
            .load_task_step("task-step-scope-a:step_01")
            .expect("load first scoped step")
            .expect("first scoped step");
        let second_step = service
            .conversation_store()
            .load_task_step("task-step-scope-b:step_01")
            .expect("load second scoped step")
            .expect("second scoped step");
        assert_eq!(first_step.task_id, "task-step-scope-a");
        assert_eq!(second_step.task_id, "task-step-scope-b");

        let replay = service
            .accept_or_replay_task(&first)
            .expect("idempotency decision");
        match replay {
            TaskRequestAcceptance::Replay(response) => {
                assert_eq!(response.status, TaskStatus::Failed);
                assert!(response.result.message.contains("system.ping"));
                assert!(!response.result.message.contains("system.status"));
            }
            other => panic!("expected replay, got {other:?}"),
        }

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    #[test]
    fn protocol_string_joins_protocol_arrays() {
        let args = json!({"protocols":["onvif", "rtsp_probe"]});
        assert_eq!(
            protocol_string(&args),
            Some("onvif + rtsp_probe".to_string())
        );
    }

    #[test]
    fn pending_candidates_only_keep_unreachable_items() {
        let pending = pending_candidates_from_results(&[
            HubScanResultItem {
                candidate_id: "cand-1".to_string(),
                device_id: None,
                name: "Cam 1".to_string(),
                room: String::new(),
                ip: "192.168.1.20".to_string(),
                port: 554,
                protocol: "RTSP".to_string(),
                note: String::new(),
                reachable: false,
                registered: false,
                requires_auth: true,
                vendor: None,
                model: None,
                rtsp_paths: vec!["/live".to_string()],
            },
            HubScanResultItem {
                candidate_id: "cand-2".to_string(),
                device_id: None,
                name: "Cam 2".to_string(),
                room: String::new(),
                ip: "192.168.1.21".to_string(),
                port: 554,
                protocol: "RTSP".to_string(),
                note: String::new(),
                reachable: true,
                registered: true,
                requires_auth: false,
                vendor: None,
                model: None,
                rtsp_paths: vec!["/live".to_string()],
            },
        ]);

        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].candidate_id, "cand-1");
    }

    #[test]
    fn format_pending_candidates_mentions_auth() {
        let rendered = format_pending_candidates(&[PendingTaskCandidate {
            candidate_id: "cand-1".to_string(),
            name: "Living Room Cam".to_string(),
            ip: "192.168.1.20".to_string(),
            room: Some("living room".to_string()),
            port: 554,
            rtsp_paths: vec!["/live".to_string()],
            requires_auth: true,
            vendor: None,
            model: None,
        }]);

        assert!(rendered.contains("需要密码"));
    }

    #[test]
    fn connect_request_preserves_room_hint() {
        let request = super::candidate_to_connect_request(
            &PendingTaskCandidate {
                candidate_id: "cand-1".to_string(),
                name: "Living Room Cam".to_string(),
                ip: "192.168.1.20".to_string(),
                room: Some("Living Room".to_string()),
                port: 554,
                rtsp_paths: vec!["/live".to_string()],
                requires_auth: false,
                vendor: None,
                model: None,
            },
            None,
        );

        assert_eq!(request.room.as_deref(), Some("Living Room"));
        assert!(request.snapshot_url.is_none());
    }

    #[test]
    fn normalize_command_text_strips_punctuation() {
        assert_eq!(
            normalize_command_text("分析 客厅摄像头！"),
            "分析客厅摄像头"
        );
    }

    #[test]
    fn infer_query_from_raw_text_keeps_search_subject() {
        assert_eq!(
            infer_query_from_raw_text("帮我找到和樱花有关的文件"),
            Some("樱花".to_string())
        );
        assert_eq!(
            infer_query_from_raw_text("根据资料回答樱花计划是什么"),
            Some("樱花计划".to_string())
        );
    }

    #[test]
    fn room_aliases_cover_living_room() {
        let aliases = room_aliases("Living Room Cam", "living room");
        assert!(aliases.contains(&"客厅"));
    }

    #[test]
    fn build_artifact_records_maps_image_kind() {
        let request = TaskRequest {
            task_id: "task-1".to_string(),
            trace_id: "trace-1".to_string(),
            step_id: "step-1".to_string(),
            source: TaskSource::default(),
            intent: TaskIntent::default(),
            entity_refs: Value::Null,
            args: Value::Null,
            autonomy: Default::default(),
            message: None,
        };
        let artifacts = build_artifact_records(
            &request,
            "step-1",
            &[super::TaskArtifact {
                kind: "image".to_string(),
                label: "抓拍图片".to_string(),
                mime_type: "image/jpeg".to_string(),
                media_asset_id: Some("asset-1".to_string()),
                path: Some("snap.jpg".to_string()),
                url: None,
                metadata: Value::Null,
            }],
        );

        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].artifact_kind, ArtifactKind::Image);
        assert_eq!(artifacts[0].media_asset_id.as_deref(), Some("asset-1"));
        assert_eq!(artifacts[0].trace_id, "trace-1");
        assert_eq!(artifacts[0].route_key, "");
        assert_eq!(artifact_kind_from_name("json"), ArtifactKind::Json);
    }

    #[test]
    fn build_snapshot_media_asset_populates_platform_fields() {
        let request = TaskRequest {
            task_id: "task-snapshot".to_string(),
            trace_id: "trace-snapshot".to_string(),
            step_id: "step-snapshot".to_string(),
            source: TaskSource {
                channel: "im_bridge".to_string(),
                surface: "harborbeacon".to_string(),
                conversation_id: "chat-1".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-1".to_string(),
                route_key: "gw_route_snapshot".to_string(),
            },
            intent: TaskIntent {
                domain: "camera".to_string(),
                action: "snapshot".to_string(),
                raw_text: "抓拍门口摄像头".to_string(),
            },
            entity_refs: Value::Null,
            args: Value::Null,
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: "om_snapshot".to_string(),
                chat_type: "p2p".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        };
        let target = ResolvedCameraTarget {
            device_id: "cam-1".to_string(),
            display_name: "Front Door".to_string(),
            status: DeviceStatus::Online,
            room_name: Some("Entry".to_string()),
            vendor: Some("DemoCam".to_string()),
            model: Some("C1".to_string()),
            ip_address: Some("192.168.1.10".to_string()),
            mac_address: None,
            discovery_source: "manual_entry".to_string(),
            primary_stream: CameraStreamRef {
                transport: StreamTransport::Rtsp,
                url: "rtsp://192.168.1.10/live".to_string(),
                requires_auth: false,
            },
            snapshot_url: None,
            onvif_device_service_url: None,
            ezviz_device_serial: None,
            ezviz_camera_no: None,
            capabilities: CameraCapabilities {
                snapshot: true,
                stream: true,
                ptz: false,
                audio: false,
            },
            last_seen_at: None,
        };
        let bytes = b"fake-jpeg";
        let snapshot = SnapshotCaptureResult::new(
            "cam-1",
            SnapshotFormat::Jpeg,
            base64::engine::general_purpose::STANDARD.encode(bytes),
            bytes.len(),
            StorageTarget::LocalDisk,
        );
        let expected_captured_at = snapshot.captured_at_epoch_ms.to_string();

        let media_asset = super::build_snapshot_media_asset(&request, &target, &snapshot);
        assert!(media_asset.asset_id.starts_with("asset-"));
        assert_eq!(
            media_asset.workspace_id,
            super::workspace_id_for_request(&request)
        );
        assert_eq!(media_asset.device_id.as_deref(), Some("cam-1"));
        assert_eq!(media_asset.asset_kind, MediaAssetKind::Snapshot);
        assert_eq!(media_asset.storage_target, StorageTargetKind::LocalDisk);
        assert_eq!(media_asset.storage_uri, snapshot.storage.relative_path);
        assert_eq!(media_asset.mime_type, "image/jpeg");
        assert_eq!(media_asset.byte_size, bytes.len() as u64);
        assert_eq!(
            media_asset.captured_at.as_deref(),
            Some(expected_captured_at.as_str())
        );
        assert!(media_asset
            .checksum
            .as_deref()
            .is_some_and(|value| value.starts_with("sha256:")));
        assert_eq!(
            media_asset
                .metadata
                .pointer("/task_id")
                .and_then(Value::as_str),
            Some("task-snapshot")
        );
        assert_eq!(
            media_asset
                .metadata
                .pointer("/device_ingest_metadata/provenance")
                .and_then(Value::as_str),
            Some("media")
        );
        assert_eq!(
            media_asset
                .metadata
                .pointer("/device_ingest_metadata/ingest_disposition")
                .and_then(Value::as_str),
            Some("knowledge_index_candidate")
        );

        let payload = super::build_snapshot_payload(&target, &snapshot, &media_asset);
        assert_eq!(
            payload
                .pointer("/snapshot/media_asset_id")
                .and_then(Value::as_str),
            Some(media_asset.asset_id.as_str())
        );

        let artifact = super::build_snapshot_artifact(&snapshot, &media_asset);
        assert_eq!(
            artifact.media_asset_id.as_deref(),
            Some(media_asset.asset_id.as_str())
        );
        assert_eq!(
            artifact
                .metadata
                .pointer("/media_asset_id")
                .and_then(Value::as_str),
            Some(media_asset.asset_id.as_str())
        );

        let records = build_artifact_records(&request, "step-snapshot", &[artifact]);
        assert_eq!(
            records[0].media_asset_id.as_deref(),
            Some(media_asset.asset_id.as_str())
        );
    }

    #[test]
    fn persist_vision_media_assets_creates_snapshot_and_derived_records() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let snapshot_path = unique_path("harborbeacon-vision-snapshot").with_extension("jpg");
        let annotated_path = unique_path("harborbeacon-vision-annotated").with_extension("jpg");
        fs::write(&snapshot_path, b"snapshot-bytes").expect("write snapshot image");
        fs::write(&annotated_path, b"annotated-bytes").expect("write annotated image");

        let service = TaskApiService::new(
            AdminConsoleStore::new(
                admin_path.clone(),
                DeviceRegistryStore::new(registry_path.clone()),
            ),
            TaskConversationStore::new(conversation_path.clone()),
        );
        let request = TaskRequest {
            task_id: "task-vision".to_string(),
            trace_id: "trace-vision".to_string(),
            step_id: "step-vision".to_string(),
            source: TaskSource {
                channel: "im_bridge".to_string(),
                surface: "harborbeacon".to_string(),
                conversation_id: "chat-1".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-1".to_string(),
                route_key: "gw_route_vision".to_string(),
            },
            intent: TaskIntent {
                domain: "camera".to_string(),
                action: "analyze".to_string(),
                raw_text: "分析门口摄像头".to_string(),
            },
            entity_refs: Value::Null,
            args: Value::Null,
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: "om_vision".to_string(),
                chat_type: "group".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        };
        let target = ResolvedCameraTarget {
            device_id: "cam-1".to_string(),
            display_name: "Front Door".to_string(),
            status: DeviceStatus::Online,
            room_name: Some("Entry".to_string()),
            vendor: None,
            model: None,
            ip_address: Some("192.168.1.10".to_string()),
            mac_address: None,
            discovery_source: "manual_entry".to_string(),
            primary_stream: CameraStreamRef {
                transport: StreamTransport::Rtsp,
                url: "rtsp://192.168.1.10/live".to_string(),
                requires_auth: false,
            },
            snapshot_url: None,
            onvif_device_service_url: None,
            ezviz_device_serial: None,
            ezviz_camera_no: None,
            capabilities: CameraCapabilities {
                snapshot: true,
                stream: true,
                ptz: false,
                audio: false,
            },
            last_seen_at: None,
        };
        let mut payload = json!({
            "summary": "检测到门口有人活动",
            "summary_source": "heuristic_fallback",
            "detection_summary": "检测到 1 个 person",
            "snapshot": {
                "image_path": snapshot_path.to_string_lossy().to_string(),
                "annotated_image_path": annotated_path.to_string_lossy().to_string(),
                "mime_type": "image/jpeg",
                "source_storage": {
                    "target": "local_disk",
                    "relative_path": "snapshots/cam-1/1710000000000.jpg"
                },
                "byte_size": 14,
                "captured_at_epoch_ms": 1710000000000u64
            }
        });

        service
            .persist_vision_media_assets(&request, &target, &mut payload)
            .expect("persist vision media assets");

        let snapshot_asset_id = payload
            .pointer("/snapshot/media_asset_id")
            .and_then(Value::as_str)
            .expect("snapshot media asset id");
        let annotated_asset_id = payload
            .pointer("/snapshot/annotated_media_asset_id")
            .and_then(Value::as_str)
            .expect("annotated media asset id");

        let snapshot_asset = service
            .conversation_store()
            .load_media_asset(snapshot_asset_id)
            .expect("load snapshot media asset")
            .expect("snapshot media asset");
        let annotated_asset = service
            .conversation_store()
            .load_media_asset(annotated_asset_id)
            .expect("load annotated media asset")
            .expect("annotated media asset");

        assert_eq!(snapshot_asset.asset_kind, MediaAssetKind::Snapshot);
        assert_eq!(snapshot_asset.storage_target, StorageTargetKind::LocalDisk);
        assert_eq!(snapshot_asset.byte_size, 14);
        assert_eq!(snapshot_asset.captured_at.as_deref(), Some("1710000000000"));
        assert!(snapshot_asset
            .checksum
            .as_deref()
            .is_some_and(|value| value.starts_with("sha256:")));
        assert_eq!(
            snapshot_asset
                .metadata
                .pointer("/source_storage/relative_path")
                .and_then(Value::as_str),
            Some("snapshots/cam-1/1710000000000.jpg")
        );

        assert_eq!(annotated_asset.asset_kind, MediaAssetKind::Derived);
        assert_eq!(
            annotated_asset.derived_from_asset_id.as_deref(),
            Some(snapshot_asset_id)
        );
        assert_eq!(
            annotated_asset.captured_at.as_deref(),
            Some("1710000000000")
        );
        assert_eq!(
            annotated_asset
                .metadata
                .pointer("/artifact_role")
                .and_then(Value::as_str),
            Some("analysis_annotation")
        );

        let artifacts = super::build_vision_artifacts(&payload);
        assert_eq!(artifacts.len(), 2);
        assert_eq!(
            artifacts[0].media_asset_id.as_deref(),
            Some(snapshot_asset_id)
        );
        assert_eq!(
            artifacts[1].media_asset_id.as_deref(),
            Some(annotated_asset_id)
        );

        let _ = fs::remove_file(snapshot_path);
        let _ = fs::remove_file(annotated_path);
        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    #[test]
    fn build_notification_request_prefers_route_key_contract_shape() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        let conversation_store = TaskConversationStore::new(conversation_path);
        let service = TaskApiService::new(admin_store, conversation_store);
        let request = TaskRequest {
            task_id: "task-vision".to_string(),
            trace_id: "trace-vision".to_string(),
            step_id: "step-vision".to_string(),
            source: TaskSource {
                channel: "im_bridge".to_string(),
                surface: "harborbeacon".to_string(),
                conversation_id: "chat-1".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-1".to_string(),
                route_key: "gw_route_notify".to_string(),
            },
            intent: TaskIntent {
                domain: "camera".to_string(),
                action: "analyze".to_string(),
                raw_text: "分析门口摄像头".to_string(),
            },
            entity_refs: Value::Null,
            args: Value::Null,
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: "om_notify".to_string(),
                chat_type: "group".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        };
        let target = ResolvedCameraTarget {
            device_id: "cam-1".to_string(),
            display_name: "Front Door".to_string(),
            status: DeviceStatus::Online,
            room_name: Some("Entry".to_string()),
            vendor: None,
            model: None,
            ip_address: Some("192.168.1.10".to_string()),
            mac_address: None,
            discovery_source: "onvif".to_string(),
            primary_stream: CameraStreamRef {
                transport: StreamTransport::Rtsp,
                url: "rtsp://192.168.1.10/live".to_string(),
                requires_auth: false,
            },
            snapshot_url: None,
            onvif_device_service_url: None,
            ezviz_device_serial: None,
            ezviz_camera_no: None,
            capabilities: CameraCapabilities {
                snapshot: true,
                stream: true,
                ptz: false,
                audio: false,
            },
            last_seen_at: None,
        };
        let notification = service
            .build_notification_request(
                &request,
                "task.completed",
                &target,
                &json!({
                    "summary": "检测到门口有人活动",
                    "notification_channel": "im_bridge",
                    "notification_format": "lark_card",
                    "notification/destination/recipient/recipient_id": "ou_platform_should_not_be_needed",
                    "notification/destination/recipient/recipient_type": "open_id",
                    "notification_card": {
                        "header": {"title": {"content": "Front Door AI 分析"}}
                    }
                }),
                &[TaskArtifact {
                    kind: "image".to_string(),
                    label: "抓拍图片".to_string(),
                    mime_type: "image/jpeg".to_string(),
                    media_asset_id: None,
                    path: Some("snap.jpg".to_string()),
                    url: None,
                    metadata: Value::Null,
                }],
            )
            .expect("notification request");
        let replay_notification = service
            .build_notification_request(
                &request,
                "task.completed",
                &target,
                &json!({
                    "summary": "检测到门口有人活动",
                    "notification_channel": "im_bridge",
                    "notification_format": "lark_card",
                    "notification_card": {
                        "header": {"title": {"content": "Front Door AI 分析"}}
                    }
                }),
                &[TaskArtifact {
                    kind: "image".to_string(),
                    label: "抓拍图片".to_string(),
                    mime_type: "image/jpeg".to_string(),
                    media_asset_id: None,
                    path: Some("snap.jpg".to_string()),
                    url: None,
                    metadata: Value::Null,
                }],
            )
            .expect("replayed notification request");

        assert_eq!(
            notification.content.payload_format,
            NotificationPayloadFormat::LarkCard
        );
        assert_eq!(
            notification.destination.kind,
            NotificationDestinationKind::Conversation
        );
        assert_eq!(notification.destination.route_key, "gw_route_notify");
        assert_eq!(notification.destination.platform, "");
        assert!(notification.destination.recipient.is_none());
        assert_eq!(notification.content.attachments.len(), 1);
        assert_eq!(notification.content.title, "Front Door AI 分析");
        assert_eq!(notification.source.service, "harborbeacon");
        assert_eq!(notification.source.module, "task_api");
        assert_eq!(notification.source.event_type, "task.completed");
        assert_eq!(notification.delivery.mode, NotificationDeliveryMode::Send);
        assert!(notification.notification_id.starts_with("notif_"));
        assert!(notification.delivery.idempotency_key.starts_with("idem_"));
        assert_eq!(
            notification.notification_id,
            replay_notification.notification_id
        );
        assert_eq!(
            notification.delivery.idempotency_key,
            replay_notification.delivery.idempotency_key
        );

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
    }

    #[test]
    fn build_notification_request_ignores_legacy_recipient_hints_when_route_key_exists() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        let conversation_store = TaskConversationStore::new(conversation_path);
        let service = TaskApiService::new(admin_store, conversation_store);
        let request = TaskRequest {
            task_id: "task-route-opaque".to_string(),
            trace_id: "trace-route-opaque".to_string(),
            step_id: "step-route-opaque".to_string(),
            source: TaskSource {
                channel: "im_bridge".to_string(),
                surface: "harborbeacon".to_string(),
                conversation_id: "chat-opaque".to_string(),
                user_id: "user-opaque".to_string(),
                session_id: "sess-opaque".to_string(),
                route_key: "gw_route_opaque".to_string(),
            },
            intent: TaskIntent {
                domain: "camera".to_string(),
                action: "analyze".to_string(),
                raw_text: "分析门口摄像头".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "notification/destination/recipient/recipient_id": "ou_should_be_ignored",
                "notification/destination/recipient/recipient_type": "open_id",
                "notification_channel": "im_bridge",
            }),
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: "om_route_opaque".to_string(),
                chat_type: "group".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        };
        let target = ResolvedCameraTarget {
            device_id: "cam-opaque".to_string(),
            display_name: "Front Door".to_string(),
            status: DeviceStatus::Online,
            room_name: Some("Entry".to_string()),
            vendor: None,
            model: None,
            ip_address: Some("192.168.1.10".to_string()),
            mac_address: None,
            discovery_source: "onvif".to_string(),
            primary_stream: CameraStreamRef {
                transport: StreamTransport::Rtsp,
                url: "rtsp://192.168.1.10/live".to_string(),
                requires_auth: false,
            },
            snapshot_url: None,
            onvif_device_service_url: None,
            ezviz_device_serial: None,
            ezviz_camera_no: None,
            capabilities: CameraCapabilities {
                snapshot: true,
                stream: true,
                ptz: false,
                audio: false,
            },
            last_seen_at: None,
        };

        let notification = service
            .build_notification_request(
                &request,
                "task.completed",
                &target,
                &json!({
                    "summary": "检测到门口有人活动",
                    "notification_channel": "im_bridge",
                }),
                &[],
            )
            .expect("notification request");

        assert_eq!(
            notification.destination.kind,
            NotificationDestinationKind::Conversation
        );
        assert_eq!(notification.destination.route_key, "gw_route_opaque");
        assert!(notification.destination.recipient.is_none());
        assert_eq!(notification.destination.platform, "");
        assert!(notification.destination.id.is_empty());

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
    }

    #[test]
    fn build_notification_request_retires_legacy_platform_fallback_without_route_key() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        let conversation_store = TaskConversationStore::new(conversation_path);
        let service = TaskApiService::new(admin_store, conversation_store);
        let request = TaskRequest {
            task_id: "task-legacy-fallback".to_string(),
            trace_id: "trace-legacy-fallback".to_string(),
            step_id: "step-legacy-fallback".to_string(),
            source: TaskSource {
                channel: "im_bridge".to_string(),
                surface: "harborbeacon".to_string(),
                conversation_id: "chat-legacy".to_string(),
                user_id: "user-legacy".to_string(),
                session_id: "sess-legacy".to_string(),
                route_key: String::new(),
            },
            intent: TaskIntent {
                domain: "camera".to_string(),
                action: "analyze".to_string(),
                raw_text: "分析门口摄像头".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "notification/destination/recipient/recipient_id": "ou_legacy_should_not_send",
                "notification/destination/recipient/recipient_type": "open_id",
                "notification_channel": "im_bridge",
            }),
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: "om_legacy".to_string(),
                chat_type: "group".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        };
        let target = ResolvedCameraTarget {
            device_id: "cam-legacy".to_string(),
            display_name: "Front Door".to_string(),
            status: DeviceStatus::Online,
            room_name: Some("Entry".to_string()),
            vendor: None,
            model: None,
            ip_address: Some("192.168.1.10".to_string()),
            mac_address: None,
            discovery_source: "onvif".to_string(),
            primary_stream: CameraStreamRef {
                transport: StreamTransport::Rtsp,
                url: "rtsp://192.168.1.10/live".to_string(),
                requires_auth: false,
            },
            snapshot_url: None,
            onvif_device_service_url: None,
            ezviz_device_serial: None,
            ezviz_camera_no: None,
            capabilities: CameraCapabilities {
                snapshot: true,
                stream: true,
                ptz: false,
                audio: false,
            },
            last_seen_at: None,
        };

        assert!(service
            .build_notification_request(
                &request,
                "task.completed",
                &target,
                &json!({
                    "summary": "检测到门口有人活动",
                    "notification_channel": "im_bridge",
                }),
                &[],
            )
            .is_none());

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
    }

    #[test]
    fn general_message_search_like_queries_are_interpreted() {
        let _guard = RETRIEVAL_GATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let request = TaskRequest {
            intent: TaskIntent {
                raw_text: "帮我找到和樱花有关的文件".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };

        assert!(should_route_general_message_to_knowledge(&request));
    }

    #[test]
    fn env_flag_enabled_accepts_common_truthy_strings() {
        assert!(env_flag_enabled("1"));
        assert!(env_flag_enabled("true"));
        assert!(env_flag_enabled("YES"));
        assert!(env_flag_enabled(" on "));
        assert!(!env_flag_enabled("0"));
        assert!(!env_flag_enabled("false"));
        assert!(!env_flag_enabled(""));
    }

    #[test]
    fn ensure_safe_capture_root_allows_explicit_non_harboros_root_with_guard() {
        let original = std::env::var(ALLOW_NON_HARBOROS_CAPTURE_ROOT_ENV).ok();
        let allowed_root = if cfg!(windows) {
            Path::new("C:/tmp/harborbeacon-agent-ci")
        } else {
            Path::new("/home/harbor/work/.tmp-live/harborbeacon-agent-ci")
        };
        unsafe {
            std::env::set_var(ALLOW_NON_HARBOROS_CAPTURE_ROOT_ENV, "1");
        }

        let result = ensure_safe_capture_root(allowed_root);

        match original {
            Some(value) => unsafe {
                std::env::set_var(ALLOW_NON_HARBOROS_CAPTURE_ROOT_ENV, value);
            },
            None => unsafe {
                std::env::remove_var(ALLOW_NON_HARBOROS_CAPTURE_ROOT_ENV);
            },
        }

        assert!(result.is_ok());
    }

    #[test]
    fn handle_camera_connect_resume_token_routes_into_resume_flow_without_platform_identity() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        let conversation_store = TaskConversationStore::new(conversation_path.clone());
        let service = TaskApiService::new(admin_store, conversation_store.clone());

        let session = ConversationSession {
            session_id: "sess-resume".to_string(),
            workspace_id: "home-1".to_string(),
            channel: "feishu".to_string(),
            surface: "harborbeacon".to_string(),
            conversation_id: "chat-resume".to_string(),
            user_id: "user-1".to_string(),
            route_key: "gw_route_resume_opaque".to_string(),
            last_message_id: "om_resume".to_string(),
            chat_type: "p2p".to_string(),
            state: Value::Null,
            resume_token: None,
            expires_at: None,
        };
        let mut conversation = TaskConversationState {
            key: "chat-resume".to_string(),
            ..Default::default()
        };
        conversation.set_camera_pending_connect(Some(PendingTaskConnect {
            resume_token: "resume-opaque-1".to_string(),
            name: "Gate Cam".to_string(),
            ip: "192.168.1.20".to_string(),
            room: Some("Entry".to_string()),
            port: 554,
            snapshot_url: Some("http://192.168.1.20/snapshot.jpg".to_string()),
            rtsp_paths: vec!["/live".to_string()],
            requires_auth: true,
            vendor: Some("Demo".to_string()),
            model: Some("X1".to_string()),
        }));
        conversation_store
            .save_for_session(&session, &conversation)
            .expect("save conversation");

        let request = TaskRequest {
            task_id: "task-resume-opaque".to_string(),
            trace_id: "trace-resume-opaque".to_string(),
            step_id: "step-resume-opaque".to_string(),
            source: TaskSource {
                channel: "feishu".to_string(),
                surface: "harborbeacon".to_string(),
                conversation_id: "chat-resume".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-resume".to_string(),
                route_key: "gw_route_resume_opaque".to_string(),
            },
            intent: TaskIntent {
                domain: "camera".to_string(),
                action: "connect".to_string(),
                raw_text: "密码 xxxxxx".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "continuation_token": "resume-opaque-1",
                "approval": {
                    "token": "approval-opaque-1",
                    "approver_id": "user-1"
                }
            }),
            autonomy: super::TaskAutonomy {
                level: "full".to_string(),
            },
            message: Some(TaskMessage {
                message_id: "om_resume_followup".to_string(),
                chat_type: "p2p".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Failed);
        assert_eq!(response.result.message, "缺少 password，无法继续接入流程。");

        let loaded = conversation_store
            .load_for_session("sess-resume", Some("chat-resume"))
            .expect("load conversation")
            .expect("conversation");
        assert_eq!(
            loaded
                .camera_pending_connect()
                .map(|pending| pending.resume_token),
            Some("resume-opaque-1".to_string())
        );
        assert_eq!(loaded.key, "chat-resume");

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    #[test]
    fn handle_general_message_resume_token_can_confirm_clip_delivery() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        let conversation_store = TaskConversationStore::new(conversation_path.clone());
        let service = TaskApiService::new(admin_store, conversation_store.clone());

        let session = ConversationSession {
            session_id: "sess-clip".to_string(),
            workspace_id: "home-1".to_string(),
            channel: "weixin".to_string(),
            surface: "harborgate".to_string(),
            conversation_id: "chat-clip".to_string(),
            user_id: "user-1".to_string(),
            route_key: "gw_route_clip".to_string(),
            last_message_id: "om_clip".to_string(),
            chat_type: "p2p".to_string(),
            state: Value::Null,
            resume_token: None,
            expires_at: None,
        };
        let mut conversation = TaskConversationState {
            key: "chat-clip".to_string(),
            ..Default::default()
        };
        conversation.set_clip_pending_confirmation(Some(PendingTaskClipConfirmation {
            resume_token: "resume-clip-1".to_string(),
            clip_media_asset_id: "asset-clip-1".to_string(),
            clip_path: "captures/clips/front-door.mp4".to_string(),
            clip_mime_type: "video/mp4".to_string(),
            cover_path: "captures/keyframes/front-door-1.jpg".to_string(),
            display_name: "门口摄像头".to_string(),
        }));
        conversation_store
            .save_for_session(&session, &conversation)
            .expect("save conversation");

        let request = TaskRequest {
            task_id: "task-clip-confirm".to_string(),
            trace_id: "trace-clip-confirm".to_string(),
            step_id: "step-clip-confirm".to_string(),
            source: TaskSource {
                channel: "weixin".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-clip".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-clip".to_string(),
                route_key: "gw_route_clip".to_string(),
            },
            intent: TaskIntent {
                domain: "general".to_string(),
                action: "message".to_string(),
                raw_text: "要".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "continuation_token": "resume-clip-1"
            }),
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: "om_clip_followup".to_string(),
                chat_type: "p2p".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.result.message, "完整回放如下");
        assert_eq!(
            response.result.data["clip_delivery"]["kind"],
            "clip_delivery"
        );
        assert_eq!(response.result.artifacts.len(), 1);
        assert_eq!(response.result.artifacts[0].kind, "video");
        assert_eq!(
            response.result.artifacts[0].media_asset_id.as_deref(),
            Some("asset-clip-1")
        );

        let loaded = conversation_store
            .load_for_session("sess-clip", Some("chat-clip"))
            .expect("load conversation")
            .expect("conversation");
        assert!(loaded.clip_pending_confirmation().is_none());

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    #[test]
    fn handle_general_message_resume_token_accepts_playback_phrase_for_clip_delivery() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        let conversation_store = TaskConversationStore::new(conversation_path.clone());
        let service = TaskApiService::new(admin_store, conversation_store.clone());

        let session = ConversationSession {
            session_id: "sess-clip-playback".to_string(),
            workspace_id: "home-1".to_string(),
            channel: "weixin".to_string(),
            surface: "harborgate".to_string(),
            conversation_id: "chat-clip-playback".to_string(),
            user_id: "user-1".to_string(),
            route_key: "gw_route_clip_playback".to_string(),
            last_message_id: "om_clip_playback".to_string(),
            chat_type: "p2p".to_string(),
            state: Value::Null,
            resume_token: None,
            expires_at: None,
        };
        let mut conversation = TaskConversationState {
            key: "chat-clip-playback".to_string(),
            ..Default::default()
        };
        conversation.set_clip_pending_confirmation(Some(PendingTaskClipConfirmation {
            resume_token: "resume-clip-playback".to_string(),
            clip_media_asset_id: "asset-clip-playback".to_string(),
            clip_path: "captures/clips/front-door-playback.mp4".to_string(),
            clip_mime_type: "video/mp4".to_string(),
            cover_path: "captures/keyframes/front-door-playback.jpg".to_string(),
            display_name: "门口摄像头".to_string(),
        }));
        conversation.set_recent_clip_playback(Some(RecentClipPlaybackState {
            clip_media_asset_id: "asset-clip-playback".to_string(),
            clip_path: "captures/clips/front-door-playback.mp4".to_string(),
            clip_mime_type: "video/mp4".to_string(),
            cover_path: "captures/keyframes/front-door-playback.jpg".to_string(),
            display_name: "门口摄像头".to_string(),
            captured_at_epoch_ms: super::current_epoch_ms(),
        }));
        conversation_store
            .save_for_session(&session, &conversation)
            .expect("save conversation");

        let request = TaskRequest {
            task_id: "task-clip-playback".to_string(),
            trace_id: "trace-clip-playback".to_string(),
            step_id: "step-clip-playback".to_string(),
            source: TaskSource {
                channel: "weixin".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-clip-playback".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-clip-playback".to_string(),
                route_key: "gw_route_clip_playback".to_string(),
            },
            intent: TaskIntent {
                domain: "general".to_string(),
                action: "message".to_string(),
                raw_text: "放一下".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "continuation_token": "resume-clip-playback"
            }),
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: "om_clip_playback_followup".to_string(),
                chat_type: "p2p".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.result.message, "完整回放如下");
        assert_eq!(
            response.result.data["clip_delivery"]["kind"],
            "clip_delivery"
        );
        assert_eq!(response.result.artifacts.len(), 1);
        assert_eq!(response.result.artifacts[0].kind, "video");

        let loaded = conversation_store
            .load_for_session("sess-clip-playback", Some("chat-clip-playback"))
            .expect("load conversation")
            .expect("conversation");
        assert!(loaded.clip_pending_confirmation().is_none());

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    #[test]
    fn handle_general_message_resume_token_can_decline_clip_delivery() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        let conversation_store = TaskConversationStore::new(conversation_path.clone());
        let service = TaskApiService::new(admin_store, conversation_store.clone());

        let session = ConversationSession {
            session_id: "sess-clip-decline".to_string(),
            workspace_id: "home-1".to_string(),
            channel: "weixin".to_string(),
            surface: "harborgate".to_string(),
            conversation_id: "chat-clip-decline".to_string(),
            user_id: "user-1".to_string(),
            route_key: "gw_route_clip_decline".to_string(),
            last_message_id: "om_clip_decline".to_string(),
            chat_type: "p2p".to_string(),
            state: Value::Null,
            resume_token: None,
            expires_at: None,
        };
        let mut conversation = TaskConversationState {
            key: "chat-clip-decline".to_string(),
            ..Default::default()
        };
        conversation.set_clip_pending_confirmation(Some(PendingTaskClipConfirmation {
            resume_token: "resume-clip-decline".to_string(),
            clip_media_asset_id: "asset-clip-decline".to_string(),
            clip_path: "captures/clips/front-door-decline.mp4".to_string(),
            clip_mime_type: "video/mp4".to_string(),
            cover_path: "captures/keyframes/front-door-decline.jpg".to_string(),
            display_name: "门口摄像头".to_string(),
        }));
        conversation_store
            .save_for_session(&session, &conversation)
            .expect("save conversation");

        let request = TaskRequest {
            task_id: "task-clip-decline".to_string(),
            trace_id: "trace-clip-decline".to_string(),
            step_id: "step-clip-decline".to_string(),
            source: TaskSource {
                channel: "weixin".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-clip-decline".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-clip-decline".to_string(),
                route_key: "gw_route_clip_decline".to_string(),
            },
            intent: TaskIntent {
                domain: "general".to_string(),
                action: "message".to_string(),
                raw_text: "不要".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "continuation_token": "resume-clip-decline"
            }),
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: "om_clip_decline_followup".to_string(),
                chat_type: "p2p".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.result.message, "好的，这段回放先不发。");
        assert!(response.result.artifacts.is_empty());
        assert_eq!(
            response.result.data["clip_confirmation"]["decision"],
            "declined"
        );

        let loaded = conversation_store
            .load_for_session("sess-clip-decline", Some("chat-clip-decline"))
            .expect("load conversation")
            .expect("conversation");
        assert!(loaded.clip_pending_confirmation().is_none());

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    #[test]
    fn general_message_can_replay_recent_clip_without_resume_token() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        let conversation_store = TaskConversationStore::new(conversation_path.clone());
        let service = TaskApiService::new(admin_store, conversation_store.clone());

        let session = ConversationSession {
            session_id: "sess-clip-replay".to_string(),
            workspace_id: "home-1".to_string(),
            channel: "weixin".to_string(),
            surface: "harborgate".to_string(),
            conversation_id: "chat-clip-replay".to_string(),
            user_id: "user-1".to_string(),
            route_key: "gw_route_clip_replay".to_string(),
            last_message_id: "om_clip_replay".to_string(),
            chat_type: "p2p".to_string(),
            state: Value::Null,
            resume_token: None,
            expires_at: None,
        };
        let mut conversation = TaskConversationState {
            key: "chat-clip-replay".to_string(),
            ..Default::default()
        };
        conversation.set_recent_clip_playback(Some(RecentClipPlaybackState {
            clip_media_asset_id: "asset-clip-replay".to_string(),
            clip_path: "captures/clips/front-door-replay.mp4".to_string(),
            clip_mime_type: "video/mp4".to_string(),
            cover_path: "captures/keyframes/front-door-replay.jpg".to_string(),
            display_name: "门口摄像头".to_string(),
            captured_at_epoch_ms: super::current_epoch_ms(),
        }));
        conversation_store
            .save_for_session(&session, &conversation)
            .expect("save conversation");

        let request = TaskRequest {
            task_id: "task-clip-replay".to_string(),
            trace_id: "trace-clip-replay".to_string(),
            step_id: "step-clip-replay".to_string(),
            source: TaskSource {
                channel: "weixin".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-clip-replay".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-clip-replay".to_string(),
                route_key: "gw_route_clip_replay".to_string(),
            },
            intent: TaskIntent {
                domain: "general".to_string(),
                action: "message".to_string(),
                raw_text: "现在回放一下".to_string(),
            },
            entity_refs: Value::Null,
            args: Value::Null,
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: "om_clip_replay_followup".to_string(),
                chat_type: "p2p".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.executor_used, "camera_hub_service");
        assert_eq!(response.result.message, "完整回放如下");
        assert_eq!(
            response.result.data["clip_delivery"]["kind"],
            "clip_delivery"
        );
        assert_eq!(
            response.result.data["general_message_controller"]["controller_stage"],
            "deterministic_single_candidate"
        );
        assert_eq!(response.result.artifacts.len(), 1);
        assert_eq!(response.result.artifacts[0].kind, "video");
        assert_eq!(
            response.result.artifacts[0].media_asset_id.as_deref(),
            Some("asset-clip-replay")
        );

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    #[test]
    fn handle_camera_share_link_returns_link_artifact() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let registry_store = DeviceRegistryStore::new(registry_path.clone());
        let admin_store = AdminConsoleStore::new(admin_path.clone(), registry_store.clone());
        let conversation_store = TaskConversationStore::new(conversation_path.clone());
        let service = TaskApiService::new(admin_store, conversation_store);

        let mut device = CameraDevice::new("cam-share", "Front Door", "rtsp://192.168.1.10/live");
        device.status = DeviceStatus::Online;
        device.room = Some("Entry".to_string());
        device.discovery_source = "manual_entry".to_string();
        device.capabilities.snapshot = true;
        device.capabilities.stream = true;
        registry_store
            .save_devices(&[device])
            .expect("save registry device");
        service
            .clone()
            .admin_store
            .save_remote_view_config(RemoteViewConfig {
                share_secret: "platform-share-secret".to_string(),
                share_link_ttl_minutes: 45,
            })
            .expect("save remote view config");

        let response = service.handle_task(TaskRequest {
            task_id: "task-share".to_string(),
            trace_id: "trace-share".to_string(),
            step_id: "step-share".to_string(),
            source: TaskSource {
                channel: "admin_api".to_string(),
                surface: "agent_hub_admin_api".to_string(),
                conversation_id: "admin-console".to_string(),
                user_id: "local-admin".to_string(),
                session_id: "admin-console".to_string(),
                route_key: String::new(),
            },
            intent: TaskIntent {
                domain: "camera".to_string(),
                action: "share_link".to_string(),
                raw_text: "生成共享观看链接".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "device_id": "cam-share",
            }),
            autonomy: Default::default(),
            message: None,
        });

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(
            response.risk_level,
            crate::orchestrator::contracts::RiskLevel::Medium
        );
        let share_link_id = response.result.data["share_link"]["share_link_id"]
            .as_str()
            .expect("share link id");
        let media_session_id = response.result.data["share_link"]["media_session_id"]
            .as_str()
            .expect("media session id");
        assert_eq!(response.result.artifacts.len(), 1);
        assert_eq!(response.result.artifacts[0].kind, "link");
        assert!(response.result.artifacts[0]
            .url
            .as_deref()
            .expect("share url")
            .starts_with("/shared/cameras/"));
        assert_eq!(response.result.data["share_link"]["ttl_minutes"], 45);
        assert_eq!(
            response.result.artifacts[0].metadata["share_link_id"],
            json!(share_link_id)
        );
        assert_eq!(
            response.result.events[0]["event_type"],
            "task.share_link_issued"
        );
        let share_url = response.result.artifacts[0]
            .url
            .as_deref()
            .expect("share url");
        let share_token = share_url.trim_start_matches("/shared/cameras/");
        let share_link = service
            .conversation_store()
            .load_share_link(share_link_id)
            .expect("load share link")
            .expect("share link");
        let media_session = service
            .conversation_store()
            .load_media_session(media_session_id)
            .expect("load media session")
            .expect("media session");
        assert_eq!(
            share_link.token_hash,
            crate::runtime::remote_view::camera_share_token_hash(share_token)
        );
        assert_eq!(share_link.media_session_id, media_session.media_session_id);
        assert_eq!(media_session.device_id, "cam-share");

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    #[test]
    fn handle_camera_live_view_alias_returns_link_artifact() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let registry_store = DeviceRegistryStore::new(registry_path.clone());
        let admin_store = AdminConsoleStore::new(admin_path.clone(), registry_store.clone());
        let conversation_store = TaskConversationStore::new(conversation_path.clone());
        let service = TaskApiService::new(admin_store, conversation_store);

        let mut device = CameraDevice::new("cam-share", "Front Door", "rtsp://192.168.1.10/live");
        device.status = DeviceStatus::Online;
        device.room = Some("Entry".to_string());
        device.discovery_source = "manual_entry".to_string();
        device.capabilities.snapshot = true;
        device.capabilities.stream = true;
        registry_store
            .save_devices(&[device])
            .expect("save registry device");
        service
            .clone()
            .admin_store
            .save_remote_view_config(RemoteViewConfig {
                share_secret: "platform-share-secret".to_string(),
                share_link_ttl_minutes: 45,
            })
            .expect("save remote view config");

        let response = service.handle_task(TaskRequest {
            task_id: "task-live-view".to_string(),
            trace_id: "trace-live-view".to_string(),
            step_id: "step-live-view".to_string(),
            source: TaskSource {
                channel: "admin_api".to_string(),
                surface: "agent_hub_admin_api".to_string(),
                conversation_id: "admin-console".to_string(),
                user_id: "local-admin".to_string(),
                session_id: "admin-session".to_string(),
                route_key: String::new(),
            },
            intent: TaskIntent {
                domain: "camera".to_string(),
                action: "live_view".to_string(),
                raw_text: "生成共享观看链接".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "device_id": "cam-share"
            }),
            autonomy: Default::default(),
            message: None,
        });

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.executor_used, "camera_hub_service");
        assert_eq!(
            response.result.data["camera_target"]["device_id"],
            "cam-share"
        );
        assert_eq!(response.result.data["share_link"]["device_id"], "cam-share");
        assert_eq!(response.result.artifacts.len(), 1);
        assert_eq!(response.result.artifacts[0].kind, "link");
        assert_eq!(
            response.result.events[0]["event_type"],
            "task.share_link_issued"
        );

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    #[test]
    fn resolve_notification_recipient_prefers_bound_chat_id() {
        let state = AdminConsoleState {
            identity_bindings: vec![IdentityBindingRecord {
                open_id: "ou_demo".to_string(),
                user_id: Some("user-1".to_string()),
                union_id: None,
                display_name: "家庭通知频道".to_string(),
                chat_id: Some("oc_demo".to_string()),
            }],
            ..Default::default()
        };
        let recipient =
            resolve_notification_recipient("家庭通知频道", &state, "user-1").expect("recipient");

        assert_eq!(
            recipient.recipient_type,
            NotificationRecipientIdType::ChatId
        );
        assert_eq!(recipient.recipient_id, "oc_demo");
    }

    #[test]
    fn resolve_notification_recipient_prefers_platform_binding_when_legacy_empty() {
        let mut state = AdminConsoleState::default();
        state.platform.identity_bindings.push(IdentityBinding {
            identity_id: "identity-ou_platform".to_string(),
            user_id: "user-1".to_string(),
            auth_source: AuthSource::ImChannel,
            provider_key: "im_bridge".to_string(),
            external_user_id: "ou_platform".to_string(),
            external_union_id: None,
            external_chat_id: Some("oc_platform".to_string()),
            profile_snapshot: json!({
                "display_name": "平台通知频道",
            }),
            last_seen_at: None,
        });

        let recipient =
            resolve_notification_recipient("平台通知频道", &state, "user-1").expect("recipient");

        assert_eq!(
            recipient.recipient_type,
            NotificationRecipientIdType::ChatId
        );
        assert_eq!(recipient.recipient_id, "oc_platform");
    }

    #[test]
    fn build_notification_request_uses_member_default_surface_for_proactive_delivery() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        let conversation_store = TaskConversationStore::new(conversation_path);
        let service = TaskApiService::new(admin_store.clone(), conversation_store);
        admin_store
            .upsert_notification_target(None, "我的微信", "gw_route_weixin_default", "weixin", true)
            .expect("save notification target");

        let request = TaskRequest {
            task_id: "task-proactive".to_string(),
            trace_id: "trace-proactive".to_string(),
            step_id: "step-proactive".to_string(),
            source: TaskSource {
                channel: "admin_api".to_string(),
                surface: "harbor-assistant".to_string(),
                conversation_id: String::new(),
                user_id: "user-weixin".to_string(),
                session_id: "sess-proactive".to_string(),
                route_key: String::new(),
            },
            intent: TaskIntent {
                domain: "camera".to_string(),
                action: "analyze".to_string(),
                raw_text: "后台分析告警".to_string(),
            },
            entity_refs: Value::Null,
            args: Value::Null,
            autonomy: Default::default(),
            message: None,
        };
        let target = ResolvedCameraTarget {
            device_id: "cam-proactive".to_string(),
            display_name: "Front Door".to_string(),
            status: DeviceStatus::Online,
            room_name: Some("Entry".to_string()),
            vendor: None,
            model: None,
            ip_address: Some("192.168.1.10".to_string()),
            mac_address: None,
            discovery_source: "onvif".to_string(),
            primary_stream: CameraStreamRef {
                transport: StreamTransport::Rtsp,
                url: "rtsp://192.168.1.10/live".to_string(),
                requires_auth: false,
            },
            snapshot_url: None,
            onvif_device_service_url: None,
            ezviz_device_serial: None,
            ezviz_camera_no: None,
            capabilities: CameraCapabilities {
                snapshot: true,
                stream: true,
                ptz: false,
                audio: false,
            },
            last_seen_at: None,
        };

        let notification = service
            .build_notification_request(
                &request,
                "task.completed",
                &target,
                &json!({
                    "summary": "独立系统提醒",
                    "notification_channel": "im_bridge",
                }),
                &[],
            )
            .expect("proactive notification");

        assert_eq!(
            notification.destination.kind,
            NotificationDestinationKind::Conversation
        );
        assert_eq!(notification.destination.platform, "weixin");
        assert_eq!(
            notification.destination.route_key,
            "gw_route_weixin_default"
        );
        assert!(notification.destination.id.is_empty());
        assert!(notification.destination.recipient.is_none());

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
    }

    #[test]
    fn notification_delivery_outcome_marks_rejected_requests() {
        let request = NotificationRequest {
            notification_id: "notif_01JABC".to_string(),
            trace_id: "trace_01JABC".to_string(),
            source: NotificationSource {
                service: "harborbeacon".to_string(),
                module: "task_api".to_string(),
                event_type: "task.completed".to_string(),
            },
            destination: NotificationDestination {
                kind: NotificationDestinationKind::Conversation,
                route_key: "gw_route_notify_fail".to_string(),
                id: String::new(),
                platform: String::new(),
                recipient: None,
            },
            content: crate::connectors::notifications::NotificationContent {
                title: "AI 分析".to_string(),
                body: "检测到人员活动".to_string(),
                payload_format: NotificationPayloadFormat::PlainText,
                structured_payload: Value::Null,
                attachments: Vec::new(),
            },
            delivery: NotificationDelivery {
                mode: NotificationDeliveryMode::Send,
                reply_to_message_id: String::new(),
                update_message_id: String::new(),
                idempotency_key: "idem_01JABC".to_string(),
            },
            metadata: NotificationMetadata {
                correlation_id: "trace_01JABC".to_string(),
            },
        };
        let outcome = notification_delivery_outcome(
            &request,
            Err(NotificationDeliveryError::RequestRejected {
                status_code: 404,
                envelope: SharedHttpErrorEnvelope {
                    ok: false,
                    error: SharedHttpErrorDetail {
                        code: "ROUTE_NOT_FOUND".to_string(),
                        message: "route expired".to_string(),
                    },
                    trace_id: Some("trace_01JABC".to_string()),
                },
            }),
        );

        assert_eq!(outcome.event_type, "task.notification_rejected");
        assert_eq!(outcome.payload["status"], "rejected");
        assert_eq!(outcome.payload["http_status"], 404);
        assert_eq!(outcome.payload["error"]["code"], "ROUTE_NOT_FOUND");
    }

    #[test]
    fn proactive_notification_delivery_outcome_uses_proactive_failure_event() {
        let request = NotificationRequest {
            notification_id: "notif_proactive".to_string(),
            trace_id: "trace_proactive".to_string(),
            source: NotificationSource {
                service: "harborbeacon".to_string(),
                module: "task_api".to_string(),
                event_type: "task.completed".to_string(),
            },
            destination: NotificationDestination {
                kind: NotificationDestinationKind::Conversation,
                route_key: "gw_route_weixin_default".to_string(),
                id: String::new(),
                platform: "weixin".to_string(),
                recipient: None,
            },
            content: NotificationContent {
                title: "系统提醒".to_string(),
                body: "请检查状态".to_string(),
                payload_format: NotificationPayloadFormat::PlainText,
                structured_payload: Value::Null,
                attachments: Vec::new(),
            },
            delivery: NotificationDelivery {
                mode: NotificationDeliveryMode::Send,
                reply_to_message_id: String::new(),
                update_message_id: String::new(),
                idempotency_key: "idem_proactive".to_string(),
            },
            metadata: NotificationMetadata {
                correlation_id: "trace_proactive".to_string(),
            },
        };

        let outcome = notification_delivery_outcome(
            &request,
            Err(NotificationDeliveryError::Transport(
                "context token missing".to_string(),
            )),
        );

        assert_eq!(outcome.event_type, "task.proactive_delivery_failed");
        assert_eq!(outcome.payload["route_mode"], "proactive");
        assert_eq!(outcome.payload["destination"]["platform"], "weixin");
    }

    #[test]
    fn effective_requires_approval_defaults_camera_connect_only() {
        let connect_request = TaskRequest {
            task_id: "task-connect".to_string(),
            trace_id: "trace-connect".to_string(),
            step_id: "step-connect".to_string(),
            source: TaskSource {
                channel: "im_bridge".to_string(),
                surface: "harborbeacon".to_string(),
                conversation_id: "chat-1".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-1".to_string(),
                route_key: String::new(),
            },
            intent: TaskIntent {
                domain: "camera".to_string(),
                action: "connect".to_string(),
                raw_text: "接入摄像头".to_string(),
            },
            entity_refs: Value::Null,
            args: Value::Null,
            autonomy: Default::default(),
            message: None,
        };
        let scan_request = TaskRequest {
            task_id: "task-scan".to_string(),
            trace_id: "trace-scan".to_string(),
            step_id: "step-scan".to_string(),
            source: connect_request.source.clone(),
            intent: TaskIntent {
                domain: "camera".to_string(),
                action: "scan".to_string(),
                raw_text: "扫描摄像头".to_string(),
            },
            entity_refs: Value::Null,
            args: Value::Null,
            autonomy: Default::default(),
            message: None,
        };

        assert!(effective_requires_approval(&connect_request));
        assert!(!effective_requires_approval(&scan_request));
    }

    #[test]
    fn effective_autonomy_defaults_to_supervised_and_normalizes_aliases() {
        let default_request = TaskRequest {
            task_id: "task-autonomy-default".to_string(),
            trace_id: "trace-autonomy-default".to_string(),
            step_id: "step-autonomy-default".to_string(),
            source: TaskSource::default(),
            intent: TaskIntent {
                domain: "camera".to_string(),
                action: "scan".to_string(),
                raw_text: "扫描摄像头".to_string(),
            },
            entity_refs: Value::Null,
            args: Value::Null,
            autonomy: Default::default(),
            message: None,
        };
        let readonly_request = TaskRequest {
            task_id: "task-autonomy-readonly".to_string(),
            trace_id: "trace-autonomy-readonly".to_string(),
            step_id: "step-autonomy-readonly".to_string(),
            source: TaskSource::default(),
            intent: TaskIntent {
                domain: "camera".to_string(),
                action: "connect".to_string(),
                raw_text: "接入摄像头".to_string(),
            },
            entity_refs: Value::Null,
            args: Value::Null,
            autonomy: super::TaskAutonomy {
                level: "ReadOnly".to_string(),
            },
            message: None,
        };

        assert_eq!(
            format!("{:?}", effective_autonomy_level(&default_request)),
            "Supervised"
        );
        assert_eq!(
            effective_autonomy_level_for_task_run(&default_request),
            "supervised"
        );
        assert_eq!(
            format!("{:?}", effective_autonomy_level(&readonly_request)),
            "ReadOnly"
        );
        assert_eq!(
            effective_autonomy_level_for_task_run(&readonly_request),
            "readonly"
        );
    }

    #[test]
    fn handle_camera_connect_blocks_by_default_until_approved() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        let conversation_store = TaskConversationStore::new(conversation_path.clone());
        let service = TaskApiService::new(admin_store, conversation_store.clone());
        let request = TaskRequest {
            task_id: "task-connect-approval".to_string(),
            trace_id: "trace-connect-approval".to_string(),
            step_id: "step-connect-approval".to_string(),
            source: TaskSource {
                channel: "im_bridge".to_string(),
                surface: "harborbeacon".to_string(),
                conversation_id: "chat-1".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-1".to_string(),
                route_key: "gw_route_connect_approval".to_string(),
            },
            intent: TaskIntent {
                domain: "camera".to_string(),
                action: "connect".to_string(),
                raw_text: "接入摄像头".to_string(),
            },
            entity_refs: Value::Null,
            args: Value::Null,
            autonomy: Default::default(),
            message: None,
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::NeedsInput);
        assert_eq!(response.missing_fields, vec!["approval_token".to_string()]);
        assert_eq!(
            response.result.data["approval_ticket"]["policy_ref"],
            "camera.connect"
        );

        let task_run = conversation_store
            .load_task_run("task-connect-approval")
            .expect("load task run")
            .expect("task run");
        assert_eq!(task_run.status, TaskRunStatus::NeedsInput);
        assert!(task_run.requires_approval);

        let approvals = conversation_store
            .approvals_for_task("task-connect-approval")
            .expect("load approvals");
        assert_eq!(approvals.len(), 1);
        assert_eq!(approvals[0].policy_ref, "camera.connect");
        assert_eq!(approvals[0].status, ApprovalStatus::Pending);

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    #[test]
    fn handle_camera_connect_fails_under_readonly_autonomy_before_approval() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        let conversation_store = TaskConversationStore::new(conversation_path.clone());
        let service = TaskApiService::new(admin_store, conversation_store.clone());
        let request = TaskRequest {
            task_id: "task-connect-readonly".to_string(),
            trace_id: "trace-connect-readonly".to_string(),
            step_id: "step-connect-readonly".to_string(),
            source: TaskSource {
                channel: "im_bridge".to_string(),
                surface: "harborbeacon".to_string(),
                conversation_id: "chat-1".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-1".to_string(),
                route_key: "gw_route_connect_readonly".to_string(),
            },
            intent: TaskIntent {
                domain: "camera".to_string(),
                action: "connect".to_string(),
                raw_text: "接入摄像头".to_string(),
            },
            entity_refs: Value::Null,
            args: Value::Null,
            autonomy: super::TaskAutonomy {
                level: "ReadOnly".to_string(),
            },
            message: None,
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Failed);
        assert_eq!(response.result.data["error"], "AUTONOMY_BLOCKED");
        assert_eq!(response.result.data["autonomy_level"], "readonly");

        let task_run = conversation_store
            .load_task_run("task-connect-readonly")
            .expect("load task run")
            .expect("task run");
        assert_eq!(task_run.status, TaskRunStatus::Failed);
        assert_eq!(task_run.autonomy_level, "readonly");

        let approvals = conversation_store
            .approvals_for_task("task-connect-readonly")
            .expect("load approvals");
        assert!(approvals.is_empty());

        let events = conversation_store
            .events_for_task("task-connect-readonly")
            .expect("load events");
        assert!(events
            .iter()
            .any(|event| event.event_type == "task.autonomy_blocked"));
        assert!(events.iter().any(|event| event.event_type == "task.failed"));

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    #[test]
    fn handle_camera_connect_with_full_autonomy_and_token_skips_approval_prompt() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        let conversation_store = TaskConversationStore::new(conversation_path.clone());
        let service = TaskApiService::new(admin_store, conversation_store.clone());
        let request = TaskRequest {
            task_id: "task-connect-full".to_string(),
            trace_id: "trace-connect-full".to_string(),
            step_id: "step-connect-full".to_string(),
            source: TaskSource {
                channel: "im_bridge".to_string(),
                surface: "harborbeacon".to_string(),
                conversation_id: "chat-1".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-1".to_string(),
                route_key: "gw_route_connect_full".to_string(),
            },
            intent: TaskIntent {
                domain: "camera".to_string(),
                action: "connect".to_string(),
                raw_text: "接入摄像头".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "approval": {
                    "token": "approved-token",
                    "approver_id": "user-1"
                }
            }),
            autonomy: super::TaskAutonomy {
                level: "full".to_string(),
            },
            message: None,
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Failed);
        assert_ne!(response.missing_fields, vec!["approval_token".to_string()]);
        assert!(
            response.result.message.contains("缺少摄像头 IP 地址"),
            "unexpected response: {}",
            response.result.message
        );

        let task_run = conversation_store
            .load_task_run("task-connect-full")
            .expect("load task run")
            .expect("task run");
        assert_eq!(task_run.status, TaskRunStatus::Failed);
        assert_eq!(task_run.autonomy_level, "full");
        assert!(task_run.requires_approval);

        let approvals = conversation_store
            .approvals_for_task("task-connect-full")
            .expect("load approvals");
        assert!(approvals.is_empty());

        let events = conversation_store
            .events_for_task("task-connect-full")
            .expect("load events");
        assert!(!events
            .iter()
            .any(|event| event.event_type == "task.approval_required"));
        assert!(events.iter().any(|event| event.event_type == "task.failed"));

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    #[test]
    fn approve_pending_approval_replays_task_request() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        let conversation_store = TaskConversationStore::new(conversation_path.clone());
        let service = TaskApiService::new(admin_store, conversation_store.clone());
        let request = TaskRequest {
            task_id: "task-approve-replay".to_string(),
            trace_id: "trace-approve-replay".to_string(),
            step_id: "step-approve-replay".to_string(),
            source: TaskSource {
                channel: "im_bridge".to_string(),
                surface: "harborbeacon".to_string(),
                conversation_id: "chat-1".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-1".to_string(),
                route_key: "gw_route_approve_replay".to_string(),
            },
            intent: TaskIntent {
                domain: "camera".to_string(),
                action: "connect".to_string(),
                raw_text: "接入摄像头".to_string(),
            },
            entity_refs: Value::Null,
            args: Value::Null,
            autonomy: Default::default(),
            message: None,
        };

        let initial = service.handle_task(request);
        let approval_id = initial.result.data["approval_ticket"]["approval_id"]
            .as_str()
            .expect("approval id")
            .to_string();
        let pending = service.pending_approvals().expect("pending approvals");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].approval_ticket.approval_id, approval_id);

        let (approval, resumed) = service
            .approve_pending_approval(&approval_id, Some("approver-1".to_string()))
            .expect("approve");

        assert_eq!(approval.approval_ticket.status, ApprovalStatus::Approved);
        assert_eq!(
            approval.approval_ticket.approver_user_id.as_deref(),
            Some("approver-1")
        );
        assert_eq!(resumed.status, TaskStatus::Failed);
        assert!(resumed.result.message.contains("缺少摄像头 IP 地址"));

        let approvals = conversation_store
            .approvals_for_task("task-approve-replay")
            .expect("load approvals");
        assert_eq!(approvals.len(), 1);
        assert_eq!(approvals[0].status, ApprovalStatus::Approved);

        let events = conversation_store
            .events_for_task("task-approve-replay")
            .expect("load events");
        assert!(events
            .iter()
            .any(|event| event.event_type == "task.approval_approved"));
        assert!(events.iter().any(|event| event.event_type == "task.failed"));

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    #[test]
    fn reject_pending_approval_closes_task() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-reject");
        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        let conversation_store = TaskConversationStore::new(conversation_path.clone());
        let service = TaskApiService::new(admin_store, conversation_store.clone());
        let request = TaskRequest {
            task_id: "task-reject-approval".to_string(),
            trace_id: "trace-reject-approval".to_string(),
            step_id: "step-reject-approval".to_string(),
            source: TaskSource {
                channel: "im_bridge".to_string(),
                surface: "harborbeacon".to_string(),
                conversation_id: "chat-1".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-1".to_string(),
                route_key: "gw_route_reject_approval".to_string(),
            },
            intent: TaskIntent {
                domain: "camera".to_string(),
                action: "connect".to_string(),
                raw_text: "接入摄像头".to_string(),
            },
            entity_refs: Value::Null,
            args: Value::Null,
            autonomy: Default::default(),
            message: None,
        };

        let initial = service.handle_task(request);
        let approval_id = initial.result.data["approval_ticket"]["approval_id"]
            .as_str()
            .expect("approval id")
            .to_string();

        let approval = service
            .reject_pending_approval(&approval_id, Some("approver-2".to_string()))
            .expect("reject");

        assert_eq!(approval.approval_ticket.status, ApprovalStatus::Rejected);
        assert_eq!(
            approval.approval_ticket.approver_user_id.as_deref(),
            Some("approver-2")
        );

        let task_run = conversation_store
            .load_task_run("task-reject-approval")
            .expect("load task run")
            .expect("task run");
        assert_eq!(task_run.status, TaskRunStatus::Failed);
        assert!(task_run.completed_at.is_some());

        let session = conversation_store
            .load_session("sess-1")
            .expect("load session")
            .expect("session");
        assert!(session.resume_token.is_none());

        let events = conversation_store
            .events_for_task("task-reject-approval")
            .expect("load events");
        assert!(events
            .iter()
            .any(|event| event.event_type == "task.approval_rejected"));

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    #[test]
    fn handle_task_blocks_when_approval_required_without_token() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        let conversation_store = TaskConversationStore::new(conversation_path.clone());
        let service = TaskApiService::new(admin_store, conversation_store.clone());
        let request = TaskRequest {
            task_id: "task-approval".to_string(),
            trace_id: "trace-approval".to_string(),
            step_id: "step-approval".to_string(),
            source: TaskSource {
                channel: "im_bridge".to_string(),
                surface: "harborbeacon".to_string(),
                conversation_id: "chat-1".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-1".to_string(),
                route_key: "gw_route_task_approval".to_string(),
            },
            intent: TaskIntent {
                domain: "camera".to_string(),
                action: "scan".to_string(),
                raw_text: "扫描摄像头".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "approval": {
                    "required": true
                }
            }),
            autonomy: Default::default(),
            message: None,
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::NeedsInput);
        assert_eq!(response.missing_fields, vec!["approval_token".to_string()]);
        assert_eq!(
            response.result.data["approval_ticket"]["task_id"],
            "task-approval"
        );

        let task_run = conversation_store
            .load_task_run("task-approval")
            .expect("load task run")
            .expect("task run");
        assert_eq!(task_run.status, TaskRunStatus::NeedsInput);
        assert!(task_run.requires_approval);

        let approvals = conversation_store
            .approvals_for_task("task-approval")
            .expect("load approvals");
        assert_eq!(approvals.len(), 1);
        assert_eq!(approvals[0].status, ApprovalStatus::Pending);
        assert_eq!(approvals[0].trace_id, "trace-approval");
        assert_eq!(approvals[0].route_key, "gw_route_task_approval");

        let events = conversation_store
            .events_for_task("task-approval")
            .expect("load events");
        assert!(events
            .iter()
            .any(|event| event.event_type == "task.approval_required"));
        assert!(events
            .iter()
            .any(|event| event.event_type == "task.needs_input"));

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    #[test]
    fn handle_task_persists_runtime_records_for_failures() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        let conversation_store = TaskConversationStore::new(conversation_path.clone());
        let service = TaskApiService::new(admin_store, conversation_store.clone());
        let request = TaskRequest {
            task_id: "task-unsupported".to_string(),
            trace_id: "trace-unsupported".to_string(),
            step_id: "step-unsupported".to_string(),
            source: TaskSource {
                channel: "feishu".to_string(),
                surface: "harborbeacon".to_string(),
                conversation_id: "chat-1".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-1".to_string(),
                route_key: "gw_route_unsupported".to_string(),
            },
            intent: TaskIntent {
                domain: "system".to_string(),
                action: "ping".to_string(),
                raw_text: "测试一下".to_string(),
            },
            entity_refs: Value::Null,
            args: Value::Null,
            autonomy: Default::default(),
            message: None,
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Failed);
        let task_run = conversation_store
            .load_task_run("task-unsupported")
            .expect("load task run")
            .expect("task run");
        assert_eq!(task_run.status, TaskRunStatus::Failed);
        assert_eq!(task_run.session_id, "sess-1");
        assert_eq!(task_run.autonomy_level, "supervised");

        let task_step = conversation_store
            .load_task_step("step-unsupported")
            .expect("load task step")
            .expect("task step");
        assert_eq!(task_step.status, TaskStepRunStatus::Failed);
        assert_eq!(task_step.executor_used, "task_api");

        let session = conversation_store
            .load_session("sess-1")
            .expect("load session")
            .expect("session");
        assert_eq!(session.channel, "feishu");
        let events = conversation_store
            .events_for_task("task-unsupported")
            .expect("load events");
        assert!(events.iter().any(|event| event.event_type == "task.failed"));

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    #[test]
    fn handle_service_status_dispatches_to_harboros_router() {
        let _guard = HARBOROS_TASK_API_TEST_LOCK.lock().expect("lock");
        reset_harbor_task_api_env();
        let (service, conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("harbor-service-status");
        let request = TaskRequest {
            task_id: "task-service-status".to_string(),
            trace_id: "trace-service-status".to_string(),
            step_id: "step-service-status".to_string(),
            source: TaskSource {
                channel: "feishu".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-service-status".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-service-status".to_string(),
                route_key: "gw_route_service_status".to_string(),
            },
            intent: TaskIntent {
                domain: "service".to_string(),
                action: "status".to_string(),
                raw_text: "查看 ssh 状态".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "resource": {
                    "service_name": "ssh"
                }
            }),
            autonomy: Default::default(),
            message: None,
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.executor_used, "middleware_api");
        assert_eq!(response.result.data["route_fallback_used"], false);
        assert_eq!(response.result.data["preview"], true);

        let task_step = conversation_store
            .load_task_step("step-service-status")
            .expect("load task step")
            .expect("task step");
        assert_eq!(task_step.route, ExecutionRoute::MiddlewareApi);
        assert_eq!(task_step.executor_used, "middleware_api");
        assert_eq!(task_step.status, TaskStepRunStatus::Success);

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
        reset_harbor_task_api_env();
    }

    #[test]
    fn handle_service_status_falls_back_to_midcli_when_middleware_fails() {
        let _guard = HARBOROS_TASK_API_TEST_LOCK.lock().expect("lock");
        reset_harbor_task_api_env();
        std::env::set_var("HARBOR_FORCE_MIDDLEWARE_ERROR", "1");
        let (service, conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("harbor-service-fallback");
        let request = TaskRequest {
            task_id: "task-service-fallback".to_string(),
            trace_id: "trace-service-fallback".to_string(),
            step_id: "step-service-fallback".to_string(),
            source: TaskSource {
                channel: "feishu".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-service-fallback".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-service-fallback".to_string(),
                route_key: "gw_route_service_fallback".to_string(),
            },
            intent: TaskIntent {
                domain: "service".to_string(),
                action: "status".to_string(),
                raw_text: "查看 ssh 状态".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "service_name": "ssh"
            }),
            autonomy: Default::default(),
            message: None,
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.executor_used, "midcli");
        assert_eq!(response.result.data["route_fallback_used"], true);

        let task_step = conversation_store
            .load_task_step("step-service-fallback")
            .expect("load task step")
            .expect("task step");
        assert_eq!(task_step.route, ExecutionRoute::Midcli);
        assert_eq!(task_step.executor_used, "midcli");

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
        reset_harbor_task_api_env();
    }

    #[test]
    fn handle_service_restart_requires_approval_before_execution() {
        let _guard = HARBOROS_TASK_API_TEST_LOCK.lock().expect("lock");
        reset_harbor_task_api_env();
        let (service, conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("harbor-service-restart");
        let request = TaskRequest {
            task_id: "task-service-restart".to_string(),
            trace_id: "trace-service-restart".to_string(),
            step_id: "step-service-restart".to_string(),
            source: TaskSource {
                channel: "feishu".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-service-restart".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-service-restart".to_string(),
                route_key: "gw_route_service_restart".to_string(),
            },
            intent: TaskIntent {
                domain: "service".to_string(),
                action: "restart".to_string(),
                raw_text: "重启 ssh".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "service_name": "ssh"
            }),
            autonomy: Default::default(),
            message: None,
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::NeedsInput);
        assert_eq!(response.executor_used, "harboros_router");
        assert_eq!(response.missing_fields, vec!["approval_token".to_string()]);
        assert_eq!(
            response.result.data["approval_ticket"]["policy_ref"],
            "service.restart"
        );

        let approvals = conversation_store
            .approvals_for_task("task-service-restart")
            .expect("load approvals");
        assert_eq!(approvals.len(), 1);
        assert_eq!(approvals[0].status, ApprovalStatus::Pending);

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
        reset_harbor_task_api_env();
    }

    #[test]
    fn approve_pending_service_restart_executes_harboros_route() {
        let _guard = HARBOROS_TASK_API_TEST_LOCK.lock().expect("lock");
        reset_harbor_task_api_env();
        let (service, conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("harbor-service-restart-approve");
        let request = TaskRequest {
            task_id: "task-service-restart-approve".to_string(),
            trace_id: "trace-service-restart-approve".to_string(),
            step_id: "step-service-restart-approve".to_string(),
            source: TaskSource {
                channel: "feishu".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-service-restart-approve".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-service-restart-approve".to_string(),
                route_key: "gw_route_service_restart_approve".to_string(),
            },
            intent: TaskIntent {
                domain: "service".to_string(),
                action: "restart".to_string(),
                raw_text: "重启 ssh".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "service_name": "ssh"
            }),
            autonomy: Default::default(),
            message: None,
        };

        let initial = service.handle_task(request);
        let approval_id = initial.result.data["approval_ticket"]["approval_id"]
            .as_str()
            .expect("approval id")
            .to_string();

        let (approval, resumed) = service
            .approve_pending_approval(&approval_id, Some("approver-1".to_string()))
            .expect("approve");

        assert_eq!(approval.approval_ticket.status, ApprovalStatus::Approved);
        assert_eq!(resumed.status, TaskStatus::Completed);
        assert_eq!(resumed.executor_used, "middleware_api");
        assert_eq!(resumed.result.data["route_fallback_used"], false);
        assert!(!resumed.audit_ref.is_empty());

        let resume_step_id = format!("approval:{approval_id}:resume");
        let task_step = conversation_store
            .load_task_step(&resume_step_id)
            .expect("load task step")
            .expect("task step");
        assert_eq!(task_step.route, ExecutionRoute::MiddlewareApi);
        assert_eq!(task_step.executor_used, "middleware_api");

        let events = conversation_store
            .events_for_task("task-service-restart-approve")
            .expect("load events");
        assert!(events
            .iter()
            .any(|event| event.event_type == "task.approval_approved"));
        assert!(events
            .iter()
            .any(|event| event.event_type == "task.harboros_dispatched"));

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
        reset_harbor_task_api_env();
    }

    #[test]
    fn handle_files_list_dispatches_to_harboros_router() {
        let _guard = HARBOROS_TASK_API_TEST_LOCK.lock().expect("lock");
        reset_harbor_task_api_env();
        let (service, conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("harbor-files-list");
        let request = TaskRequest {
            task_id: "task-files-list".to_string(),
            trace_id: "trace-files-list".to_string(),
            step_id: "step-files-list".to_string(),
            source: TaskSource {
                channel: "feishu".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-files-list".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-files-list".to_string(),
                route_key: "gw_route_files_list".to_string(),
            },
            intent: TaskIntent {
                domain: "files".to_string(),
                action: "list".to_string(),
                raw_text: "列出 agent-ci".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "resource": {
                    "path": "/mnt/agent-ci"
                },
                "recursive": true
            }),
            autonomy: Default::default(),
            message: None,
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.executor_used, "middleware_api");
        assert_eq!(response.result.data["route_fallback_used"], false);

        let task_step = conversation_store
            .load_task_step("step-files-list")
            .expect("load task step")
            .expect("task step");
        assert_eq!(task_step.route, ExecutionRoute::MiddlewareApi);
        assert_eq!(task_step.executor_used, "middleware_api");

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
        reset_harbor_task_api_env();
    }

    #[test]
    fn handle_files_move_requires_approval_before_execution() {
        let _guard = HARBOROS_TASK_API_TEST_LOCK.lock().expect("lock");
        reset_harbor_task_api_env();
        let (service, conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("harbor-files-move");
        let request = TaskRequest {
            task_id: "task-files-move".to_string(),
            trace_id: "trace-files-move".to_string(),
            step_id: "step-files-move".to_string(),
            source: TaskSource {
                channel: "feishu".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-files-move".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-files-move".to_string(),
                route_key: "gw_route_files_move".to_string(),
            },
            intent: TaskIntent {
                domain: "files".to_string(),
                action: "move".to_string(),
                raw_text: "移动文件".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "source": "/mnt/agent-ci/inbox.txt",
                "target": "/mnt/agent-ci/archive/inbox.txt"
            }),
            autonomy: Default::default(),
            message: None,
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::NeedsInput);
        assert_eq!(response.executor_used, "harboros_router");
        assert_eq!(
            response.result.data["approval_ticket"]["policy_ref"],
            "files.move"
        );

        let approvals = conversation_store
            .approvals_for_task("task-files-move")
            .expect("load approvals");
        assert_eq!(approvals.len(), 1);
        assert_eq!(approvals[0].status, ApprovalStatus::Pending);

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
        reset_harbor_task_api_env();
    }

    #[test]
    fn handle_files_copy_denied_path_surfaces_router_failure_details() {
        let _guard = HARBOROS_TASK_API_TEST_LOCK.lock().expect("lock");
        reset_harbor_task_api_env();
        let (service, conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("harbor-files-copy-denied");
        let request = TaskRequest {
            task_id: "task-files-copy-denied".to_string(),
            trace_id: "trace-files-copy-denied".to_string(),
            step_id: "step-files-copy-denied".to_string(),
            source: TaskSource {
                channel: "feishu".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-files-copy-denied".to_string(),
                user_id: "user-1".to_string(),
                session_id: "sess-files-copy-denied".to_string(),
                route_key: "gw_route_files_copy_denied".to_string(),
            },
            intent: TaskIntent {
                domain: "files".to_string(),
                action: "copy".to_string(),
                raw_text: "复制 passwd".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "source": "/etc/passwd",
                "target": "/mnt/agent-ci/out.txt"
            }),
            autonomy: Default::default(),
            message: None,
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Failed);
        assert_eq!(response.executor_used, "none");
        assert_eq!(response.result.data["error_code"], "NO_EXECUTOR_AVAILABLE");
        assert!(response.result.message.contains("denied path"));

        let task_step = conversation_store
            .load_task_step("step-files-copy-denied")
            .expect("load task step")
            .expect("task step");
        assert_eq!(task_step.route, ExecutionRoute::Local);
        assert_eq!(
            task_step.error_code.as_deref(),
            Some("NO_EXECUTOR_AVAILABLE")
        );
        assert!(task_step
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("denied path"));

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
        reset_harbor_task_api_env();
    }

    #[test]
    fn delivery_hints_add_native_image_for_image_artifacts() {
        let response = TaskResponse {
            task_id: "task-image-hint".to_string(),
            trace_id: "trace-image-hint".to_string(),
            status: TaskStatus::Completed,
            executor_used: "knowledge_search_service".to_string(),
            risk_level: RiskLevel::Low,
            result: TaskResultEnvelope {
                message: "found images".to_string(),
                data: json!({}),
                artifacts: vec![
                    TaskArtifact {
                        kind: "image".to_string(),
                        label: "spring one".to_string(),
                        mime_type: "image/jpeg".to_string(),
                        media_asset_id: Some("artifact-image-1".to_string()),
                        path: Some("/mnt/photos/spring-1.jpg".to_string()),
                        url: None,
                        metadata: json!({}),
                    },
                    TaskArtifact {
                        kind: "image".to_string(),
                        label: "spring two".to_string(),
                        mime_type: "image/jpeg".to_string(),
                        media_asset_id: Some("artifact-image-2".to_string()),
                        path: Some("/mnt/photos/spring-2.jpg".to_string()),
                        url: None,
                        metadata: json!({}),
                    },
                ],
                events: Vec::new(),
                next_actions: Vec::new(),
            },
            audit_ref: "audit-image-hint".to_string(),
            missing_fields: Vec::new(),
            prompt: None,
            resume_token: None,
        };

        let hints = delivery_hints_from_task_response(&response);

        assert_eq!(hints.len(), 1);
        assert_eq!(hints[0].kind, "native_image");
        assert_eq!(hints[0].artifact_id.as_deref(), Some("artifact-image-1"));
        assert_eq!(hints[0].fallback.as_deref(), Some("text"));
        assert_eq!(hints[0].metadata["max_items"], 3);
        assert_eq!(hints[0].metadata["artifact_count"], 2);
        assert_eq!(hints[0].metadata["paths"][0], "/mnt/photos/spring-1.jpg");
    }

    #[test]
    fn handle_knowledge_search_returns_document_and_image_hits() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let knowledge_root = unique_dir("harborbeacon-knowledge-runtime");
        let index_root = unique_dir("harborbeacon-knowledge-index-runtime");
        fs::create_dir_all(knowledge_root.join("docs")).expect("create docs");
        fs::create_dir_all(knowledge_root.join("images")).expect("create images");
        fs::write(
            knowledge_root.join("docs").join("sakura-notes.md"),
            "樱花季整理计划，记录花园图片和说明。",
        )
        .expect("write doc");
        fs::write(
            knowledge_root.join("images").join("spring-garden.jpg"),
            b"not-an-image",
        )
        .expect("write image");
        fs::write(
            knowledge_root.join("images").join("spring-garden.json"),
            r#"{"caption":"春天盛开的樱花树"}"#,
        )
        .expect("write sidecar");

        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        configure_knowledge_source(&admin_store, &knowledge_root, &index_root);
        let service = TaskApiService::new(
            admin_store,
            TaskConversationStore::new(conversation_path.clone()),
        );
        let request = TaskRequest {
            task_id: "task-knowledge-search".to_string(),
            trace_id: "trace-knowledge-search".to_string(),
            step_id: "step-knowledge-search".to_string(),
            source: TaskSource::default(),
            intent: TaskIntent {
                domain: "knowledge".to_string(),
                action: "search".to_string(),
                raw_text: "搜索樱花文件".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "query": "樱花",
                "roots": [knowledge_root.to_string_lossy().to_string()]
            }),
            autonomy: Default::default(),
            message: None,
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.executor_used, "knowledge_search_service");
        assert_eq!(
            response.result.message,
            response.result.data["reply_pack"]["summary"]
        );
        assert_eq!(
            response.result.data["documents"].as_array().map(Vec::len),
            Some(1)
        );
        assert_eq!(
            response.result.data["images"].as_array().map(Vec::len),
            Some(1)
        );
        assert_eq!(
            response.result.data["reply_pack"]["citations"]
                .as_array()
                .map(Vec::len),
            Some(2)
        );
        assert_eq!(
            response.result.data["reply_pack"]["citations"][0]["title"],
            "sakura-notes.md"
        );
        assert!(
            response.result.data["reply_pack"]["citations"][0]["preview"]
                .as_str()
                .unwrap_or_default()
                .contains("樱花")
        );
        assert_eq!(response.result.artifacts.len(), 2);
        assert_eq!(response.result.artifacts[0].kind, "text");
        assert_eq!(response.result.artifacts[1].kind, "image");

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
        let _ = fs::remove_dir_all(knowledge_root);
        let _ = fs::remove_dir_all(index_root);
    }

    #[test]
    fn handle_general_message_video_follow_up_focuses_first_video_hit() {
        let _guard = RETRIEVAL_GATE_TEST_LOCK.lock().expect("lock");
        let (service, _conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("harborbeacon-video-follow-up");
        let knowledge_root = unique_dir("harborbeacon-knowledge-video-follow-up");
        let index_root = unique_dir("harborbeacon-knowledge-index-video-follow-up");
        let video_dir = knowledge_root.join("videos");
        fs::create_dir_all(&video_dir).expect("create videos");
        let first_video = video_dir.join("a-porch.mp4");
        let second_video = video_dir.join("b-garage.mp4");
        fs::write(&first_video, b"fake-video-a").expect("write first video");
        fs::write(&second_video, b"fake-video-b").expect("write second video");
        fs::write(
            video_dir.join("a-porch.json"),
            r#"{"summary":"门口摄像头看到快递箱放在门边","start_time":"00:00:05","end_time":"00:00:12"}"#,
        )
        .expect("write first sidecar");
        fs::write(
            video_dir.join("b-garage.json"),
            r#"{"summary":"车库摄像头看到快递箱放在架子旁","start_time":"00:00:15","end_time":"00:00:22"}"#,
        )
        .expect("write second sidecar");
        configure_knowledge_source(&service.clone().admin_store, &knowledge_root, &index_root);

        let first_request =
            general_message_test_request("video-follow-up-1", "帮我找视频里快递箱", json!({}));
        let source = first_request.source.clone();
        let first_response = service.handle_task(first_request);
        let top_video_path = first_response
            .result
            .artifacts
            .iter()
            .find(|artifact| artifact.kind == "video")
            .and_then(|artifact| artifact.path.clone())
            .expect("top video artifact");
        assert_eq!(first_response.status, TaskStatus::Completed);
        assert_eq!(
            first_response.result.data["videos"]
                .as_array()
                .map(Vec::len),
            Some(2)
        );

        let mut second_request =
            general_message_test_request("video-follow-up-2", "只看第一个视频里的结果", json!({}));
        second_request.source = source;
        let second_response = service.handle_task(second_request);

        assert_eq!(second_response.status, TaskStatus::Completed);
        assert_eq!(second_response.result.data["query"], "快递箱");
        assert_eq!(
            second_response.result.data["videos"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(
            second_response.result.data["videos"][0]["path"],
            top_video_path
        );
        assert_eq!(
            second_response.result.artifacts[0].path.as_deref(),
            Some(top_video_path.as_str())
        );

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
        let _ = fs::remove_dir_all(knowledge_root);
        let _ = fs::remove_dir_all(index_root);
    }

    #[test]
    fn handle_knowledge_search_rejects_unconfigured_root_expansion() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let knowledge_root = unique_dir("harborbeacon-knowledge-configured");
        let outside_root = unique_dir("harborbeacon-knowledge-outside");
        let index_root = unique_dir("harborbeacon-knowledge-index-configured");
        fs::create_dir_all(&knowledge_root).expect("create configured root");
        fs::create_dir_all(&outside_root).expect("create outside root");

        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        configure_knowledge_source(&admin_store, &knowledge_root, &index_root);
        let service = TaskApiService::new(
            admin_store,
            TaskConversationStore::new(conversation_path.clone()),
        );
        let request = TaskRequest {
            task_id: "task-knowledge-denied-root".to_string(),
            trace_id: "trace-knowledge-denied-root".to_string(),
            step_id: "step-knowledge-denied-root".to_string(),
            source: TaskSource::default(),
            intent: TaskIntent {
                domain: "knowledge".to_string(),
                action: "search".to_string(),
                raw_text: "搜索外部文件".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "query": "anything",
                "roots": [outside_root.to_string_lossy().to_string()]
            }),
            autonomy: Default::default(),
            message: None,
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Failed);
        assert_eq!(response.executor_used, "knowledge_search_service");
        assert!(response
            .result
            .message
            .contains("未在 Harbor Assistant 启用"));
        assert_eq!(response.result.data["status"], "degraded");
        assert_eq!(response.result.data["degraded"], true);
        assert_eq!(
            response.result.data["degraded_reason"],
            "source_scope_blocked"
        );

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
        let _ = fs::remove_dir_all(knowledge_root);
        let _ = fs::remove_dir_all(outside_root);
        let _ = fs::remove_dir_all(index_root);
    }

    #[test]
    fn handle_knowledge_search_blocks_privacy_policy_escalation() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let knowledge_root = unique_dir("harborbeacon-knowledge-privacy-policy");
        let index_root = unique_dir("harborbeacon-knowledge-index-privacy-policy");
        fs::create_dir_all(&knowledge_root).expect("create knowledge root");

        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        configure_knowledge_source(&admin_store, &knowledge_root, &index_root);
        let service = TaskApiService::new(
            admin_store,
            TaskConversationStore::new(conversation_path.clone()),
        );
        let request = TaskRequest {
            task_id: "task-knowledge-privacy-policy".to_string(),
            trace_id: "trace-knowledge-privacy-policy".to_string(),
            step_id: "step-knowledge-privacy-policy".to_string(),
            source: TaskSource::default(),
            intent: TaskIntent {
                domain: "knowledge".to_string(),
                action: "search".to_string(),
                raw_text: "搜索文档".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "query": "anything",
                "privacy_level": "allow_cloud"
            }),
            autonomy: Default::default(),
            message: None,
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Failed);
        assert_eq!(response.result.data["status"], "degraded");
        assert_eq!(
            response.result.data["degraded_reason"],
            "privacy_policy_blocked"
        );
        assert_eq!(response.result.data["privacy_level"], "strict_local");
        assert!(response.result.message.contains("超出 workspace 当前策略"));

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
        let _ = fs::remove_dir_all(knowledge_root);
        let _ = fs::remove_dir_all(index_root);
    }

    #[test]
    fn handle_knowledge_search_blocks_cloud_profile_under_strict_local() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let knowledge_root = unique_dir("harborbeacon-knowledge-cloud-profile");
        let index_root = unique_dir("harborbeacon-knowledge-index-cloud-profile");
        fs::create_dir_all(&knowledge_root).expect("create knowledge root");

        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        configure_knowledge_source(&admin_store, &knowledge_root, &index_root);
        let service = TaskApiService::new(
            admin_store,
            TaskConversationStore::new(conversation_path.clone()),
        );
        let request = TaskRequest {
            task_id: "task-knowledge-cloud-profile".to_string(),
            trace_id: "trace-knowledge-cloud-profile".to_string(),
            step_id: "step-knowledge-cloud-profile".to_string(),
            source: TaskSource::default(),
            intent: TaskIntent {
                domain: "knowledge".to_string(),
                action: "search".to_string(),
                raw_text: "搜索文档".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "query": "anything",
                "resource_profile": "cloud_allowed"
            }),
            autonomy: Default::default(),
            message: None,
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Failed);
        assert_eq!(response.result.data["status"], "degraded");
        assert_eq!(
            response.result.data["degraded_reason"],
            "blocked_resource_profile"
        );
        assert_eq!(response.result.data["privacy_level"], "strict_local");
        assert_eq!(response.result.data["resource_profile"], "cloud_allowed");
        assert!(response.result.message.contains("strict_local"));

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
        let _ = fs::remove_dir_all(knowledge_root);
        let _ = fs::remove_dir_all(index_root);
    }

    #[test]
    fn handle_rag_answer_returns_cited_answer_from_search_context() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let knowledge_root = unique_dir("harborbeacon-rag-answer-root");
        let index_root = unique_dir("harborbeacon-rag-answer-index");
        fs::create_dir_all(knowledge_root.join("docs")).expect("create docs");
        fs::write(
            knowledge_root.join("docs").join("sakura-plan.md"),
            "樱花计划是春季花园归档安排，包含拍照、说明和后续分享。",
        )
        .expect("write doc");

        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        configure_knowledge_source(&admin_store, &knowledge_root, &index_root);
        let service = TaskApiService::new(
            admin_store,
            TaskConversationStore::new(conversation_path.clone()),
        );
        configure_mock_general_message_llm(&service, "樱花计划是春季花园归档安排。[1]");
        let request = TaskRequest {
            task_id: "task-rag-answer".to_string(),
            trace_id: "trace-rag-answer".to_string(),
            step_id: "step-rag-answer".to_string(),
            source: TaskSource::default(),
            intent: TaskIntent {
                domain: "rag".to_string(),
                action: "answer".to_string(),
                raw_text: "樱花计划是什么".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "query": "樱花计划",
                "roots": [knowledge_root.to_string_lossy().to_string()]
            }),
            autonomy: Default::default(),
            message: None,
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.executor_used, "rag_answer_service");
        assert_eq!(response.result.data["kind"], "rag.answer");
        assert_eq!(
            response.result.data["answer_citation_policy"],
            "cited_context_only"
        );
        assert!(response.result.data["answer"]
            .as_str()
            .unwrap_or_default()
            .contains("[1]"));
        assert_eq!(response.result.data["model"]["available"], true);
        assert_eq!(
            response.result.data["citations"].as_array().map(Vec::len),
            Some(1)
        );
        assert_eq!(response.result.artifacts.len(), 1);

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
        let _ = fs::remove_dir_all(knowledge_root);
        let _ = fs::remove_dir_all(index_root);
    }

    #[test]
    fn handle_rag_answer_refuses_when_evidence_is_weak() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let knowledge_root = unique_dir("harborbeacon-rag-answer-weak-root");
        let index_root = unique_dir("harborbeacon-rag-answer-weak-index");
        fs::create_dir_all(knowledge_root.join("docs")).expect("create docs");
        fs::write(
            knowledge_root.join("docs").join("sakura-plan.md"),
            "樱花计划是春季花园归档安排。",
        )
        .expect("write doc");

        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        configure_knowledge_source(&admin_store, &knowledge_root, &index_root);
        let service = TaskApiService::new(
            admin_store,
            TaskConversationStore::new(conversation_path.clone()),
        );
        let request = TaskRequest {
            task_id: "task-rag-answer-weak".to_string(),
            trace_id: "trace-rag-answer-weak".to_string(),
            step_id: "step-rag-answer-weak".to_string(),
            source: TaskSource::default(),
            intent: TaskIntent {
                domain: "rag".to_string(),
                action: "answer".to_string(),
                raw_text: "车库门禁密码是什么".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "query": "车库门禁密码",
                "roots": [knowledge_root.to_string_lossy().to_string()]
            }),
            autonomy: Default::default(),
            message: None,
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.executor_used, "rag_answer_service");
        assert_eq!(response.result.data["status"], "degraded");
        assert_eq!(response.result.data["degraded"], true);
        assert_eq!(response.result.data["degraded_reason"], "weak_evidence");
        assert_eq!(
            response.result.data["citations"].as_array().map(Vec::len),
            Some(0)
        );
        assert!(response.result.message.contains("没有找到足够证据"));

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
        let _ = fs::remove_dir_all(knowledge_root);
        let _ = fs::remove_dir_all(index_root);
    }

    #[test]
    fn handle_rag_answer_blocks_cloud_profile_under_strict_local() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let knowledge_root = unique_dir("harborbeacon-rag-answer-cloud-root");
        let index_root = unique_dir("harborbeacon-rag-answer-cloud-index");
        fs::create_dir_all(&knowledge_root).expect("create knowledge root");

        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        configure_knowledge_source(&admin_store, &knowledge_root, &index_root);
        let service = TaskApiService::new(
            admin_store,
            TaskConversationStore::new(conversation_path.clone()),
        );
        let request = TaskRequest {
            task_id: "task-rag-answer-cloud".to_string(),
            trace_id: "trace-rag-answer-cloud".to_string(),
            step_id: "step-rag-answer-cloud".to_string(),
            source: TaskSource::default(),
            intent: TaskIntent {
                domain: "rag".to_string(),
                action: "answer".to_string(),
                raw_text: "总结资料".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "query": "anything",
                "resource_profile": "cloud_allowed"
            }),
            autonomy: Default::default(),
            message: None,
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Failed);
        assert_eq!(response.executor_used, "rag_answer_service");
        assert_eq!(response.result.data["kind"], "rag.answer");
        assert_eq!(
            response.result.data["degraded_reason"],
            "blocked_resource_profile"
        );
        assert_eq!(response.result.data["privacy_level"], "strict_local");
        assert_eq!(response.result.data["resource_profile"], "cloud_allowed");

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
        let _ = fs::remove_dir_all(knowledge_root);
        let _ = fs::remove_dir_all(index_root);
    }

    #[test]
    fn handle_rag_answer_redacted_cloud_uses_privacy_capsule() {
        let _guard = RETRIEVAL_GATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let admin_path = unique_path("harborbeacon-admin-state");
        let previous_admin_state_path = std::env::var_os("HARBOR_ADMIN_STATE_PATH");
        std::env::set_var("HARBOR_ADMIN_STATE_PATH", &admin_path);
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let knowledge_root = unique_dir("harborbeacon-rag-answer-redacted-cloud-root");
        let index_root = unique_dir("harborbeacon-rag-answer-redacted-cloud-index");
        fs::create_dir_all(knowledge_root.join("docs")).expect("create docs");
        fs::write(
            knowledge_root.join("docs").join("family-plan.md"),
            "The family garden plan includes photos, captions, and a sharing checklist.",
        )
        .expect("write doc");

        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        configure_knowledge_source_with_policy(
            &admin_store,
            &knowledge_root,
            &index_root,
            PrivacyLevel::AllowRedactedCloud,
            RagResourceProfile::CloudAllowed,
        );
        let service = TaskApiService::new(
            admin_store,
            TaskConversationStore::new(conversation_path.clone()),
        );
        configure_mock_embedding(&service);
        configure_mock_cloud_llm(&service, "The plan includes photos and captions. [1]");
        let request = TaskRequest {
            task_id: "task-rag-answer-redacted-cloud".to_string(),
            trace_id: "trace-rag-answer-redacted-cloud".to_string(),
            step_id: "step-rag-answer-redacted-cloud".to_string(),
            source: TaskSource {
                user_id: "owner-1".to_string(),
                ..Default::default()
            },
            intent: TaskIntent {
                domain: "rag".to_string(),
                action: "answer".to_string(),
                raw_text: "summarize the family plan".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "query": "family garden plan",
                "roots": [knowledge_root.to_string_lossy().to_string()],
                "privacy_level": "allow_redacted_cloud",
                "resource_profile": "cloud_allowed"
            }),
            autonomy: Default::default(),
            message: None,
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(
            response.result.data["privacy_gateway"]["decision"],
            "allow_redacted_cloud"
        );
        assert_eq!(
            response.result.data["privacy_gateway"]["capsule_prompt_used"],
            true
        );
        assert_eq!(
            response.result.data["model"]["selected_endpoint_kind"],
            "cloud"
        );
        assert_eq!(
            response.result.data["privacy_gateway"]["capsule_fact_count"],
            1
        );
        let audits = service.admin_store.audit_records().expect("audit records");
        assert!(audits
            .iter()
            .any(|record| record.action == "privacy_gateway.rag_answer.evaluate"));

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
        let _ = fs::remove_dir_all(knowledge_root);
        let _ = fs::remove_dir_all(index_root);
        restore_env_var("HARBOR_ADMIN_STATE_PATH", previous_admin_state_path);
    }

    #[test]
    fn handle_rag_answer_redacted_cloud_blocks_credential_capsule() {
        let _guard = RETRIEVAL_GATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let admin_path = unique_path("harborbeacon-admin-state");
        let previous_admin_state_path = std::env::var_os("HARBOR_ADMIN_STATE_PATH");
        std::env::set_var("HARBOR_ADMIN_STATE_PATH", &admin_path);
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let knowledge_root = unique_dir("harborbeacon-rag-answer-credential-root");
        let index_root = unique_dir("harborbeacon-rag-answer-credential-index");
        fs::create_dir_all(knowledge_root.join("docs")).expect("create docs");
        fs::write(
            knowledge_root.join("docs").join("garage-secret.md"),
            "The garage integration api_key=secret-123 should never leave the home.",
        )
        .expect("write doc");

        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        configure_knowledge_source_with_policy(
            &admin_store,
            &knowledge_root,
            &index_root,
            PrivacyLevel::AllowRedactedCloud,
            RagResourceProfile::CloudAllowed,
        );
        let service = TaskApiService::new(
            admin_store,
            TaskConversationStore::new(conversation_path.clone()),
        );
        configure_mock_embedding(&service);
        configure_mock_cloud_llm(&service, "this cloud text should not be used [1]");
        let request = TaskRequest {
            task_id: "task-rag-answer-credential".to_string(),
            trace_id: "trace-rag-answer-credential".to_string(),
            step_id: "step-rag-answer-credential".to_string(),
            source: TaskSource::default(),
            intent: TaskIntent {
                domain: "rag".to_string(),
                action: "answer".to_string(),
                raw_text: "what is the garage api key".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "query": "garage api key",
                "roots": [knowledge_root.to_string_lossy().to_string()],
                "privacy_level": "allow_redacted_cloud",
                "resource_profile": "cloud_allowed"
            }),
            autonomy: Default::default(),
            message: None,
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.result.data["status"], "degraded");
        assert_eq!(
            response.result.data["degraded_reason"],
            "privacy_gateway_blocked"
        );
        assert_eq!(
            response.result.data["privacy_gateway"]["decision"],
            "blocked_or_degraded"
        );
        assert_eq!(
            response.result.data["privacy_gateway"]["cloud_allowed"],
            false
        );
        assert_eq!(response.result.data["model"], Value::Null);
        assert!(response.result.message.contains("Privacy Gateway"));

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
        let _ = fs::remove_dir_all(knowledge_root);
        let _ = fs::remove_dir_all(index_root);
        restore_env_var("HARBOR_ADMIN_STATE_PATH", previous_admin_state_path);
    }

    #[test]
    fn handle_rag_answer_allow_cloud_records_privacy_gateway_evidence() {
        let _guard = RETRIEVAL_GATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let admin_path = unique_path("harborbeacon-admin-state");
        let previous_admin_state_path = std::env::var_os("HARBOR_ADMIN_STATE_PATH");
        std::env::set_var("HARBOR_ADMIN_STATE_PATH", &admin_path);
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let knowledge_root = unique_dir("harborbeacon-rag-answer-allow-cloud-root");
        let index_root = unique_dir("harborbeacon-rag-answer-allow-cloud-index");
        fs::create_dir_all(knowledge_root.join("docs")).expect("create docs");
        fs::write(
            knowledge_root.join("docs").join("device-note.md"),
            "The device note says Harbor can answer from controlled cloud fallback.",
        )
        .expect("write doc");

        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        configure_knowledge_source_with_policy(
            &admin_store,
            &knowledge_root,
            &index_root,
            PrivacyLevel::AllowCloud,
            RagResourceProfile::CloudAllowed,
        );
        let service = TaskApiService::new(
            admin_store,
            TaskConversationStore::new(conversation_path.clone()),
        );
        configure_mock_embedding(&service);
        configure_mock_cloud_llm(
            &service,
            "Harbor can answer from controlled cloud fallback. [1]",
        );
        let request = TaskRequest {
            task_id: "task-rag-answer-allow-cloud".to_string(),
            trace_id: "trace-rag-answer-allow-cloud".to_string(),
            step_id: "step-rag-answer-allow-cloud".to_string(),
            source: TaskSource::default(),
            intent: TaskIntent {
                domain: "rag".to_string(),
                action: "answer".to_string(),
                raw_text: "what does the device note say".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "query": "controlled cloud fallback",
                "roots": [knowledge_root.to_string_lossy().to_string()],
                "privacy_level": "allow_cloud",
                "resource_profile": "cloud_allowed"
            }),
            autonomy: Default::default(),
            message: None,
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(
            response.result.data["privacy_gateway"]["decision"],
            "allow_cloud"
        );
        assert_eq!(
            response.result.data["privacy_gateway"]["capsule_prompt_used"],
            false
        );
        assert_eq!(
            response.result.data["model"]["selected_endpoint_kind"],
            "cloud"
        );
        assert_eq!(
            response.result.data["degraded"], false,
            "{}",
            response.result.data
        );

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
        let _ = fs::remove_dir_all(knowledge_root);
        let _ = fs::remove_dir_all(index_root);
        restore_env_var("HARBOR_ADMIN_STATE_PATH", previous_admin_state_path);
    }

    #[test]
    fn general_message_routes_retrieval_query_to_knowledge_search() {
        let _guard = RETRIEVAL_GATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let knowledge_root = unique_dir("harborbeacon-knowledge-general-message");
        let index_root = unique_dir("harborbeacon-knowledge-index-general-message");
        fs::create_dir_all(knowledge_root.join("docs")).expect("create docs");
        fs::write(
            knowledge_root.join("docs").join("sakura-journal.md"),
            "我把樱花相关的文档放在这里，方便后续整理。",
        )
        .expect("write doc");

        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        configure_knowledge_source(&admin_store, &knowledge_root, &index_root);
        let service = TaskApiService::new(
            admin_store,
            TaskConversationStore::new(conversation_path.clone()),
        );
        let request = TaskRequest {
            task_id: "task-general-message-search".to_string(),
            trace_id: "trace-general-message-search".to_string(),
            step_id: "step-general-message-search".to_string(),
            source: TaskSource {
                channel: "wechat".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-search".to_string(),
                user_id: "user-1".to_string(),
                session_id: "session-search".to_string(),
                route_key: "gw_route_search".to_string(),
            },
            intent: TaskIntent {
                domain: "general".to_string(),
                action: "message".to_string(),
                raw_text: "帮我找到和樱花有关的文件".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "roots": [knowledge_root.to_string_lossy().to_string()]
            }),
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: "om_knowledge_01".to_string(),
                chat_type: "p2p".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.executor_used, "knowledge_search_service");
        assert_eq!(
            response.result.message,
            response.result.data["reply_pack"]["summary"]
        );
        assert_eq!(
            response.result.data["reply_pack"]["citations"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(response.result.artifacts.len(), 1);
        assert_eq!(
            response.result.artifacts[0].metadata["citation"]["title"],
            "sakura-journal.md"
        );
        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
        let _ = fs::remove_dir_all(knowledge_root);
        let _ = fs::remove_dir_all(index_root);
    }

    #[test]
    fn general_message_search_uses_configured_roots_when_roots_are_omitted() {
        let _guard = RETRIEVAL_GATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let knowledge_root = unique_dir("harborbeacon-knowledge-general-message-default-root");
        let index_root = unique_dir("harborbeacon-knowledge-index-general-message-default-root");
        fs::create_dir_all(knowledge_root.join("docs")).expect("create docs");
        fs::write(
            knowledge_root.join("docs").join("spring-photo-note.md"),
            "春天照片测试：花树、粉色花朵和公园场景。",
        )
        .expect("write doc");

        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        configure_knowledge_source(&admin_store, &knowledge_root, &index_root);
        let service = TaskApiService::new(
            admin_store,
            TaskConversationStore::new(conversation_path.clone()),
        );
        let request = TaskRequest {
            task_id: "task-general-message-search-default-roots".to_string(),
            trace_id: "trace-general-message-search-default-roots".to_string(),
            step_id: "step-general-message-search-default-roots".to_string(),
            source: TaskSource {
                channel: "wechat".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-search".to_string(),
                user_id: "user-1".to_string(),
                session_id: "session-search".to_string(),
                route_key: "gw_route_search".to_string(),
            },
            intent: TaskIntent {
                domain: "general".to_string(),
                action: "message".to_string(),
                raw_text: "找到和春天相关的内容".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({}),
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: "om_knowledge_02".to_string(),
                chat_type: "p2p".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.executor_used, "knowledge_search_service");
        assert_eq!(
            response.result.data["source_scope"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        let expected_scope = knowledge_root
            .canonicalize()
            .unwrap_or_else(|_| knowledge_root.clone())
            .to_string_lossy()
            .into_owned();
        let actual_scope = response.result.data["source_scope"][0]
            .as_str()
            .expect("source scope path");
        assert_eq!(
            actual_scope.strip_prefix("\\\\?\\").unwrap_or(actual_scope),
            expected_scope
                .strip_prefix("\\\\?\\")
                .unwrap_or(expected_scope.as_str())
        );
        assert!(response.result.message.contains("春天"));
        assert_ne!(
            response.result.data["degraded_reason"].as_str(),
            Some("source_scope_blocked")
        );

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
        let _ = fs::remove_dir_all(knowledge_root);
        let _ = fs::remove_dir_all(index_root);
    }

    #[test]
    fn knowledge_search_modalities_follow_photo_language_after_general_routing() {
        let request = TaskRequest {
            task_id: "task-photo-modality".to_string(),
            trace_id: "trace-photo-modality".to_string(),
            step_id: "step-photo-modality".to_string(),
            source: TaskSource::default(),
            intent: TaskIntent {
                domain: KNOWLEDGE_DOMAIN.to_string(),
                action: KNOWLEDGE_OP_SEARCH.to_string(),
                raw_text: "找到和春天相关的照片".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({}),
            autonomy: Default::default(),
            message: None,
        };

        assert_eq!(knowledge_modalities(&request), (false, true, false));
    }

    #[test]
    fn knowledge_search_modalities_follow_video_language() {
        let request = TaskRequest {
            task_id: "task-video-modality".to_string(),
            trace_id: "trace-video-modality".to_string(),
            step_id: "step-video-modality".to_string(),
            source: TaskSource::default(),
            intent: TaskIntent {
                domain: KNOWLEDGE_DOMAIN.to_string(),
                action: KNOWLEDGE_OP_SEARCH.to_string(),
                raw_text: "找一下视频里出现快递箱的片段".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({}),
            autonomy: Default::default(),
            message: None,
        };

        assert_eq!(knowledge_modalities(&request), (false, false, true));

        let exclude_non_video_request = TaskRequest {
            intent: TaskIntent {
                raw_text: "排除图片和文档，只看视频结果".to_string(),
                ..request.intent.clone()
            },
            ..request
        };
        assert_eq!(
            knowledge_modalities(&exclude_non_video_request),
            (false, false, true)
        );
    }

    #[test]
    fn general_message_routes_knowledge_question_to_rag_answer() {
        let _guard = RETRIEVAL_GATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (service, _conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("general-message-rag-answer");
        let knowledge_root = unique_dir("harborbeacon-knowledge-general-rag-answer");
        let index_root = unique_dir("harborbeacon-knowledge-index-general-rag-answer");
        fs::create_dir_all(knowledge_root.join("docs")).expect("create docs");
        fs::write(
            knowledge_root.join("docs").join("sakura-plan.md"),
            "樱花计划是春季花园归档安排，包含拍照、说明和后续分享。",
        )
        .expect("write doc");
        configure_knowledge_source(&service.admin_store, &knowledge_root, &index_root);
        configure_mock_general_message_llm(&service, "樱花计划是春季花园归档安排。[1]");

        let request = general_message_test_request(
            "general-message-rag-answer",
            "根据资料回答樱花计划是什么",
            json!({
                "roots": [knowledge_root.to_string_lossy().to_string()]
            }),
        );

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.executor_used, "rag_answer_service");
        assert_eq!(response.result.data["kind"], "rag.answer");
        assert!(response.result.data["answer"]
            .as_str()
            .unwrap_or_default()
            .contains("[1]"));
        assert_eq!(
            response.result.data["citations"].as_array().map(Vec::len),
            Some(1)
        );
        assert_eq!(
            response.result.data["general_message_controller"]["controller_stage"],
            "deterministic_single_candidate"
        );
        assert_eq!(
            response.result.data["general_message_controller"]["router_llm"],
            true
        );
        assert_eq!(
            response.result.data["general_message_controller"]["nsp_route"]["local_only"],
            true
        );

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
        let _ = fs::remove_dir_all(knowledge_root);
        let _ = fs::remove_dir_all(index_root);
    }

    #[test]
    fn general_message_capability_queries_are_detected_without_shadowing_commands() {
        assert!(general_message_requests_capability_summary("你能做什么"));
        assert!(general_message_requests_capability_summary("你还能做什么"));
        assert!(general_message_requests_capability_summary("帮助"));
        assert!(general_message_requests_capability_summary(
            "摄像头能干什么"
        ));
        assert!(general_message_requests_capability_summary(
            "解释一下 HarborBeacon 和 HarborGate 现在的 local-first 架构，以及云端 fallback 是怎么受控的"
        ));
        assert!(!general_message_requests_capability_summary(
            "帮助我抓拍一下当前摄像头画面"
        ));
        assert!(!general_message_requests_capability_summary(
            "Harbor Assistant Search 怎么工作"
        ));

        assert_eq!(
            fallback_general_message_plan("摄像头能干什么", None)
                .expect("capability summary plan")
                .kind,
            GeneralMessagePlanKind::CapabilitySummary
        );
        assert_eq!(
            fallback_general_message_plan(
                "解释一下 HarborBeacon 和 HarborGate 现在的 local-first 架构",
                None,
            )
            .expect("local-first architecture plan")
            .kind,
            GeneralMessagePlanKind::CapabilitySummary
        );
        assert_eq!(
            fallback_general_message_plan("帮我抓拍一下当前摄像头画面", None)
                .expect("snapshot plan")
                .kind,
            GeneralMessagePlanKind::CameraSnapshot
        );
        assert_eq!(
            fallback_general_message_plan("帮我录一段门口摄像头", None)
                .expect("clip plan")
                .kind,
            GeneralMessagePlanKind::CameraRecordClip
        );
        assert_eq!(
            fallback_general_message_plan("最近事件", None)
                .expect("event summary plan")
                .kind,
            GeneralMessagePlanKind::VisionEventSummary
        );
        assert_eq!(
            fallback_general_message_plan("通知最新事件", None)
                .expect("event notify plan")
                .kind,
            GeneralMessagePlanKind::VisionEventNotifyLatest
        );
        assert_eq!(
            fallback_general_message_plan("状态", None)
                .expect("system readiness plan")
                .kind,
            GeneralMessagePlanKind::SystemReadiness
        );
        assert_eq!(
            fallback_general_message_plan("开灯", None)
                .expect("HA action plan")
                .kind,
            GeneralMessagePlanKind::HomeAssistantServiceAction
        );
        let video_search_plan = fallback_general_message_plan("帮我找录像里出现快递箱的片段", None)
            .expect("video search plan");
        assert_eq!(
            video_search_plan.kind,
            GeneralMessagePlanKind::KnowledgeSearch
        );
        assert_eq!(video_search_plan.query.as_deref(), Some("快递箱"));
        assert_eq!(
            fallback_general_message_plan("非常好，帮我录一段", None)
                .expect("clip plan")
                .kind,
            GeneralMessagePlanKind::CameraRecordClip
        );
        assert_eq!(
            fallback_general_message_plan("帮我找到和樱花有关的文件", None)
                .expect("search plan")
                .kind,
            GeneralMessagePlanKind::KnowledgeSearch
        );
        let rag_plan = fallback_general_message_plan("根据资料回答樱花计划是什么", None)
            .expect("rag answer plan");
        assert_eq!(rag_plan.kind, GeneralMessagePlanKind::RagAnswer);
        assert_eq!(rag_plan.query.as_deref(), Some("樱花计划"));
    }

    #[test]
    fn general_message_home_assistant_short_phrases_are_closed_and_low_risk() {
        let light = infer_home_assistant_natural_action("开灯").expect("light action");
        assert_eq!(
            light,
            HomeAssistantNaturalAction::Request(HomeAssistantNaturalActionRequest {
                domain: "light".to_string(),
                service: "turn_on".to_string(),
                entity_hint: None,
            })
        );
        let switch = infer_home_assistant_natural_action("切换开关").expect("switch action");
        assert_eq!(
            switch,
            HomeAssistantNaturalAction::Request(HomeAssistantNaturalActionRequest {
                domain: "switch".to_string(),
                service: "toggle".to_string(),
                entity_hint: None,
            })
        );
        let scene = infer_home_assistant_natural_action("执行测试场景").expect("scene action");
        assert_eq!(
            scene,
            HomeAssistantNaturalAction::Request(HomeAssistantNaturalActionRequest {
                domain: "scene".to_string(),
                service: "turn_on".to_string(),
                entity_hint: Some("测试".to_string()),
            })
        );
        assert!(matches!(
            infer_home_assistant_natural_action("解锁门锁"),
            Some(HomeAssistantNaturalAction::Blocked { .. })
        ));
        assert!(infer_home_assistant_natural_action("深圳天气怎么样").is_none());
    }

    #[test]
    fn home_assistant_entity_resolution_directs_unique_and_clarifies_many() {
        let kitchen = HomeAssistantEntity {
            entity_id: "light.kitchen".to_string(),
            domain: "light".to_string(),
            state: "off".to_string(),
            display_name: "厨房灯".to_string(),
            area_id: None,
            device_class: None,
            last_changed: None,
            last_updated: None,
            attributes: json!({"token": "must-not-leak"}),
        };
        let porch = HomeAssistantEntity {
            entity_id: "light.porch".to_string(),
            domain: "light".to_string(),
            state: "on".to_string(),
            display_name: "门口灯".to_string(),
            area_id: None,
            device_class: None,
            last_changed: None,
            last_updated: None,
            attributes: json!({}),
        };
        let action = HomeAssistantNaturalActionRequest {
            domain: "light".to_string(),
            service: "turn_on".to_string(),
            entity_hint: Some("门口".to_string()),
        };
        assert_eq!(
            resolve_home_assistant_action_entity(&action, &[kitchen.clone(), porch.clone()]),
            HomeAssistantEntityResolution::Resolved(porch.clone())
        );
        let ambiguous_action = HomeAssistantNaturalActionRequest {
            entity_hint: None,
            ..action
        };
        assert!(matches!(
            resolve_home_assistant_action_entity(
                &ambiguous_action,
                &[kitchen.clone(), porch.clone()]
            ),
            HomeAssistantEntityResolution::Clarify(candidates) if candidates.len() == 2
        ));
        assert_eq!(
            resolve_home_assistant_action_entity(&ambiguous_action, &[kitchen.clone()]),
            HomeAssistantEntityResolution::Resolved(kitchen)
        );

        let scene_action = HomeAssistantNaturalActionRequest {
            domain: "scene".to_string(),
            service: "turn_on".to_string(),
            entity_hint: Some("测试".to_string()),
        };
        let scene_on = HomeAssistantEntity {
            entity_id: "scene.harbordock_test_on".to_string(),
            domain: "scene".to_string(),
            state: "2026-05-09T00:32:28+00:00".to_string(),
            display_name: "HarborDock Test On".to_string(),
            area_id: None,
            device_class: None,
            last_changed: None,
            last_updated: None,
            attributes: json!({}),
        };
        let scene_off = HomeAssistantEntity {
            entity_id: "scene.harbordock_test_off".to_string(),
            domain: "scene".to_string(),
            state: "2026-05-09T00:32:29+00:00".to_string(),
            display_name: "HarborDock Test Off".to_string(),
            area_id: None,
            device_class: None,
            last_changed: None,
            last_updated: None,
            attributes: json!({}),
        };
        assert!(matches!(
            resolve_home_assistant_action_entity(&scene_action, &[scene_on, scene_off]),
            HomeAssistantEntityResolution::Clarify(candidates) if candidates.len() == 2
        ));
    }

    #[test]
    fn general_message_home_assistant_ambiguous_entities_need_input_and_persist_candidates() {
        let (service, conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("general-message-ha-clarify");
        let (base_url, _posts, server_thread) =
            start_mock_home_assistant_server(vec![home_assistant_light_entities()], 1);
        configure_mock_home_assistant(&service, &base_url);
        let request = general_message_test_request("ha_clarify", "开灯", Value::Null);

        let response = service.handle_task(request.clone());

        assert_eq!(response.status, TaskStatus::NeedsInput);
        assert_eq!(response.executor_used, "agentic_interpreter");
        assert_eq!(
            response.result.data["reply_pack"]["kind"],
            "ha_action_clarify"
        );
        assert_eq!(response.result.data["reply_pack"]["status"], "needs_input");
        assert!(response.result.message.contains("第一个"));
        assert_eq!(
            response.result.data["reply_pack"]["candidates"]
                .as_array()
                .map(Vec::len),
            Some(2)
        );
        let payload = serde_json::to_string(&response.result.data).expect("serialize response");
        assert!(!payload.contains("must-not-leak"));
        assert!(!payload.contains("ha-test-token"));

        let loaded = conversation_store
            .load_for_session(
                &request.source.session_id,
                Some(&request.source.conversation_id),
            )
            .expect("load conversation")
            .expect("conversation");
        let pending = loaded.general_message_loop().expect("pending HA clarify");
        let pending_ha = pending.home_assistant.expect("pending HA payload");
        assert_eq!(pending_ha.domain, "light");
        assert_eq!(pending_ha.service, "turn_on");
        assert_eq!(pending_ha.candidates.len(), 2);
        assert!(response.result.events.iter().any(|event| {
            event["event_type"] == "home_assistant.service_action_clarification_required"
                && event["payload"]["status"] == "needs_input"
        }));

        server_thread.join().expect("HA server");
        cleanup_task_api_service(admin_path, registry_path, conversation_path);
    }

    #[test]
    fn general_message_home_assistant_clarification_ordinal_executes_selected_entity() {
        let (service, conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("general-message-ha-ordinal");
        let (base_url, posts, server_thread) =
            start_mock_home_assistant_server(vec![home_assistant_light_entities()], 3);
        configure_mock_home_assistant(&service, &base_url);
        let first_request = general_message_test_request("ha_ordinal", "开灯", Value::Null);
        let first = service.handle_task(first_request.clone());
        let resume_token = first.resume_token.clone().expect("resume token");
        let followup = general_message_followup_request(
            "ha_ordinal_followup",
            "第一个",
            &first_request,
            &resume_token,
        );

        let response = service.handle_task(followup);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.result.data["reply_pack"]["status"], "succeeded");
        assert_eq!(
            response.result.data["reply_pack"]["entity"]["entity_id"],
            "light.kitchen"
        );
        assert_eq!(response.result.data["reply_pack"]["fields"], json!({}));
        let post_records = posts.lock().expect("posts");
        assert_eq!(post_records.len(), 1);
        assert_eq!(post_records[0]["path"], "/api/services/light/turn_on");
        assert_eq!(post_records[0]["body"]["entity_id"], "light.kitchen");
        assert_eq!(
            post_records[0]["body"]
                .as_object()
                .map(serde_json::Map::len),
            Some(1)
        );
        drop(post_records);
        let loaded = conversation_store
            .load_for_session(
                &first_request.source.session_id,
                Some(&first_request.source.conversation_id),
            )
            .expect("load conversation")
            .expect("conversation");
        assert!(loaded.general_message_loop().is_none());

        server_thread.join().expect("HA server");
        cleanup_task_api_service(admin_path, registry_path, conversation_path);
    }

    #[test]
    fn general_message_home_assistant_clarification_name_executes_selected_entity() {
        let (service, conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("general-message-ha-name");
        let (base_url, posts, server_thread) =
            start_mock_home_assistant_server(vec![home_assistant_light_entities()], 3);
        configure_mock_home_assistant(&service, &base_url);
        let first_request = general_message_test_request("ha_name", "开灯", Value::Null);
        let first = service.handle_task(first_request.clone());
        let resume_token = first.resume_token.clone().expect("resume token");
        let followup = general_message_followup_request(
            "ha_name_followup",
            "门口灯",
            &first_request,
            &resume_token,
        );

        let response = service.handle_task(followup);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.result.data["reply_pack"]["status"], "succeeded");
        assert_eq!(
            response.result.data["reply_pack"]["entity"]["entity_id"],
            "light.porch"
        );
        let post_records = posts.lock().expect("posts");
        assert_eq!(post_records.len(), 1);
        assert_eq!(post_records[0]["body"]["entity_id"], "light.porch");
        drop(post_records);
        let loaded = conversation_store
            .load_for_session(
                &first_request.source.session_id,
                Some(&first_request.source.conversation_id),
            )
            .expect("load conversation")
            .expect("conversation");
        assert!(loaded.general_message_loop().is_none());

        server_thread.join().expect("HA server");
        cleanup_task_api_service(admin_path, registry_path, conversation_path);
    }

    #[test]
    fn general_message_home_assistant_clarification_cancel_clears_pending_without_execution() {
        let (service, conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("general-message-ha-cancel");
        let (base_url, posts, server_thread) =
            start_mock_home_assistant_server(vec![home_assistant_light_entities()], 1);
        configure_mock_home_assistant(&service, &base_url);
        let first_request = general_message_test_request("ha_cancel", "开灯", Value::Null);
        let first = service.handle_task(first_request.clone());
        let resume_token = first.resume_token.clone().expect("resume token");
        server_thread.join().expect("HA server");
        let followup = general_message_followup_request(
            "ha_cancel_followup",
            "取消",
            &first_request,
            &resume_token,
        );

        let response = service.handle_task(followup);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(
            response.result.data["reply_pack"]["kind"],
            "ha_action_cancel"
        );
        assert_eq!(posts.lock().expect("posts").len(), 0);
        let loaded = conversation_store
            .load_for_session(
                &first_request.source.session_id,
                Some(&first_request.source.conversation_id),
            )
            .expect("load conversation")
            .expect("conversation");
        assert!(loaded.general_message_loop().is_none());

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
    }

    #[test]
    fn general_message_home_assistant_clarification_stale_candidate_keeps_frame() {
        let (service, conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("general-message-ha-stale");
        let only_porch = json!([home_assistant_entity_state(
            "light.porch",
            "on",
            "门口灯",
            Some("porch"),
            Some("light")
        )]);
        let (base_url, posts, server_thread) =
            start_mock_home_assistant_server(vec![home_assistant_light_entities(), only_porch], 2);
        configure_mock_home_assistant(&service, &base_url);
        let first_request = general_message_test_request("ha_stale", "开灯", Value::Null);
        let first = service.handle_task(first_request.clone());
        let resume_token = first.resume_token.clone().expect("resume token");
        let followup = general_message_followup_request(
            "ha_stale_followup",
            "第一个",
            &first_request,
            &resume_token,
        );

        let response = service.handle_task(followup);

        assert_eq!(response.status, TaskStatus::NeedsInput);
        assert_eq!(
            response.result.data["home_assistant_clarification"]["reason"],
            "candidate_stale"
        );
        assert_eq!(
            response.result.data["reply_pack"]["candidates"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        assert_eq!(posts.lock().expect("posts").len(), 0);
        let loaded = conversation_store
            .load_for_session(
                &first_request.source.session_id,
                Some(&first_request.source.conversation_id),
            )
            .expect("load conversation")
            .expect("conversation");
        let pending = loaded.general_message_loop().expect("pending");
        assert_eq!(
            pending
                .home_assistant
                .as_ref()
                .map(|pending_ha| pending_ha.candidates.len()),
            Some(1)
        );

        server_thread.join().expect("HA server");
        cleanup_task_api_service(admin_path, registry_path, conversation_path);
    }

    #[test]
    fn general_message_home_assistant_unsafe_followup_blocks_and_clears_pending() {
        let (service, conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("general-message-ha-unsafe-followup");
        let (base_url, posts, server_thread) =
            start_mock_home_assistant_server(vec![home_assistant_light_entities()], 1);
        configure_mock_home_assistant(&service, &base_url);
        let first_request = general_message_test_request("ha_unsafe_followup", "开灯", Value::Null);
        let first = service.handle_task(first_request.clone());
        let resume_token = first.resume_token.clone().expect("resume token");
        server_thread.join().expect("HA server");
        let followup = general_message_followup_request(
            "ha_unsafe_followup_reply",
            "解锁门锁",
            &first_request,
            &resume_token,
        );

        let response = service.handle_task(followup);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.result.data["reply_pack"]["status"], "blocked");
        assert_eq!(response.result.data["reply_pack"]["executed"], false);
        assert_eq!(posts.lock().expect("posts").len(), 0);
        let loaded = conversation_store
            .load_for_session(
                &first_request.source.session_id,
                Some(&first_request.source.conversation_id),
            )
            .expect("load conversation")
            .expect("conversation");
        assert!(loaded.general_message_loop().is_none());

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
    }

    #[test]
    fn vision_event_summary_redacts_artifact_paths() {
        let stored = StoredLocalVisionEvent {
            received_at: "2026-06-02T01:02:03Z".to_string(),
            event: LocalVisionEvent {
                event_id: "event-1".to_string(),
                camera_id: "front-door".to_string(),
                event_type: "motion".to_string(),
                confidence: 0.91,
                labels: vec!["person".to_string()],
                summary: "门口有人经过".to_string(),
                snapshot_artifact: SnapshotArtifact {
                    artifact_id: Some("artifact-1".to_string()),
                    path: Some(
                        "/mnt/software/harborbeacon-agent-ci/secret/snapshot.jpg".to_string(),
                    ),
                    mime_type: Some("image/jpeg".to_string()),
                    byte_size: Some(1234),
                    sha256: Some("abcdef".to_string()),
                    source: Some("local_snapshot".to_string()),
                },
                started_at: "2026-06-02T01:02:02Z".to_string(),
                analyzer: "local-vlm".to_string(),
                latency_ms: 42,
                metrics: json!({}),
                vlm: Some(LocalVisionEventVlmSummary {
                    status: "completed".to_string(),
                    summary: "门口有人".to_string(),
                    tags: vec!["person".to_string()],
                    labels: vec!["person".to_string()],
                    derived_text: String::new(),
                    artifacts: vec![LocalVisionEventArtifact {
                        artifact_id: Some("vlm-artifact".to_string()),
                        role: "annotated_snapshot".to_string(),
                        mime_type: Some("image/jpeg".to_string()),
                        byte_size: Some(222),
                        sha256: Some("123456".to_string()),
                        source: Some("vlm".to_string()),
                    }],
                    ingest_metadata: json!({}),
                    vlm_metrics: json!({}),
                    error: Some("rtsp://admin:secret@camera.local/stream".to_string()),
                }),
            },
            audit_record: json!({}),
            ha_mqtt_payload: json!({}),
        };

        let summary = build_redacted_vision_event_summary(&stored);
        let text = serde_json::to_string(&summary).expect("serialize summary");

        assert_eq!(summary["snapshot_artifact"]["path_redacted"], json!(true));
        assert_eq!(summary["snapshot_artifact"]["sha256_present"], json!(true));
        assert!(!text.contains("/mnt/software"));
        assert!(!text.contains("rtsp://"));
        assert!(!text.contains("admin:secret"));
        assert!(text.contains("redacted"));
    }

    #[test]
    fn system_readiness_summary_redacts_gate_and_ha_secrets() {
        let mut state = AdminConsoleState::default();
        state.home_assistant.enabled = true;
        state.home_assistant.base_url = "http://homeassistant.local:8123".to_string();
        state.home_assistant.access_token = "ha-secret-token".to_string();
        state.home_assistant.last_status = "synced".to_string();
        state.home_assistant.entity_count = 7;
        state.notification_targets = vec![NotificationTargetRecord {
            target_id: "target-1".to_string(),
            label: "Weixin".to_string(),
            route_key: "gw_route_secret".to_string(),
            platform_hint: "weixin".to_string(),
            is_default: true,
        }];
        let gateway_status = json!({
            "channels": [{
                "platform": "weixin",
                "configured": true,
                "connected": true,
                "context_token": "secret-context-token"
            }],
            "context_tokens": { "count": 1 },
        });

        let readiness =
            build_general_message_readiness_summary(&state, Some(&gateway_status), 2, None);
        let text = serde_json::to_string(&readiness).expect("serialize readiness");

        assert_eq!(readiness["gateway"]["weixin"]["status"], json!("connected"));
        assert_eq!(
            readiness["default_notification_target"]["route_key_redacted"],
            json!(true)
        );
        assert_eq!(readiness["home_assistant"]["token_redacted"], json!(true));
        assert!(!text.contains("gw_route_secret"));
        assert!(!text.contains("ha-secret-token"));
        assert!(!text.contains("secret-context-token"));
    }

    #[test]
    fn general_message_workflow_compiler_shadow_matches_system_readiness() {
        let (service, _conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("general-message-workflow-shadow-status");
        let request = general_message_test_request(
            "general_message_workflow_shadow_status",
            "状态",
            Value::Null,
        );

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(
            response.result.data["reply_pack"]["kind"],
            json!("system_readiness")
        );
        let shadow =
            &response.result.data["general_message_controller"]["workflow_compiler_shadow"];
        assert_eq!(shadow["workflow_id"], json!("system_diagnostics_v1"));
        assert_eq!(shadow["candidate_kind"], json!("system_readiness"));
        assert_eq!(shadow["matched_current_plan"], json!(true));
        assert_eq!(shadow["read_only"], json!(true));
        assert_eq!(shadow["unsafe_action"], json!(false));

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
    }

    #[test]
    fn general_message_workflow_compiler_shadow_does_not_override_nsp_plan() {
        let (service, _conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("general-message-workflow-shadow-mismatch");
        configure_mock_general_message_llm(
            &service,
            r#"{"decision":"capability_summary","confidence":0.95,"reason":"forced mismatch"}"#,
        );
        let request = general_message_test_request(
            "general_message_workflow_shadow_mismatch",
            "状态",
            Value::Null,
        );

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(
            response.result.data["general_message_controller"]["controller_stage"],
            json!("nsp_router")
        );
        assert_eq!(
            response.result.data["reply_pack"]["kind"],
            json!("capability_summary")
        );
        let shadow =
            &response.result.data["general_message_controller"]["workflow_compiler_shadow"];
        assert_eq!(shadow["candidate_kind"], json!("system_readiness"));
        assert_eq!(shadow["matched_current_plan"], json!(false));
        assert_eq!(shadow["unsafe_action"], json!(false));

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
    }

    #[test]
    fn parse_general_message_plan_accepts_json_embedded_in_text() {
        let plan = parse_general_message_plan(
            r#"先给出结论：
```json
{
  "decision": "capability_summary",
  "reply_text": "我可以帮你看摄像头、录短视频，也能搜索保存的内容。",
  "camera_hint": "null",
  "query": "null",
  "reason": "camera capability question"
}
```
如果你愿意，我可以继续执行。"#,
        )
        .expect("embedded plan");

        assert_eq!(plan.kind, GeneralMessagePlanKind::CapabilitySummary);
        assert_eq!(
            plan.reply_text.as_deref(),
            Some("我可以帮你看摄像头、录短视频，也能搜索保存的内容。")
        );
        assert_eq!(plan.camera_hint, None);
        assert_eq!(plan.query, None);

        let rag_plan = parse_general_message_plan(
            r#"{"decision":"rag_answer","query":"樱花计划","reason":"answer from knowledge context"}"#,
        )
        .expect("rag answer plan");
        assert_eq!(rag_plan.kind, GeneralMessagePlanKind::RagAnswer);
        assert_eq!(rag_plan.query.as_deref(), Some("樱花计划"));

        let ha_plan = parse_general_message_plan(r#"{"decision":"ha_service_action"}"#)
            .expect("HA action plan");
        assert_eq!(
            ha_plan.kind,
            GeneralMessagePlanKind::HomeAssistantServiceAction
        );
        assert!(ha_plan.home_assistant_action.is_none());
        let ha_slots_plan = parse_general_message_plan(
            r#"{
                "decision": "ha_service_action",
                "confidence": 0.91,
                "canonical_phrase": "开灯",
                "home_assistant": {
                    "domain": "light",
                    "service": "turn_on",
                    "entity_hint": "厨房"
                }
            }"#,
        )
        .expect("HA slots plan");
        assert_eq!(ha_slots_plan.confidence, Some(91));
        assert_eq!(ha_slots_plan.canonical_phrase.as_deref(), Some("开灯"));
        assert_eq!(
            ha_slots_plan.home_assistant_action,
            Some(HomeAssistantNaturalAction::Request(
                HomeAssistantNaturalActionRequest {
                    domain: "light".to_string(),
                    service: "turn_on".to_string(),
                    entity_hint: Some("厨房".to_string()),
                }
            ))
        );
        let notify_plan =
            parse_general_message_plan(r#"{"decision":"vision_event_notify_latest"}"#)
                .expect("event notify plan");
        assert_eq!(
            notify_plan.kind,
            GeneralMessagePlanKind::VisionEventNotifyLatest
        );
    }

    #[test]
    fn parse_general_message_plan_maps_unsupported_to_conversation_boundary() {
        let plan = parse_general_message_plan(
            r#"{"decision":"unsupported","reply_text":"这件事我现在不直接处理。"}"#,
        )
        .expect("conversation boundary plan");

        assert_eq!(plan.kind, GeneralMessagePlanKind::ConversationAct);
        assert_eq!(
            plan.conversation_act,
            Some(GeneralMessageConversationAct::Boundary)
        );
        assert_eq!(plan.reply_text.as_deref(), Some("这件事我现在不直接处理。"));
    }

    #[test]
    fn parse_general_message_plan_accepts_closed_nsp_tool_decisions() {
        let cases = [
            ("camera_snapshot", GeneralMessagePlanKind::CameraSnapshot),
            (
                "camera_record_clip",
                GeneralMessagePlanKind::CameraRecordClip,
            ),
            (
                "vision_event_summary",
                GeneralMessagePlanKind::VisionEventSummary,
            ),
            (
                "vision_event_notify_latest",
                GeneralMessagePlanKind::VisionEventNotifyLatest,
            ),
            ("system_readiness", GeneralMessagePlanKind::SystemReadiness),
            ("evt_readiness", GeneralMessagePlanKind::EvtReadiness),
            ("evt_preflight", GeneralMessagePlanKind::EvtPreflight),
            (
                "evt_evidence_bundle",
                GeneralMessagePlanKind::EvtEvidenceBundle,
            ),
            ("knowledge_search", GeneralMessagePlanKind::KnowledgeSearch),
            ("rag_answer", GeneralMessagePlanKind::RagAnswer),
        ];

        for (decision, expected_kind) in cases {
            let plan = parse_general_message_plan(&format!(
                r#"{{
                    "decision": "{decision}",
                    "confidence": 0.88,
                    "canonical_phrase": "{decision}",
                    "camera_hint": "门口",
                    "query": "本地资料"
                }}"#
            ))
            .expect("closed NSP decision");
            assert_eq!(plan.kind, expected_kind);
            assert_eq!(plan.confidence, Some(88));
            assert_eq!(plan.canonical_phrase.as_deref(), Some(decision));
        }
    }

    #[test]
    fn general_message_router_system_prompt_disables_qwen3_thinking() {
        let prompt = super::build_general_message_router_system_prompt();

        assert!(prompt.starts_with("/no_think"));
        assert!(prompt.contains("Return exactly one valid JSON object"));
        assert!(prompt.contains("\"confidence\":0.95"));
        assert!(prompt.contains("do not copy 0.0"));
        assert!(prompt.contains("asking status/diagnostics/health"));
        assert!(prompt.contains("evt_readiness, evt_preflight, evt_evidence_bundle"));
        assert!(prompt.contains("4h or 72h stress test"));
        assert!(prompt.contains("default notification"));
        assert!(prompt.contains("\"domain\":\"scene\",\"service\":\"turn_on\""));
        assert!(prompt.contains("帮我做一下EVT预检"));
        assert!(prompt.contains("\"decision\":\"evt_preflight\""));
        assert!(prompt.contains("生成压测证据"));
        assert!(prompt.contains("\"decision\":\"evt_evidence_bundle\""));
    }

    #[test]
    fn general_message_nsp_hard_guard_overrides_external_or_unsafe_continue() {
        let weather_request =
            general_message_test_request("nsp_boundary_weather", "深圳今天会下雨吗", Value::Null);
        let mut weather_plan =
            parse_general_message_plan(r#"{"decision":"conversation_continue","confidence":0.95}"#)
                .expect("weather continue plan");

        super::enforce_general_message_nsp_hard_guards(&weather_request, &mut weather_plan);

        assert_eq!(weather_plan.kind, GeneralMessagePlanKind::ConversationAct);
        assert_eq!(
            weather_plan.conversation_act,
            Some(GeneralMessageConversationAct::Boundary)
        );
        assert_eq!(
            weather_plan.reason.as_deref(),
            Some("nsp_boundary_hard_guard")
        );

        let lock_request = general_message_test_request(
            "nsp_boundary_lock",
            "我到门口了，把门锁打开",
            Value::Null,
        );
        let mut lock_plan =
            parse_general_message_plan(r#"{"decision":"conversation_continue","confidence":0.95}"#)
                .expect("lock continue plan");

        super::enforce_general_message_nsp_hard_guards(&lock_request, &mut lock_plan);

        assert_eq!(
            lock_plan.conversation_act,
            Some(GeneralMessageConversationAct::Boundary)
        );

        let long_run_request = general_message_test_request(
            "nsp_boundary_evt_long_run",
            "开始72小时压测",
            Value::Null,
        );
        let mut long_run_plan =
            parse_general_message_plan(r#"{"decision":"evt_preflight","confidence":0.95}"#)
                .expect("long run plan");

        super::enforce_general_message_nsp_hard_guards(&long_run_request, &mut long_run_plan);

        assert_eq!(long_run_plan.kind, GeneralMessagePlanKind::ConversationAct);
        assert_eq!(
            long_run_plan.conversation_act,
            Some(GeneralMessageConversationAct::Boundary)
        );
        assert_eq!(
            long_run_plan.reason.as_deref(),
            Some("evt_long_run_requires_operator_supervisor")
        );
        assert!(long_run_plan
            .reply_text
            .as_deref()
            .unwrap_or_default()
            .contains("operator supervisor"));
    }

    #[test]
    fn general_message_evt_preflight_runs_short_guard_only() {
        let (service, _conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("general-message-evt-preflight");
        configure_mock_general_message_llm(
            &service,
            r#"{
                "decision": "evt_preflight",
                "confidence": 0.94,
                "canonical_phrase": "EVT预检",
                "reason": "user asks for pre-stress environment preflight"
            }"#,
        );

        let response = service.handle_task(general_message_test_request(
            "evt_preflight",
            "帮我做一下EVT预检",
            Value::Null,
        ));

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.result.data["reply_pack"]["kind"], "evt_preflight");
        assert_eq!(
            response.result.data["reply_pack"]["preflight"]["long_run_started"],
            json!(false)
        );
        assert_eq!(
            response.result.data["reply_pack"]["preflight"]["short_run_started"],
            json!(false)
        );
        let text = serde_json::to_string(&response.result.data).expect("serialize reply");
        assert!(!text.contains("rtsp://"));
        assert!(!text.contains("Bearer "));
        assert!(!text.contains("data:image"));

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
    }

    #[test]
    fn general_message_nsp_low_confidence_tool_does_not_execute() {
        let (service, _conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("general-message-nsp-low-confidence");
        configure_mock_general_message_llm(
            &service,
            r#"{
                "decision": "camera_snapshot",
                "confidence": 0.31,
                "canonical_phrase": "拍一张门口",
                "camera_hint": "front-door",
                "reason": "weak visual request"
            }"#,
        );

        let response = service.handle_task(general_message_test_request(
            "nsp_low_confidence",
            "门口现在咋样",
            Value::Null,
        ));

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.executor_used, "agentic_interpreter");
        assert_eq!(
            response.result.data["reply_pack"]["kind"],
            "conversation_continue"
        );
        assert_eq!(
            response.result.data["general_message_controller"]["fallback_reason"],
            "nsp_low_confidence"
        );
        assert_eq!(
            response.result.data["general_message_controller"]["nsp_route"]["confidence"],
            json!(31)
        );

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
    }

    #[test]
    fn general_message_nsp_home_assistant_slots_clarify_ambiguous_entities() {
        let (service, _conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("general-message-nsp-ha-clarify");
        let (base_url, posts, server_thread) =
            start_mock_home_assistant_server(vec![home_assistant_light_entities()], 1);
        configure_mock_home_assistant(&service, &base_url);
        configure_mock_general_message_llm(
            &service,
            r#"{
                "decision": "ha_service_action",
                "confidence": 0.94,
                "canonical_phrase": "开灯",
                "home_assistant": {
                    "domain": "light",
                    "service": "turn_on",
                    "entity_hint": null
                },
                "reason": "safe low-risk light action"
            }"#,
        );

        let response = service.handle_task(general_message_test_request(
            "nsp_ha_clarify",
            "屋里太暗了，帮我弄亮一点",
            Value::Null,
        ));

        assert_eq!(response.status, TaskStatus::NeedsInput);
        assert_eq!(response.result.data["reply_pack"]["status"], "needs_input");
        assert_eq!(
            response.result.data["general_message_controller"]["controller_stage"],
            "nsp_router"
        );
        assert_eq!(
            response.result.data["general_message_controller"]["nsp_route"]["decision"],
            "ha_service_action"
        );
        assert_eq!(posts.lock().expect("posts").len(), 0);

        server_thread.join().expect("HA server");
        cleanup_task_api_service(admin_path, registry_path, conversation_path);
    }

    #[test]
    fn general_message_nsp_unsafe_home_assistant_slots_are_blocked() {
        let (service, _conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("general-message-nsp-ha-blocked");
        configure_mock_general_message_llm(
            &service,
            r#"{
                "decision": "ha_service_action",
                "confidence": 0.95,
                "canonical_phrase": "解锁门锁",
                "home_assistant": {
                    "domain": "lock",
                    "service": "unlock",
                    "entity_hint": "门口"
                },
                "reason": "unsafe lock action"
            }"#,
        );

        let response = service.handle_task(general_message_test_request(
            "nsp_ha_blocked",
            "我回来了，帮我把门打开",
            Value::Null,
        ));

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.result.data["reply_pack"]["status"], "blocked");
        assert_eq!(response.result.data["reply_pack"]["executed"], false);
        assert_eq!(
            response.result.data["general_message_controller"]["controller_stage"],
            "nsp_router"
        );
        let text = serde_json::to_string(&response.result.data).expect("serialize response");
        assert!(!text.contains("ha-test-token"));
        assert!(!text.contains("rtsp://"));

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
    }

    #[test]
    fn general_message_capability_query_returns_summary_without_llm() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let service = TaskApiService::new(
            AdminConsoleStore::new(
                admin_path.clone(),
                DeviceRegistryStore::new(registry_path.clone()),
            ),
            TaskConversationStore::new(conversation_path.clone()),
        );
        let request = TaskRequest {
            task_id: "task-capability".to_string(),
            trace_id: "trace-capability".to_string(),
            step_id: "step-capability".to_string(),
            source: TaskSource {
                channel: "wechat".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-capability".to_string(),
                user_id: "user-1".to_string(),
                session_id: "session-capability".to_string(),
                route_key: "gw_route_capability".to_string(),
            },
            intent: TaskIntent {
                domain: "general".to_string(),
                action: "message".to_string(),
                raw_text: "摄像头能干什么".to_string(),
            },
            entity_refs: Value::Null,
            args: Value::Null,
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: "om_capability".to_string(),
                chat_type: "p2p".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.executor_used, "agentic_interpreter");
        assert_eq!(
            response.result.message,
            response.result.data["reply_pack"]["summary"]
        );
        assert_eq!(
            response.result.data["reply_pack"]["kind"],
            "capability_summary"
        );
        assert_eq!(
            response.result.data["reply_pack"]["examples"]
                .as_array()
                .map(Vec::len),
            Some(19)
        );
        assert!(response.result.data["reply_pack"]["capabilities"]
            .as_array()
            .expect("capabilities")
            .iter()
            .any(|capability| capability.as_str() == Some("ha_service_action")));
        assert!(response.result.data["reply_pack"]["capabilities"]
            .as_array()
            .expect("capabilities")
            .iter()
            .any(|capability| capability.as_str() == Some("vision_event_notify_latest")));
        assert!(response.result.data["reply_pack"]["capabilities"]
            .as_array()
            .expect("capabilities")
            .iter()
            .any(|capability| capability.as_str() == Some("evt_preflight")));
        assert!(response.result.data["reply_pack"]["capabilities"]
            .as_array()
            .expect("capabilities")
            .iter()
            .any(|capability| capability.as_str() == Some("guardian_rule_proposal")));
        assert!(response.result.data["reply_pack"]["capabilities"]
            .as_array()
            .expect("capabilities")
            .iter()
            .any(|capability| capability.as_str() == Some("vlm_describe_latest_event")));
        assert!(response.result.data["reply_pack"]["capabilities"]
            .as_array()
            .expect("capabilities")
            .iter()
            .any(|capability| capability.as_str() == Some("family_memory_summary")));
        assert!(response.result.data["reply_pack"]["examples"]
            .as_array()
            .expect("examples")
            .iter()
            .any(|example| example.as_str() == Some("通知最新事件")));
        assert!(response.result.message.contains("K3 家庭入口"));
        assert!(response.result.message.contains("低风险家居动作"));

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    #[test]
    fn general_message_local_first_architecture_returns_policy_summary() {
        let (service, _conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("general-message-local-first-architecture");
        let request = general_message_test_request(
            "local-first-architecture",
            "解释一下 HarborBeacon 和 HarborGate 现在的 local-first 架构，以及云端 fallback 是怎么受控的",
            Value::Null,
        );

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.executor_used, "agentic_interpreter");
        assert_eq!(
            response.result.data["reply_pack"]["kind"],
            "capability_summary"
        );
        assert!(response.result.message.contains("local-first"));
        assert!(response.result.message.contains("HarborBeacon"));
        assert!(response.result.message.contains("HarborGate"));
        assert!(response.result.message.contains("受控 fallback"));
        assert!(response.result.message.contains("SiliconFlow"));
        assert_eq!(
            response.result.data["general_message_controller"]["router_llm"],
            true
        );
        assert_eq!(
            response.result.data["general_message_controller"]["nsp_route"]["local_only"],
            true
        );

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
    }

    #[test]
    fn general_message_capability_query_records_controller_trace() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let service = TaskApiService::new(
            AdminConsoleStore::new(
                admin_path.clone(),
                DeviceRegistryStore::new(registry_path.clone()),
            ),
            TaskConversationStore::new(conversation_path.clone()),
        );
        let request = TaskRequest {
            task_id: "task-capability-trace".to_string(),
            trace_id: "trace-capability-trace".to_string(),
            step_id: "step-capability-trace".to_string(),
            source: TaskSource {
                channel: "wechat".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-capability-trace".to_string(),
                user_id: "user-1".to_string(),
                session_id: "session-capability-trace".to_string(),
                route_key: "gw_route_capability_trace".to_string(),
            },
            intent: TaskIntent {
                domain: "general".to_string(),
                action: "message".to_string(),
                raw_text: "摄像头能干什么".to_string(),
            },
            entity_refs: Value::Null,
            args: Value::Null,
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: "om_capability_trace".to_string(),
                chat_type: "p2p".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        };

        let response = service.handle_task(request);

        assert_eq!(
            response.result.data["general_message_controller"]["controller_stage"],
            "deterministic_single_candidate"
        );
        assert_eq!(
            response.result.data["general_message_controller"]["router_llm"],
            true
        );
        assert_eq!(
            response.result.data["general_message_controller"]["nsp_route"]["local_only"],
            true
        );
        assert!(
            response.result.data["general_message_controller"]["total_turn_latency_ms"]
                .as_u64()
                .is_some()
        );

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    #[test]
    fn general_message_feedback_defaults_to_conversation_continue() {
        let (service, _conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("general-message-feedback");
        for (index, raw_text) in ["非常好", "谢谢", "收到", "你真棒"].into_iter().enumerate()
        {
            let request = general_message_test_request(
                &format!("general_feedback_{index}"),
                raw_text,
                Value::Null,
            );

            let response = service.handle_task(request);

            assert_eq!(response.status, TaskStatus::Completed);
            assert_eq!(response.executor_used, "agentic_interpreter");
            assert_eq!(
                response.result.data["reply_pack"]["kind"],
                "conversation_continue"
            );
            assert_eq!(
                response.result.data["reply_pack"]["conversation_act"],
                "continue"
            );
            assert!(!response.result.message.contains("我暂时还不能稳定理解"));
        }

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
    }

    #[test]
    fn general_message_boundary_is_conversation_act_not_unsupported() {
        let (service, _conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("general-message-boundary");
        let request =
            general_message_test_request("general_boundary", "今天天气怎么样", Value::Null);

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.executor_used, "agentic_interpreter");
        assert_eq!(
            response.result.data["reply_pack"]["kind"],
            "conversation_boundary"
        );
        assert!(response.result.message.contains("天气"));
        assert!(response.result.message.contains("公网实时信息"));
        assert!(response.result.message.contains("低风险家居动作"));
        assert!(!response.result.message.contains("我暂时还不能稳定理解"));

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
    }

    #[test]
    fn general_message_repair_is_conversation_act_not_tool() {
        let (service, _conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("general-message-repair");
        let request = general_message_test_request("general_repair", "不对", Value::Null);

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.executor_used, "agentic_interpreter");
        assert_eq!(
            response.result.data["reply_pack"]["kind"],
            "conversation_repair"
        );
        assert_eq!(response.result.artifacts.len(), 0);

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
    }

    #[test]
    fn general_message_pending_feedback_preserves_loop_state() {
        let (service, conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("general-message-pending-feedback");
        let first_request =
            general_message_test_request("general_pending_feedback", "帮我看一下门口", Value::Null);
        let first_response = service.handle_task(first_request);
        assert_eq!(first_response.status, TaskStatus::NeedsInput);
        let resume_token = first_response.resume_token.clone().expect("resume token");

        let mut second_request = general_message_test_request(
            "general_pending_feedback_followup",
            "非常好",
            json!({ "continuation_token": resume_token }),
        );
        second_request.source = TaskSource {
            session_id: "session-general_pending_feedback".to_string(),
            conversation_id: "chat-general_pending_feedback".to_string(),
            ..second_request.source
        };
        let response = service.handle_task(second_request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(
            response.result.data["reply_pack"]["kind"],
            "conversation_clarify_continue"
        );
        let loaded = conversation_store
            .load_for_session(
                "session-general_pending_feedback",
                Some("chat-general_pending_feedback"),
            )
            .expect("load conversation")
            .expect("conversation");
        assert!(loaded.general_message_loop().is_some());

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
    }

    #[test]
    fn general_message_turn_feedback_keeps_active_clarify_frame() {
        let (service, conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("general-message-turn-pending-feedback");
        let first_response = service.handle_turn(general_message_turn_envelope(
            "general_turn_pending_feedback",
            "帮我看一下门口",
            None,
            None,
        ));
        assert_eq!(first_response.turn.status, TaskStatus::NeedsInput);
        assert_eq!(first_response.reply.kind, "frame_prompt");
        let first_frame = first_response.active_frame.expect("active frame");
        assert_eq!(first_frame.kind, "conversation.clarify");

        let second_response = service.handle_turn(general_message_turn_envelope(
            "general_turn_pending_feedback_followup",
            "非常好",
            Some(first_response.conversation.handle.clone()),
            Some(TaskTurnContinuation {
                token: first_frame.continuation_token.clone(),
                frame_id: first_frame.frame_id.clone(),
                reply_to_turn_id: first_response.turn.turn_id.clone(),
                expires_at: None,
            }),
        ));

        assert_eq!(second_response.turn.status, TaskStatus::Completed);
        assert_eq!(second_response.reply.kind, "clarify");
        assert!(second_response.reply.text.contains("拍一张"));
        let second_frame = second_response.active_frame.expect("active frame");
        assert_eq!(second_frame.kind, "conversation.clarify");
        assert_eq!(
            second_frame.continuation_token,
            first_frame.continuation_token
        );
        let loaded = conversation_store
            .load_for_session("", Some(&first_response.conversation.handle))
            .expect("load conversation")
            .expect("conversation");
        assert!(loaded.general_message_loop().is_some());

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
    }

    #[test]
    fn clip_confirmation_turn_feedback_preserves_frame_until_playback() {
        let (service, conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("clip-confirmation-turn-feedback");
        let handle = "conv-clip-confirmation-feedback";
        let token = "cont-clip-feedback";
        seed_clip_confirmation_turn_state(&conversation_store, handle, token);

        let feedback = service.handle_turn(general_message_turn_envelope(
            "clip_feedback",
            "非常好",
            Some(handle.to_string()),
            Some(TaskTurnContinuation {
                token: token.to_string(),
                frame_id: "frame-clip-feedback".to_string(),
                reply_to_turn_id: "turn-record".to_string(),
                expires_at: None,
            }),
        ));

        assert_eq!(feedback.turn.status, TaskStatus::Completed);
        assert_eq!(feedback.reply.kind, "frame_prompt");
        assert!(feedback.reply.text.contains("谢谢认可"));
        assert!(feedback
            .reply
            .text
            .contains("刚才 Tapo 231 那段短视频已经录好"));
        assert!(feedback.reply.text.contains("要发完整回放吗"));
        let feedback_frame = feedback.active_frame.expect("active clip frame");
        assert_eq!(feedback_frame.kind, "camera.clip_confirmation");
        assert_eq!(feedback_frame.continuation_token, token);

        let playback = service.handle_turn(general_message_turn_envelope(
            "clip_feedback_playback",
            "回放一下刚刚录的短视频",
            Some(handle.to_string()),
            Some(TaskTurnContinuation {
                token: feedback_frame.continuation_token.clone(),
                frame_id: feedback_frame.frame_id.clone(),
                reply_to_turn_id: feedback.turn.turn_id.clone(),
                expires_at: None,
            }),
        ));

        assert_eq!(playback.turn.status, TaskStatus::Completed);
        assert_eq!(playback.reply.kind, "tool_result");
        assert_eq!(playback.reply.text, "完整回放如下");
        assert!(playback.active_frame.is_none());
        assert_eq!(playback.artifacts.len(), 1);
        assert_eq!(playback.artifacts[0].kind, "video");
        assert_eq!(playback.delivery_hints.len(), 1);
        assert_eq!(playback.delivery_hints[0].kind, "native_video");
        let loaded = conversation_store
            .load_for_session(handle, Some(handle))
            .expect("load conversation")
            .expect("conversation");
        assert!(loaded.clip_pending_confirmation().is_none());

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
    }

    #[test]
    fn clip_confirmation_turn_no_tool_inputs_preserve_frame_without_continuation() {
        let (service, conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("clip-confirmation-turn-no-tool-preserve");
        let handle = "conv-clip-confirmation-no-tool";
        let token = "cont-clip-no-tool";
        seed_clip_confirmation_turn_state(&conversation_store, handle, token);

        for (index, (text, expected_intro)) in [
            ("谢谢你", "谢谢认可"),
            ("你真棒", "谢谢认可"),
            ("今天天气怎么样", "天气这类实时信息我现在不处理"),
            ("你能干什么", "我可以抓拍最新画面"),
            ("不对", "明白，我先不把这句话当作新的工具动作"),
        ]
        .into_iter()
        .enumerate()
        {
            let response = service.handle_turn(general_message_turn_envelope(
                &format!("clip_no_tool_{index}"),
                text,
                Some(handle.to_string()),
                None,
            ));
            assert_eq!(response.turn.status, TaskStatus::Completed);
            assert_eq!(response.reply.kind, "frame_prompt");
            assert!(response.reply.text.contains(expected_intro));
            assert!(response
                .reply
                .text
                .contains("刚才 Tapo 231 那段短视频已经录好"));
            assert!(response.reply.text.contains("要发完整回放吗"));
            let frame = response.active_frame.expect("active clip frame");
            assert_eq!(frame.kind, "camera.clip_confirmation");
            assert_eq!(frame.continuation_token, token);
        }

        let playback = service.handle_turn(general_message_turn_envelope(
            "clip_no_tool_playback",
            "要",
            Some(handle.to_string()),
            None,
        ));
        assert_eq!(playback.turn.status, TaskStatus::Completed);
        assert_eq!(playback.reply.text, "完整回放如下");
        assert!(playback.active_frame.is_none());
        assert_eq!(playback.delivery_hints[0].kind, "native_video");

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
    }

    #[test]
    fn clip_confirmation_turn_cancel_clears_frame_without_continuation() {
        let (service, conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("clip-confirmation-turn-cancel");
        let handle = "conv-clip-confirmation-cancel";
        seed_clip_confirmation_turn_state(&conversation_store, handle, "cont-clip-cancel");

        let response = service.handle_turn(general_message_turn_envelope(
            "clip_cancel",
            "算了",
            Some(handle.to_string()),
            None,
        ));

        assert_eq!(response.turn.status, TaskStatus::Completed);
        assert_eq!(response.reply.kind, "cancel");
        assert!(response.active_frame.is_none());
        let loaded = conversation_store
            .load_for_session(handle, Some(handle))
            .expect("load conversation")
            .expect("conversation");
        assert!(loaded.clip_pending_confirmation().is_none());

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
    }

    #[test]
    fn clip_confirmation_turn_explicit_tool_supersedes_frame() {
        let (service, conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("clip-confirmation-turn-supersede");
        let handle = "conv-clip-confirmation-supersede";
        seed_clip_confirmation_turn_state(&conversation_store, handle, "cont-clip-supersede");

        let response = service.handle_turn(general_message_turn_envelope(
            "clip_supersede",
            "拍一张",
            Some(handle.to_string()),
            None,
        ));

        assert_eq!(response.turn.status, TaskStatus::Failed);
        assert!(response.active_frame.is_none());
        let loaded = conversation_store
            .load_for_session(handle, Some(handle))
            .expect("load conversation")
            .expect("conversation");
        assert!(loaded.clip_pending_confirmation().is_none());

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
    }

    #[test]
    fn general_message_pending_cancel_clears_loop_state() {
        let (service, conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("general-message-pending-cancel");
        let first_request =
            general_message_test_request("general_pending_cancel", "帮我看一下门口", Value::Null);
        let first_response = service.handle_task(first_request);
        assert_eq!(first_response.status, TaskStatus::NeedsInput);
        let resume_token = first_response.resume_token.clone().expect("resume token");

        let mut second_request = general_message_test_request(
            "general_pending_cancel_followup",
            "算了",
            json!({ "continuation_token": resume_token }),
        );
        second_request.source = TaskSource {
            session_id: "session-general_pending_cancel".to_string(),
            conversation_id: "chat-general_pending_cancel".to_string(),
            ..second_request.source
        };
        let response = service.handle_task(second_request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(
            response.result.data["reply_pack"]["kind"],
            "conversation_cancel"
        );
        let loaded = conversation_store
            .load_for_session(
                "session-general_pending_cancel",
                Some("chat-general_pending_cancel"),
            )
            .expect("load conversation")
            .expect("conversation");
        assert!(loaded.general_message_loop().is_none());

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
    }

    #[test]
    fn general_message_ambiguous_request_returns_clarification_and_persists_loop_state() {
        let (service, conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("general-message-clarification");
        configure_mock_general_message_llm(
            &service,
            r#"{
                "decision": "clarify",
                "reply_text": "你是想拍一张门口画面，还是录一段短视频？",
                "camera_hint": "front-door",
                "reason": "need one follow-up"
            }"#,
        );

        let request = TaskRequest {
            task_id: "task-general-clarify".to_string(),
            trace_id: "trace-general-clarify".to_string(),
            step_id: "step-general-clarify".to_string(),
            source: TaskSource {
                channel: "weixin".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-general-clarify".to_string(),
                user_id: "user-1".to_string(),
                session_id: "session-general-clarify".to_string(),
                route_key: "gw_route_general_clarify".to_string(),
            },
            intent: TaskIntent {
                domain: "general".to_string(),
                action: "message".to_string(),
                raw_text: "帮我看一下门口".to_string(),
            },
            entity_refs: Value::Null,
            args: Value::Null,
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: "om_general_clarify".to_string(),
                chat_type: "p2p".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::NeedsInput);
        assert_eq!(response.executor_used, "agentic_interpreter");
        assert_eq!(response.result.data["reply_pack"]["kind"], "clarification");
        assert_eq!(
            response.result.message,
            "你是想拍一张门口画面，还是录一段短视频？"
        );
        let resume_token = response.resume_token.clone().expect("resume token");
        let loaded = conversation_store
            .load_for_session("session-general-clarify", Some("chat-general-clarify"))
            .expect("load conversation")
            .expect("conversation");
        let pending = loaded.general_message_loop().expect("pending loop");
        assert_eq!(pending.resume_token, resume_token);
        assert_eq!(pending.original_goal, "帮我看一下门口");
        assert_eq!(pending.latest_user_intent_text, "帮我看一下门口");
        assert_eq!(
            pending.last_clarification_prompt,
            "你是想拍一张门口画面，还是录一段短视频？"
        );
        assert_eq!(pending.camera_hint.as_deref(), Some("front-door"));

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
    }

    #[test]
    fn general_message_resume_token_can_route_follow_up_to_knowledge_search() {
        let _guard = RETRIEVAL_GATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (service, conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("general-message-resume-search");
        let knowledge_root = unique_dir("harborbeacon-general-message-loop-search");
        let index_root = unique_dir("harborbeacon-general-message-loop-index");
        fs::create_dir_all(knowledge_root.join("docs")).expect("create docs");
        fs::write(
            knowledge_root.join("docs").join("sakura-journal.md"),
            "这里整理了樱花相关的历史记录。",
        )
        .expect("write doc");
        configure_knowledge_source(&service.admin_store, &knowledge_root, &index_root);

        configure_mock_general_message_llm(
            &service,
            r#"{
                "decision": "clarify",
                "reply_text": "你是想实时拍摄，还是搜索已经保存的樱花内容？",
                "query": "樱花",
                "reason": "need one follow-up"
            }"#,
        );

        let first_request = TaskRequest {
            task_id: "task-general-resume-search-1".to_string(),
            trace_id: "trace-general-resume-search-1".to_string(),
            step_id: "step-general-resume-search-1".to_string(),
            source: TaskSource {
                channel: "weixin".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-general-resume-search".to_string(),
                user_id: "user-1".to_string(),
                session_id: "session-general-resume-search".to_string(),
                route_key: "gw_route_general_resume_search".to_string(),
            },
            intent: TaskIntent {
                domain: "general".to_string(),
                action: "message".to_string(),
                raw_text: "帮我看一下樱花".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "roots": [knowledge_root.to_string_lossy().to_string()]
            }),
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: "om_general_resume_search_1".to_string(),
                chat_type: "p2p".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        };

        let first_response = service.handle_task(first_request);
        assert_eq!(first_response.status, TaskStatus::NeedsInput);
        let resume_token = first_response.resume_token.clone().expect("resume token");

        configure_mock_general_message_llm(
            &service,
            r#"{
                "decision": "knowledge_search",
                "query": "樱花",
                "reason": "user chose stored content"
            }"#,
        );

        let second_request = TaskRequest {
            task_id: "task-general-resume-search-2".to_string(),
            trace_id: "trace-general-resume-search-2".to_string(),
            step_id: "step-general-resume-search-2".to_string(),
            source: TaskSource {
                channel: "weixin".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-general-resume-search".to_string(),
                user_id: "user-1".to_string(),
                session_id: "session-general-resume-search".to_string(),
                route_key: "gw_route_general_resume_search".to_string(),
            },
            intent: TaskIntent {
                domain: "general".to_string(),
                action: "message".to_string(),
                raw_text: "搜索已有内容".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "continuation_token": resume_token,
                "roots": [knowledge_root.to_string_lossy().to_string()]
            }),
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: "om_general_resume_search_2".to_string(),
                chat_type: "p2p".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        };

        let response = service.handle_task(second_request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.executor_used, "knowledge_search_service");
        assert_eq!(
            response.result.data["reply_pack"]["citations"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        let loaded = conversation_store
            .load_for_session(
                "session-general-resume-search",
                Some("chat-general-resume-search"),
            )
            .expect("load conversation")
            .expect("conversation");
        assert!(loaded.general_message_loop().is_none());

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
        let _ = fs::remove_dir_all(knowledge_root);
        let _ = fs::remove_dir_all(index_root);
    }

    #[test]
    fn general_message_capability_summary_short_circuits_before_llm_when_available() {
        let (service, _conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("general-message-capability-llm");
        configure_mock_general_message_llm(
            &service,
            r#"{
                "decision": "capability_summary",
                "reply_text": "我现在可以帮你看摄像头、录短视频，也能搜索已经保存的内容。你想先试哪个？",
                "reason": "user is asking about supported camera capabilities"
            }"#,
        );

        let request = TaskRequest {
            task_id: "task-general-capability-llm".to_string(),
            trace_id: "trace-general-capability-llm".to_string(),
            step_id: "step-general-capability-llm".to_string(),
            source: TaskSource {
                channel: "weixin".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-general-capability-llm".to_string(),
                user_id: "user-1".to_string(),
                session_id: "session-general-capability-llm".to_string(),
                route_key: "gw_route_general_capability_llm".to_string(),
            },
            intent: TaskIntent {
                domain: "general".to_string(),
                action: "message".to_string(),
                raw_text: "摄像头能干什么".to_string(),
            },
            entity_refs: Value::Null,
            args: Value::Null,
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: "om_general_capability_llm".to_string(),
                chat_type: "p2p".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.executor_used, "agentic_interpreter");
        assert!(response.result.message.contains("K3 家庭入口"));
        assert!(response.result.message.contains("不会猜着执行"));
        assert_eq!(
            response.result.data["reply_pack"]["summary"],
            response.result.message
        );
        assert_eq!(
            response.result.data["reply_pack"]["kind"],
            "capability_summary"
        );

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
    }

    #[test]
    fn general_message_boundary_uses_llm_reply_text_when_available() {
        let (service, _conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("general-message-boundary-llm");
        configure_mock_general_message_llm(
            &service,
            r#"{
                "decision": "conversation_boundary",
                "reply_text": "天气这类问题我现在还不能稳定回答，但我可以马上帮你抓拍、录一段，或者查本地保存的内容。",
                "reason": "request is out of current scope"
            }"#,
        );

        let request = TaskRequest {
            task_id: "task-general-unsupported-llm".to_string(),
            trace_id: "trace-general-unsupported-llm".to_string(),
            step_id: "step-general-unsupported-llm".to_string(),
            source: TaskSource {
                channel: "weixin".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-general-unsupported-llm".to_string(),
                user_id: "user-1".to_string(),
                session_id: "session-general-unsupported-llm".to_string(),
                route_key: "gw_route_general_unsupported_llm".to_string(),
            },
            intent: TaskIntent {
                domain: "general".to_string(),
                action: "message".to_string(),
                raw_text: "今天天气怎么样".to_string(),
            },
            entity_refs: Value::Null,
            args: Value::Null,
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: "om_general_unsupported_llm".to_string(),
                chat_type: "p2p".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.executor_used, "agentic_interpreter");
        assert_eq!(
            response.result.message,
            "天气这类问题我现在还不能稳定回答，但我可以马上帮你抓拍、录一段，或者查本地保存的内容。"
        );
        assert_eq!(
            response.result.data["reply_pack"]["summary"],
            response.result.message
        );
        assert_eq!(
            response.result.data["reply_pack"]["kind"],
            "conversation_boundary"
        );

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
    }

    #[test]
    fn general_message_invalid_llm_json_falls_back_to_deterministic_routing() {
        let _guard = RETRIEVAL_GATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (service, _conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("general-message-invalid-json");
        let knowledge_root = unique_dir("harborbeacon-general-message-invalid-json");
        let index_root = unique_dir("harborbeacon-general-message-invalid-index");
        fs::create_dir_all(knowledge_root.join("docs")).expect("create docs");
        fs::write(
            knowledge_root.join("docs").join("sakura-journal.md"),
            "这里整理了樱花相关的历史记录。",
        )
        .expect("write doc");
        configure_knowledge_source(&service.admin_store, &knowledge_root, &index_root);

        configure_mock_general_message_llm(&service, "definitely-not-json");

        let request = TaskRequest {
            task_id: "task-general-invalid-json".to_string(),
            trace_id: "trace-general-invalid-json".to_string(),
            step_id: "step-general-invalid-json".to_string(),
            source: TaskSource {
                channel: "weixin".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-general-invalid-json".to_string(),
                user_id: "user-1".to_string(),
                session_id: "session-general-invalid-json".to_string(),
                route_key: "gw_route_general_invalid_json".to_string(),
            },
            intent: TaskIntent {
                domain: "general".to_string(),
                action: "message".to_string(),
                raw_text: "帮我找到和樱花有关的文件".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "roots": [knowledge_root.to_string_lossy().to_string()]
            }),
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: "om_general_invalid_json".to_string(),
                chat_type: "p2p".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.executor_used, "knowledge_search_service");
        assert_eq!(
            response.result.data["reply_pack"]["citations"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
        let _ = fs::remove_dir_all(knowledge_root);
        let _ = fs::remove_dir_all(index_root);
    }

    #[test]
    fn general_message_router_invalid_label_falls_back_to_conversation_act() {
        let (service, _conversation_store, admin_path, registry_path, conversation_path) =
            build_task_api_service("general-message-router-invalid-label");
        configure_mock_general_message_llm(&service, "camera_snapshot|knowledge_search");

        let request = TaskRequest {
            task_id: "task-general-router-invalid".to_string(),
            trace_id: "trace-general-router-invalid".to_string(),
            step_id: "step-general-router-invalid".to_string(),
            source: TaskSource {
                channel: "weixin".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-general-router-invalid".to_string(),
                user_id: "user-1".to_string(),
                session_id: "session-general-router-invalid".to_string(),
                route_key: "gw_route_general_router_invalid".to_string(),
            },
            intent: TaskIntent {
                domain: "general".to_string(),
                action: "message".to_string(),
                raw_text: "门口咋样".to_string(),
            },
            entity_refs: Value::Null,
            args: Value::Null,
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: "om_general_router_invalid".to_string(),
                chat_type: "p2p".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.executor_used, "agentic_interpreter");
        assert_eq!(
            response.result.data["reply_pack"]["kind"],
            "conversation_continue"
        );
        assert_eq!(
            response.result.data["general_message_controller"]["router_llm"],
            true
        );
        assert_eq!(
            response.result.data["general_message_controller"]["fallback_reason"],
            "nsp_invalid_json_or_decision"
        );
        assert!(!response.result.message.contains("我暂时还不能稳定理解"));

        cleanup_task_api_service(admin_path, registry_path, conversation_path);
    }

    #[test]
    fn general_message_boundary_query_returns_friendly_summary_without_backend_error() {
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let service = TaskApiService::new(
            AdminConsoleStore::new(
                admin_path.clone(),
                DeviceRegistryStore::new(registry_path.clone()),
            ),
            TaskConversationStore::new(conversation_path.clone()),
        );
        let request = TaskRequest {
            task_id: "task-unsupported-general".to_string(),
            trace_id: "trace-unsupported-general".to_string(),
            step_id: "step-unsupported-general".to_string(),
            source: TaskSource {
                channel: "wechat".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-unsupported".to_string(),
                user_id: "user-1".to_string(),
                session_id: "session-unsupported".to_string(),
                route_key: "gw_route_unsupported_general".to_string(),
            },
            intent: TaskIntent {
                domain: "general".to_string(),
                action: "message".to_string(),
                raw_text: "今天的天气怎么样".to_string(),
            },
            entity_refs: Value::Null,
            args: Value::Null,
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: "om_unsupported_general".to_string(),
                chat_type: "p2p".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        };

        let response = service.handle_task(request);

        assert_eq!(response.status, TaskStatus::Completed);
        assert_eq!(response.executor_used, "agentic_interpreter");
        assert_eq!(
            response.result.message,
            response.result.data["reply_pack"]["summary"]
        );
        assert_eq!(
            response.result.data["reply_pack"]["kind"],
            "conversation_boundary"
        );
        assert!(response.result.message.contains("天气"));
        assert!(!response.result.message.contains("LLM endpoint"));
        assert!(!response.result.message.contains("我暂时还不能稳定理解"));

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    #[test]
    fn retrieval_round_trip_launch_pack_covers_explicit_enabled_and_disabled_paths() {
        let _guard = RETRIEVAL_GATE_TEST_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let admin_path = unique_path("harborbeacon-admin-state");
        let registry_path = unique_path("harborbeacon-device-registry");
        let conversation_path = unique_path("harborbeacon-task-runtime");
        let knowledge_root = unique_dir("harborbeacon-knowledge-launch-pack");
        let index_root = unique_dir("harborbeacon-knowledge-index-launch-pack");
        fs::create_dir_all(knowledge_root.join("docs")).expect("create docs");
        fs::create_dir_all(knowledge_root.join("images")).expect("create images");
        fs::create_dir_all(&index_root).expect("create index root");
        fs::write(
            knowledge_root.join("docs").join("sakura-notes.md"),
            "今年花园里的樱花开得很盛，适合做春季归档。",
        )
        .expect("write doc");
        fs::write(
            knowledge_root.join("images").join("spring-garden.jpg"),
            b"fake-image",
        )
        .expect("write image");
        fs::write(
            knowledge_root.join("images").join("spring-garden.json"),
            r#"{"caption":"春天盛开的樱花树","labels":["sakura","spring"]}"#,
        )
        .expect("write sidecar");

        let admin_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        configure_knowledge_source(&admin_store, &knowledge_root, &index_root);
        let service = TaskApiService::new(
            admin_store,
            TaskConversationStore::new(conversation_path.clone()),
        );

        let explicit_request = TaskRequest {
            task_id: "task-launch-explicit".to_string(),
            trace_id: "trace-launch-explicit".to_string(),
            step_id: "step-launch-explicit".to_string(),
            source: TaskSource::default(),
            intent: TaskIntent {
                domain: "knowledge".to_string(),
                action: "search".to_string(),
                raw_text: "搜索樱花文件".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "query": "樱花",
                "roots": [knowledge_root.to_string_lossy().to_string()]
            }),
            autonomy: Default::default(),
            message: None,
        };
        let explicit_response = service.handle_task(explicit_request);
        assert_eq!(explicit_response.status, TaskStatus::Completed);
        assert_eq!(explicit_response.executor_used, "knowledge_search_service");
        assert_eq!(
            explicit_response.result.message,
            explicit_response.result.data["reply_pack"]["summary"]
        );
        assert_eq!(
            explicit_response.result.data["reply_pack"]["citations"]
                .as_array()
                .map(Vec::len),
            Some(2)
        );
        assert_eq!(
            explicit_response.result.data["reply_pack"]["citations"][0]["line_start"],
            1
        );
        assert_eq!(explicit_response.result.artifacts.len(), 2);
        assert_eq!(
            explicit_response.result.artifacts[0].metadata["citation"]["title"],
            "sakura-notes.md"
        );
        assert_eq!(
            explicit_response.result.artifacts[0].metadata["citation"]["line_start"],
            1
        );

        let general_message_request = TaskRequest {
            task_id: "task-launch-enabled".to_string(),
            trace_id: "trace-launch-enabled".to_string(),
            step_id: "step-launch-enabled".to_string(),
            source: TaskSource {
                channel: "wechat".to_string(),
                surface: "harborgate".to_string(),
                conversation_id: "chat-launch".to_string(),
                user_id: "user-1".to_string(),
                session_id: "session-launch".to_string(),
                route_key: "gw_route_launch".to_string(),
            },
            intent: TaskIntent {
                domain: "general".to_string(),
                action: "message".to_string(),
                raw_text: "帮我找到和樱花有关的文件".to_string(),
            },
            entity_refs: Value::Null,
            args: json!({
                "roots": [knowledge_root.to_string_lossy().to_string()]
            }),
            autonomy: Default::default(),
            message: Some(TaskMessage {
                message_id: "om_launch_01".to_string(),
                chat_type: "p2p".to_string(),
                mentions: Vec::new(),
                attachments: Vec::new(),
            }),
        };
        assert!(should_route_general_message_to_knowledge(
            &general_message_request
        ));
        let general_message_response = service.handle_task(general_message_request);
        assert_eq!(general_message_response.status, TaskStatus::Completed);
        assert_eq!(
            general_message_response.executor_used,
            "knowledge_search_service"
        );
        assert_eq!(
            general_message_response.result.message,
            general_message_response.result.data["reply_pack"]["summary"]
        );
        assert_eq!(
            general_message_response.result.data["reply_pack"]["citations"]
                .as_array()
                .map(Vec::len),
            Some(2)
        );
        assert_eq!(general_message_response.result.artifacts.len(), 2);

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
        let _ = fs::remove_dir_all(knowledge_root);
        let _ = fs::remove_dir_all(index_root);
    }
}
