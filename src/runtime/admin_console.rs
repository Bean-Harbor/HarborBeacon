//! Thin persistence layer for the local Agent Hub admin console.

use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use if_addrs::{get_if_addrs, IfAddr};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::connectors::home_assistant::{
    redact_home_assistant_token, token_is_redacted, HOME_ASSISTANT_TOKEN_REDACTION,
};
use crate::control_plane::access::{PermissionBinding, PermissionEffect, ScopeKind};
use crate::control_plane::audit::{
    append_bounded_audit_record, sanitize_audit_stream, AuditRecord,
};
use crate::control_plane::auth::{AuthSource, IdentityBinding};
use crate::control_plane::credentials::{
    CredentialKind, CredentialRecord, CredentialRotationState, ProviderAccount,
    ProviderAccountStatus, ProviderKind, ProviderOwnerScope,
};
use crate::control_plane::media::{RecordingPolicy, RecordingTriggerMode, StorageTargetKind};
use crate::control_plane::models::{
    ModelEndpoint, ModelEndpointKind, ModelEndpointStatus, ModelKind, ModelRoutePolicy,
    PrivacyLevel,
};
use crate::control_plane::users::{
    Membership, MembershipStatus, RoleKind, UserAccount, UserStatus, Workspace, WorkspaceStatus,
    WorkspaceType,
};
use crate::runtime::dvr::{
    dvr_knowledge_root_id, sanitize_dvr_recording_settings, DvrRecordingSettings,
};
use crate::runtime::hub::non_empty_opt;
use crate::runtime::registry::{CameraDevice, DeviceRegistryStore};

const DEFAULT_BINDING_CHANNEL_LABEL: &str = "Harbor HarborGate";
const DEFAULT_PROVIDER_ACCOUNT_DISPLAY_NAME: &str = "Harbor HarborGate";
static ADMIN_CONSOLE_MODEL_DOWNLOAD_WRITE_LOCK: Mutex<()> = Mutex::new(());
static ADMIN_CONSOLE_STATE_FILE_LOCK: Mutex<()> = Mutex::new(());

fn model_download_write_guard() -> Result<MutexGuard<'static, ()>, String> {
    ADMIN_CONSOLE_MODEL_DOWNLOAD_WRITE_LOCK
        .lock()
        .map_err(|_| "model download state write lock poisoned".to_string())
}

fn admin_console_state_file_guard() -> Result<MutexGuard<'static, ()>, String> {
    ADMIN_CONSOLE_STATE_FILE_LOCK
        .lock()
        .map_err(|_| "admin console state file lock poisoned".to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdminBindingState {
    pub status: String,
    pub metric: String,
    pub bound_user: Option<String>,
    pub channel: String,
    #[serde(default = "generate_binding_code")]
    pub session_code: String,
    #[serde(default)]
    pub qr_token: String,
    #[serde(default)]
    pub setup_url: String,
    #[serde(default)]
    pub static_setup_url: String,
}

impl Default for AdminBindingState {
    fn default() -> Self {
        let session_code = generate_binding_code();
        Self {
            status: "等待扫码".to_string(),
            metric: "等待绑定".to_string(),
            bound_user: None,
            channel: DEFAULT_BINDING_CHANNEL_LABEL.to_string(),
            session_code: session_code.clone(),
            qr_token: generate_qr_token(&session_code),
            setup_url: String::new(),
            static_setup_url: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct BridgeProviderCapabilities {
    #[serde(default)]
    pub reply: bool,
    #[serde(default)]
    pub update: bool,
    #[serde(default)]
    pub attachments: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct BridgeProviderConfig {
    #[serde(default)]
    pub configured: bool,
    #[serde(default)]
    pub connected: bool,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub gateway_base_url: String,
    #[serde(default)]
    pub app_id: String,
    #[serde(default)]
    pub app_secret: String,
    #[serde(default)]
    pub app_name: String,
    #[serde(default)]
    pub bot_open_id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub last_checked_at: String,
    #[serde(default)]
    pub capabilities: BridgeProviderCapabilities,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RemoteViewConfig {
    #[serde(default = "default_share_secret")]
    pub share_secret: String,
    #[serde(default = "default_share_link_ttl_minutes")]
    pub share_link_ttl_minutes: u32,
}

impl Default for RemoteViewConfig {
    fn default() -> Self {
        Self {
            share_secret: default_share_secret(),
            share_link_ttl_minutes: default_share_link_ttl_minutes(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IdentityBindingRecord {
    pub open_id: String,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub union_id: Option<String>,
    pub display_name: String,
    #[serde(default)]
    pub chat_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NotificationTargetRecord {
    pub target_id: String,
    pub label: String,
    pub route_key: String,
    #[serde(default)]
    pub platform_hint: String,
    #[serde(default)]
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AutomationRuleReview {
    pub review_id: String,
    #[serde(default = "default_workspace_id")]
    pub workspace_id: String,
    #[serde(default = "default_automation_review_source")]
    pub source: String,
    #[serde(default)]
    pub source_channel: Option<String>,
    #[serde(default)]
    pub source_conversation_id: Option<String>,
    pub original_prompt: String,
    #[serde(default = "default_automation_review_status")]
    pub status: String,
    #[serde(default)]
    pub trigger_definition: Option<Value>,
    #[serde(default)]
    pub condition_definition: Option<Value>,
    #[serde(default)]
    pub action_plan: Option<Value>,
    #[serde(default)]
    pub device_refs: Vec<Value>,
    #[serde(default)]
    pub risk_level: Option<String>,
    #[serde(default)]
    pub requires_approval: bool,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub rule_id: Option<String>,
    #[serde(default)]
    pub run_summaries: Vec<Value>,
    #[serde(default)]
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AdminDefaults {
    pub cidr: String,
    pub discovery: String,
    pub recording: String,
    pub capture: String,
    pub ai: String,
    #[serde(alias = "feishu_group")]
    pub notification_channel: String,
    #[serde(default = "default_rtsp_username")]
    pub rtsp_username: String,
    #[serde(default)]
    pub rtsp_password: String,
    #[serde(default = "default_rtsp_port")]
    pub rtsp_port: u16,
    #[serde(default = "default_rtsp_paths")]
    pub rtsp_paths: Vec<String>,
    #[serde(default)]
    pub selected_camera_device_id: Option<String>,
    #[serde(default = "default_capture_subdirectory")]
    pub capture_subdirectory: String,
    #[serde(default = "default_clip_length_seconds")]
    pub clip_length_seconds: u32,
    #[serde(default = "default_keyframe_count")]
    pub keyframe_count: u32,
    #[serde(default = "default_keyframe_interval_seconds")]
    pub keyframe_interval_seconds: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnowledgeSourceRoot {
    #[serde(default)]
    pub root_id: String,
    #[serde(default)]
    pub label: String,
    pub path: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub last_indexed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KnowledgeRetrievalSettings {
    #[serde(default = "default_retrieval_fusion_strategy")]
    pub fusion_strategy: String,
    #[serde(default = "default_retrieval_rrf_k")]
    pub rrf_k: f32,
    #[serde(default = "default_retrieval_lexical_weight")]
    pub lexical_weight: f32,
    #[serde(default = "default_retrieval_vector_weight")]
    pub vector_weight: f32,
    #[serde(default = "default_retrieval_candidate_limit")]
    pub candidate_limit: usize,
    #[serde(default = "default_retrieval_vector_min_score")]
    pub vector_min_score: f32,
    #[serde(default = "default_retrieval_semantic_only_min_score")]
    pub semantic_only_min_score: f32,
    #[serde(default = "default_true")]
    pub rerank_enabled: bool,
    #[serde(default = "default_retrieval_rerank_top_k")]
    pub rerank_top_k: usize,
    #[serde(default = "default_retrieval_rerank_min_score")]
    pub rerank_min_score: f32,
    #[serde(default = "default_true")]
    pub mmr_enabled: bool,
    #[serde(default = "default_retrieval_mmr_lambda")]
    pub mmr_lambda: f32,
}

impl Default for KnowledgeRetrievalSettings {
    fn default() -> Self {
        Self {
            fusion_strategy: default_retrieval_fusion_strategy(),
            rrf_k: default_retrieval_rrf_k(),
            lexical_weight: default_retrieval_lexical_weight(),
            vector_weight: default_retrieval_vector_weight(),
            candidate_limit: default_retrieval_candidate_limit(),
            vector_min_score: default_retrieval_vector_min_score(),
            semantic_only_min_score: default_retrieval_semantic_only_min_score(),
            rerank_enabled: true,
            rerank_top_k: default_retrieval_rerank_top_k(),
            rerank_min_score: default_retrieval_rerank_min_score(),
            mmr_enabled: true,
            mmr_lambda: default_retrieval_mmr_lambda(),
        }
    }
}

fn default_retrieval_fusion_strategy() -> String {
    "rrf".to_string()
}

fn default_retrieval_rrf_k() -> f32 {
    60.0
}

fn default_retrieval_lexical_weight() -> f32 {
    0.35
}

fn default_retrieval_vector_weight() -> f32 {
    0.65
}

fn default_retrieval_candidate_limit() -> usize {
    80
}

fn default_retrieval_vector_min_score() -> f32 {
    0.25
}

fn default_retrieval_semantic_only_min_score() -> f32 {
    0.55
}

fn default_retrieval_rerank_top_k() -> usize {
    30
}

fn default_retrieval_rerank_min_score() -> f32 {
    0.15
}

fn default_retrieval_mmr_lambda() -> f32 {
    0.70
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct KnowledgeSettings {
    #[serde(default)]
    pub source_roots: Vec<KnowledgeSourceRoot>,
    #[serde(default = "default_knowledge_index_root")]
    pub index_root: String,
    #[serde(default)]
    pub privacy_level: PrivacyLevel,
    #[serde(default)]
    pub default_resource_profile: RagResourceProfile,
    #[serde(default)]
    pub retrieval: KnowledgeRetrievalSettings,
}

impl Default for KnowledgeSettings {
    fn default() -> Self {
        Self {
            source_roots: Vec::new(),
            index_root: default_knowledge_index_root(),
            privacy_level: PrivacyLevel::StrictLocal,
            default_resource_profile: RagResourceProfile::CpuOnly,
            retrieval: KnowledgeRetrievalSettings::default(),
        }
    }
}

impl KnowledgeSettings {
    pub fn enabled_source_root_paths(&self) -> Vec<String> {
        self.source_roots
            .iter()
            .filter(|root| root.enabled)
            .map(|root| root.path.trim())
            .filter(|path| !path.is_empty())
            .map(ToString::to_string)
            .collect()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RagResourceProfile {
    #[default]
    CpuOnly,
    LocalGpu,
    SidecarGpu,
    CloudAllowed,
}

impl RagResourceProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CpuOnly => "cpu_only",
            Self::LocalGpu => "local_gpu",
            Self::SidecarGpu => "sidecar_gpu",
            Self::CloudAllowed => "cloud_allowed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct KnowledgeIndexJobRecord {
    pub job_id: String,
    pub source_root_id: String,
    pub source_root_label: String,
    pub source_root_path: String,
    #[serde(default)]
    pub modalities: Vec<String>,
    pub status: String,
    #[serde(default)]
    pub progress_percent: Option<u8>,
    #[serde(default)]
    pub requested_at: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub error_message: Option<String>,
    #[serde(default)]
    pub retry_count: u32,
    #[serde(default)]
    pub checkpoint: Value,
    #[serde(default)]
    pub resource_profile: RagResourceProfile,
    #[serde(default)]
    pub cancel_requested: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct DeviceCredentialSecret {
    pub device_id: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub rtsp_port: Option<u16>,
    #[serde(default)]
    pub rtsp_paths: Vec<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub last_verified_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DeviceEvidenceRecord {
    pub evidence_id: String,
    pub device_id: String,
    pub evidence_kind: String,
    pub status: String,
    pub observed_at: String,
    pub summary: String,
    #[serde(default)]
    pub details: Value,
}

impl Default for AdminDefaults {
    fn default() -> Self {
        Self {
            cidr: default_scan_cidr(),
            // Prefer ONVIF WS-Discovery when available, fall back to RTSP probe for legacy cameras.
            discovery: "ONVIF + RTSP".to_string(),
            recording: "按事件录制".to_string(),
            capture: "图片 + 摘要".to_string(),
            ai: "人体检测 + 中文摘要".to_string(),
            notification_channel: "家庭通知频道".to_string(),
            rtsp_username: default_rtsp_username(),
            rtsp_password: String::new(),
            rtsp_port: default_rtsp_port(),
            rtsp_paths: default_rtsp_paths(),
            selected_camera_device_id: None,
            capture_subdirectory: default_capture_subdirectory(),
            clip_length_seconds: default_clip_length_seconds(),
            keyframe_count: default_keyframe_count(),
            keyframe_interval_seconds: default_keyframe_interval_seconds(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminPlatformState {
    #[serde(default)]
    pub workspaces: Vec<Workspace>,
    #[serde(default)]
    pub users: Vec<UserAccount>,
    #[serde(default)]
    pub memberships: Vec<Membership>,
    #[serde(default)]
    pub identity_bindings: Vec<IdentityBinding>,
    #[serde(default)]
    pub permission_bindings: Vec<PermissionBinding>,
    #[serde(default)]
    pub provider_accounts: Vec<ProviderAccount>,
    #[serde(default)]
    pub credentials: Vec<CredentialRecord>,
    #[serde(default)]
    pub recording_policies: Vec<RecordingPolicy>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AdminModelCenterState {
    #[serde(default)]
    pub endpoints: Vec<ModelEndpoint>,
    #[serde(default)]
    pub route_policies: Vec<ModelRoutePolicy>,
    #[serde(default = "default_model_store_root")]
    pub model_store_root: String,
    #[serde(default)]
    pub capability_bindings: Vec<ModelCapabilityBindingRecord>,
    #[serde(default)]
    pub runtimes: Vec<ModelRuntimeRecord>,
}

impl Default for AdminModelCenterState {
    fn default() -> Self {
        Self {
            endpoints: default_model_endpoints(),
            route_policies: default_model_route_policies(),
            model_store_root: default_model_store_root(),
            capability_bindings: Vec::new(),
            runtimes: default_model_runtimes(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelCapabilityBindingRecord {
    pub capability_id: String,
    pub model_id: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelRuntimeRecord {
    pub runtime_id: String,
    pub display_name: String,
    pub runtime_kind: String,
    pub provider_key: String,
    pub status: String,
    #[serde(default)]
    pub managed: bool,
    #[serde(default)]
    pub installable: bool,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub runtime_profiles: Vec<String>,
    #[serde(default)]
    pub bind_url: Option<String>,
    #[serde(default)]
    pub healthz_url: Option<String>,
    #[serde(default)]
    pub model_store_path: String,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub installed_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelDownloadJobRecord {
    pub job_id: String,
    pub model_id: String,
    pub display_name: String,
    pub provider_key: String,
    pub status: String,
    pub requested_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub target_path: Option<String>,
    #[serde(default)]
    pub progress_percent: Option<u8>,
    #[serde(default)]
    pub bytes_downloaded: Option<u64>,
    #[serde(default)]
    pub total_bytes: Option<u64>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub error_message: Option<String>,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelDownloadJobCreateResult {
    pub job: ModelDownloadJobRecord,
    pub should_spawn_worker: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AdminConsoleState {
    #[serde(default)]
    pub binding: AdminBindingState,
    #[serde(default)]
    pub defaults: AdminDefaults,
    #[serde(default, alias = "feishu_bot")]
    pub bridge_provider: BridgeProviderConfig,
    #[serde(default)]
    pub remote_view: RemoteViewConfig,
    #[serde(default)]
    pub dvr: DvrRecordingSettings,
    #[serde(default, alias = "feishu_users")]
    pub identity_bindings: Vec<IdentityBindingRecord>,
    #[serde(default)]
    pub notification_targets: Vec<NotificationTargetRecord>,
    #[serde(default)]
    pub automation_reviews: Vec<AutomationRuleReview>,
    #[serde(default)]
    pub device_credentials: Vec<DeviceCredentialSecret>,
    #[serde(default)]
    pub device_evidence: Vec<DeviceEvidenceRecord>,
    #[serde(default)]
    pub platform: AdminPlatformState,
    #[serde(default)]
    pub models: AdminModelCenterState,
    #[serde(default)]
    pub model_download_jobs: Vec<ModelDownloadJobRecord>,
    #[serde(default)]
    pub knowledge: KnowledgeSettings,
    #[serde(default)]
    pub knowledge_index_jobs: Vec<KnowledgeIndexJobRecord>,
    #[serde(default)]
    pub home_assistant: HomeAssistantAdminState,
    #[serde(default)]
    pub audit_records: Vec<AuditRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HomeAssistantAdminState {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub access_token: String,
    #[serde(default)]
    pub exposed_domains: Vec<String>,
    #[serde(default)]
    pub last_status: String,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_test_at: Option<String>,
    #[serde(default)]
    pub last_sync_at: Option<String>,
    #[serde(default)]
    pub entity_count: usize,
    #[serde(default)]
    pub service_count: usize,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub location_name: Option<String>,
}

impl Default for HomeAssistantAdminState {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: String::new(),
            access_token: String::new(),
            exposed_domains: default_home_assistant_exposed_domains(),
            last_status: "not_configured".to_string(),
            last_error: None,
            last_test_at: None,
            last_sync_at: None,
            entity_count: 0,
            service_count: 0,
            version: None,
            location_name: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct HomeAssistantConfigUpdate {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub access_token: Option<String>,
    #[serde(default)]
    pub exposed_domains: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceSummary {
    pub workspace_id: String,
    pub display_name: String,
    pub workspace_type: String,
    pub status: String,
    pub timezone: String,
    pub locale: String,
    pub owner_user_id: String,
    pub member_count: usize,
    pub active_member_count: usize,
    pub identity_binding_count: usize,
    pub permission_rule_count: usize,
    pub provider_account_count: usize,
    pub credential_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_principal_user_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_principal_display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_principal_auth_source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemberRoleSummary {
    pub role_kind: String,
    pub member_count: usize,
    pub active_member_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IdentityBindingSummary {
    pub identity_id: String,
    pub user_id: String,
    pub display_name: String,
    pub provider_key: String,
    pub open_id: String,
    #[serde(default)]
    pub union_id: Option<String>,
    #[serde(default)]
    pub chat_id: Option<String>,
    pub role_kind: String,
    pub membership_status: String,
    pub can_edit: bool,
    pub is_owner: bool,
    #[serde(default)]
    pub proactive_delivery_surface: String,
    #[serde(default)]
    pub binding_availability: String,
    #[serde(default)]
    pub binding_available: bool,
    #[serde(default)]
    pub binding_availability_note: String,
    #[serde(default)]
    pub recent_interactive_surface: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoleGovernanceSummary {
    pub role_kind: String,
    pub permission_rule_count: usize,
    pub member_count: usize,
    pub active_member_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccessGovernanceSummary {
    pub permission_rule_count: usize,
    pub owner_count: usize,
    pub member_count: usize,
    pub active_member_count: usize,
    pub role_policies: Vec<RoleGovernanceSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GatewayStatusSummary {
    pub binding_channel: String,
    pub binding_status: String,
    pub binding_metric: String,
    #[serde(default)]
    pub binding_bound_user: Option<String>,
    #[serde(default)]
    pub manage_url: String,
    #[serde(default)]
    pub setup_url: String,
    #[serde(default)]
    pub static_setup_url: String,
    pub bridge_provider: BridgeProviderConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliveryPolicySummary {
    pub interactive_reply: String,
    pub proactive_delivery: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountManagementSnapshot {
    pub workspace: WorkspaceSummary,
    pub member_role_counts: Vec<MemberRoleSummary>,
    pub identity_bindings: Vec<IdentityBindingSummary>,
    pub access_governance: AccessGovernanceSummary,
    pub gateway: GatewayStatusSummary,
    #[serde(default)]
    pub notification_targets: Vec<NotificationTargetRecord>,
    pub delivery_policy: DeliveryPolicySummary,
}

#[derive(Debug, Clone)]
pub struct AdminConsoleStore {
    path: PathBuf,
    registry_store: DeviceRegistryStore,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AdminConsoleStateStats {
    pub exists: bool,
    pub file_bytes: u64,
    pub automation_review_count: usize,
    pub audit_record_count: usize,
    pub notification_target_count: usize,
    pub device_credential_count: usize,
    pub platform_credential_count: usize,
    pub platform_credential_duplicate_count: usize,
    pub knowledge_index_job_count: usize,
    pub model_download_job_count: usize,
    pub top_level_section_sizes: Value,
    pub section_size_scan_skipped: bool,
    pub metadata_only: bool,
}

const DEFAULT_WORKSPACE_ID: &str = "home-1";
const DEFAULT_WORKSPACE_OWNER_ID: &str = "local-owner";
const LOCAL_RTSP_PROVIDER_ACCOUNT_ID: &str = "provider-local-rtsp";
const LOCAL_RTSP_CREDENTIAL_ID: &str = "credential-local-rtsp-password";
const BRIDGE_PROVIDER_ACCOUNT_ID: &str = "provider-im-bridge";
const DEFAULT_RECORDING_POLICY_ID: &str = "recording-policy-default";
const DEFAULT_MODEL_WORKSPACE_ID: &str = "home-1";
const DEFAULT_POLICY_RETRIEVAL_OCR: &str = "retrieval.ocr";
const DEFAULT_POLICY_RETRIEVAL_EMBED: &str = "retrieval.embed";
const DEFAULT_POLICY_RETRIEVAL_RERANK: &str = "retrieval.rerank";
const DEFAULT_POLICY_RETRIEVAL_ANSWER: &str = "retrieval.answer";
const DEFAULT_POLICY_RETRIEVAL_VISION_SUMMARY: &str = "retrieval.vision_summary";
const DEFAULT_POLICY_SEMANTIC_ROUTER: &str = "semantic.router";
const DEFAULT_SILICONFLOW_ENDPOINT_ID: &str = "llm-cloud-siliconflow";
const DEFAULT_SILICONFLOW_BASE_URL: &str = "https://api.siliconflow.cn/v1";
const DEFAULT_SILICONFLOW_MODEL: &str = "deepseek-ai/DeepSeek-V4-Flash";
const DEFAULT_PROACTIVE_DELIVERY_SURFACE: &str = "feishu";
const HARBOROS_CURRENT_USER_ENV: &str = "HARBOR_HARBOROS_USER";
const HARBOROS_WRITABLE_ROOT_ENV: &str = "HARBOR_HARBOROS_WRITABLE_ROOT";
const DEFAULT_HARBOROS_WRITABLE_ROOT: &str = "/mnt/software/harborbeacon-agent-ci";
const DEFAULT_KNOWLEDGE_INDEX_SUBDIR: &str = "knowledge-index";
const MODEL_API_BASE_URL_ENV: &str = "HARBOR_MODEL_API_BASE_URL";

fn default_workspace_id() -> String {
    DEFAULT_WORKSPACE_ID.to_string()
}

fn default_automation_review_source() -> String {
    "harbor_assistant".to_string()
}

fn default_automation_review_status() -> String {
    "draft".to_string()
}
const MODEL_API_TOKEN_ENV: &str = "HARBOR_MODEL_API_TOKEN";
const DEFAULT_MODEL_API_BASE_URL: &str = "http://127.0.0.1:4174/api/inference/v1";
const DEFAULT_MODEL_API_TOKEN: &str = "harbor-local-model-token";

impl AdminConsoleStore {
    pub fn new(path: impl Into<PathBuf>, registry_store: DeviceRegistryStore) -> Self {
        Self {
            path: path.into(),
            registry_store,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load_state(&self) -> Result<AdminConsoleState, String> {
        if !self.path.exists() {
            return self.bootstrap_state();
        }

        let _guard = admin_console_state_file_guard()?;
        let text = fs::read_to_string(&self.path).map_err(|e| {
            format!(
                "failed to read admin console state {}: {e}",
                self.path.display()
            )
        })?;
        let mut state: AdminConsoleState = serde_json::from_str(&text).map_err(|e| {
            format!(
                "failed to parse admin console state {}: {e}",
                self.path.display()
            )
        })?;
        self.apply_registry_hints(&mut state)?;
        Ok(state)
    }

    pub fn state_stats(&self, state: Option<&AdminConsoleState>) -> AdminConsoleStateStats {
        let file_bytes = fs::metadata(&self.path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let mut stats = AdminConsoleStateStats {
            exists: self.path.exists(),
            file_bytes,
            automation_review_count: 0,
            audit_record_count: 0,
            notification_target_count: 0,
            device_credential_count: 0,
            platform_credential_count: 0,
            platform_credential_duplicate_count: 0,
            knowledge_index_job_count: 0,
            model_download_job_count: 0,
            top_level_section_sizes: json!({
                "status": "skipped",
                "reason": "section byte sizing is skipped for large admin state files",
            }),
            section_size_scan_skipped: true,
            metadata_only: true,
        };
        if let Some(state) = state {
            stats.automation_review_count = state.automation_reviews.len();
            stats.audit_record_count = state.audit_records.len();
            stats.notification_target_count = state.notification_targets.len();
            stats.device_credential_count = state.device_credentials.len();
            stats.platform_credential_count = state.platform.credentials.len();
            let mut credential_ids = HashSet::new();
            stats.platform_credential_duplicate_count = state
                .platform
                .credentials
                .iter()
                .filter(|credential| !credential_ids.insert(credential.credential_id.clone()))
                .count();
            stats.knowledge_index_job_count = state.knowledge_index_jobs.len();
            stats.model_download_job_count = state.model_download_jobs.len();
            if file_bytes <= 4 * 1024 * 1024 {
                if let Ok(value) = serde_json::to_value(state) {
                    if let Some(object) = value.as_object() {
                        let mut sizes = serde_json::Map::new();
                        for (key, value) in object {
                            let bytes = serde_json::to_vec(value)
                                .map(|bytes| bytes.len())
                                .unwrap_or(0);
                            sizes.insert(key.clone(), json!(bytes));
                        }
                        stats.top_level_section_sizes = Value::Object(sizes);
                        stats.section_size_scan_skipped = false;
                    }
                }
            }
        }
        stats
    }

    fn save_state(&self, state: &AdminConsoleState) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|e| {
                format!(
                    "failed to create admin console directory {}: {e}",
                    parent.display()
                )
            })?;
        }

        let mut sanitized = state.clone();
        sanitize_admin_state(&mut sanitized);
        let payload = serde_json::to_string_pretty(&sanitized).map_err(|e| {
            format!(
                "failed to serialize admin console state {}: {e}",
                self.path.display()
            )
        })?;
        let _guard = admin_console_state_file_guard()?;
        fs::write(&self.path, payload).map_err(|e| {
            format!(
                "failed to write admin console state {}: {e}",
                self.path.display()
            )
        })
    }

    fn save_platform_primary_state(
        &self,
        mut platform_state: AdminConsoleState,
    ) -> Result<AdminConsoleState, String> {
        hydrate_legacy_views_from_platform(&mut platform_state);
        sanitize_legacy_admin_fields(&mut platform_state);
        platform_state.platform = sync_platform_from_legacy(&platform_state);
        self.save_state(&platform_state)?;
        Ok(platform_state)
    }

    fn save_projected_state(
        &self,
        mut projected_state: AdminConsoleState,
    ) -> Result<AdminConsoleState, String> {
        sanitize_legacy_admin_fields(&mut projected_state);
        projected_state.platform = sync_platform_from_legacy(&projected_state);
        hydrate_legacy_views_from_platform(&mut projected_state);
        self.save_state(&projected_state)?;
        Ok(projected_state)
    }

    pub fn load_or_create_state(&self) -> Result<AdminConsoleState, String> {
        let state = self.load_state()?;
        self.save_state(&state)?;
        Ok(state)
    }

    pub fn home_assistant_state(&self) -> Result<HomeAssistantAdminState, String> {
        Ok(redact_home_assistant_state(
            self.load_state()?.home_assistant,
        ))
    }

    pub fn home_assistant_secret_state(&self) -> Result<HomeAssistantAdminState, String> {
        Ok(self.load_state()?.home_assistant)
    }

    pub fn save_home_assistant_config(
        &self,
        update: HomeAssistantConfigUpdate,
    ) -> Result<AdminConsoleState, String> {
        let mut state = self.load_or_create_state()?;
        let mut next = state.home_assistant.clone();
        next.enabled = update.enabled;
        next.base_url = update.base_url;
        next.exposed_domains = update.exposed_domains;
        if let Some(access_token) = update.access_token {
            if !token_is_redacted(&access_token) {
                next.access_token = access_token;
            }
        }
        next.last_error = None;
        if !next.enabled {
            next.last_status = "disabled".to_string();
        } else if next.base_url.trim().is_empty() || next.access_token.trim().is_empty() {
            next.last_status = "not_configured".to_string();
        } else {
            next.last_status = "configured".to_string();
        }
        state.home_assistant = sanitize_home_assistant_state(next);
        self.save_projected_state(state)
    }

    pub fn record_home_assistant_test(
        &self,
        ok: bool,
        version: Option<String>,
        location_name: Option<String>,
        error: Option<String>,
    ) -> Result<AdminConsoleState, String> {
        let mut state = self.load_or_create_state()?;
        state.home_assistant.last_test_at = Some(model_test_timestamp());
        state.home_assistant.last_status = if ok { "connected" } else { "error" }.to_string();
        state.home_assistant.last_error = error;
        state.home_assistant.version = version;
        state.home_assistant.location_name = location_name;
        state.home_assistant = sanitize_home_assistant_state(state.home_assistant);
        self.save_projected_state(state)
    }

    pub fn record_home_assistant_sync(
        &self,
        entity_count: usize,
        service_count: usize,
    ) -> Result<AdminConsoleState, String> {
        let mut state = self.load_or_create_state()?;
        state.home_assistant.last_status = "synced".to_string();
        state.home_assistant.last_error = None;
        state.home_assistant.last_sync_at = Some(model_test_timestamp());
        state.home_assistant.entity_count = entity_count;
        state.home_assistant.service_count = service_count;
        state.home_assistant = sanitize_home_assistant_state(state.home_assistant);
        self.save_projected_state(state)
    }

    pub fn refresh_binding_qr(&self) -> Result<AdminConsoleState, String> {
        let mut state = self.load_or_create_state()?;
        state.binding = AdminBindingState::default();
        self.save_projected_state(state)
    }

    pub fn mark_demo_bound(&self, user_name: &str) -> Result<AdminConsoleState, String> {
        let mut state = self.load_or_create_state()?;
        state.binding.status = "已绑定".to_string();
        state.binding.metric = "已绑定".to_string();
        state.binding.bound_user = Some(user_name.to_string());
        self.save_projected_state(state)
    }

    pub fn bind_identity_user(
        &self,
        token_or_code: &str,
        user: IdentityBindingRecord,
    ) -> Result<AdminConsoleState, String> {
        let mut state = self.load_or_create_state()?;
        let provided_code =
            normalize_binding_code(token_or_code).ok_or_else(|| "绑定码格式不正确".to_string())?;
        if provided_code != state.binding.session_code {
            return Err(format!(
                "绑定码不匹配，当前有效绑定码是 {}",
                state.binding.session_code
            ));
        }

        let user = sanitize_identity_binding_record(user)?;
        let workspace = state
            .platform
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == DEFAULT_WORKSPACE_ID)
            .or_else(|| state.platform.workspaces.first())
            .cloned()
            .ok_or_else(|| "当前没有可编辑的 workspace".to_string())?;
        let projected_user_id = projected_user_id_for_binding(&user);

        state.binding.status = "已绑定".to_string();
        state.binding.metric = "已绑定".to_string();
        state.binding.bound_user = Some(user.display_name.clone());

        upsert_user(
            &mut state.platform.users,
            build_user_account_projection(&user, &workspace.workspace_id),
        );
        upsert_identity_binding(
            &mut state.platform.identity_bindings,
            build_identity_binding_projection(&user),
        );

        if projected_user_id != workspace.owner_user_id {
            if let Some(existing) = state.platform.memberships.iter_mut().find(|membership| {
                membership.workspace_id == workspace.workspace_id
                    && membership.user_id == projected_user_id
            }) {
                existing.status = MembershipStatus::Active;
            } else {
                state
                    .platform
                    .memberships
                    .push(build_membership_projection(&workspace, &user));
            }
        }

        if let Some(workspace) = preferred_workspace_mut(&mut state.platform) {
            set_workspace_binding_projection(workspace, &state.binding);
        }

        self.save_platform_primary_state(state)
    }

    pub fn upsert_notification_target(
        &self,
        target_id: Option<&str>,
        label: &str,
        route_key: &str,
        platform_hint: &str,
        make_default: bool,
    ) -> Result<AdminConsoleState, String> {
        let mut state = self.load_or_create_state()?;
        let label = sanitize_notification_target_label(label)?;
        let route_key = sanitize_notification_target_route_key(route_key)?;
        let platform_hint = normalize_platform_hint(platform_hint);
        let requested_target_id = target_id
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string());

        let target_index = requested_target_id
            .as_deref()
            .and_then(|target_id| {
                state
                    .notification_targets
                    .iter()
                    .position(|target| target.target_id == target_id)
            })
            .or_else(|| {
                state
                    .notification_targets
                    .iter()
                    .position(|target| target.route_key == route_key)
            });

        let target_id = if let Some(index) = target_index {
            let target = state
                .notification_targets
                .get_mut(index)
                .expect("notification target index should remain valid");
            target.label = label;
            target.route_key = route_key;
            target.platform_hint = platform_hint;
            target.target_id.clone()
        } else {
            let target_id = requested_target_id.unwrap_or_else(new_notification_target_id);
            state.notification_targets.push(NotificationTargetRecord {
                target_id: target_id.clone(),
                label,
                route_key,
                platform_hint,
                is_default: false,
            });
            target_id
        };

        let should_make_default = make_default
            || state
                .notification_targets
                .iter()
                .all(|target| !target.is_default);
        if should_make_default {
            for target in &mut state.notification_targets {
                target.is_default = target.target_id == target_id;
            }
        }

        state.notification_targets = sanitize_notification_targets(state.notification_targets);
        self.save_projected_state(state)
    }

    pub fn set_default_notification_target(
        &self,
        target_id: &str,
    ) -> Result<AdminConsoleState, String> {
        let target_id = target_id.trim();
        if target_id.is_empty() {
            return Err("target_id 不能为空".to_string());
        }

        let mut state = self.load_or_create_state()?;
        if !state
            .notification_targets
            .iter()
            .any(|target| target.target_id == target_id)
        {
            return Err(format!("未找到 notification target {target_id}"));
        }
        for target in &mut state.notification_targets {
            target.is_default = target.target_id == target_id;
        }
        state.notification_targets = sanitize_notification_targets(state.notification_targets);
        self.save_projected_state(state)
    }

    pub fn delete_notification_target(&self, target_id: &str) -> Result<AdminConsoleState, String> {
        let target_id = target_id.trim();
        if target_id.is_empty() {
            return Err("target_id 不能为空".to_string());
        }

        let mut state = self.load_or_create_state()?;
        let original_len = state.notification_targets.len();
        state
            .notification_targets
            .retain(|target| target.target_id != target_id);
        if state.notification_targets.len() == original_len {
            return Err(format!("未找到 notification target {target_id}"));
        }
        state.notification_targets = sanitize_notification_targets(state.notification_targets);
        self.save_projected_state(state)
    }

    pub fn upsert_automation_review(
        &self,
        review: AutomationRuleReview,
    ) -> Result<AdminConsoleState, String> {
        let mut state = self.load_or_create_state()?;
        let mut review = sanitize_automation_review(review)?;
        let now = model_test_timestamp();
        if review.created_at.is_none() {
            review.created_at = Some(now.clone());
        }
        review.updated_at = Some(now);

        if let Some(existing) = state
            .automation_reviews
            .iter_mut()
            .find(|existing| existing.review_id == review.review_id)
        {
            *existing = review;
        } else {
            state.automation_reviews.push(review);
        }

        state.automation_reviews = sanitize_automation_reviews(state.automation_reviews);
        self.save_projected_state(state)
    }

    pub fn set_automation_review_status(
        &self,
        review_id: &str,
        status: &str,
    ) -> Result<AdminConsoleState, String> {
        let review_id = review_id.trim();
        if review_id.is_empty() {
            return Err("review_id 不能为空".to_string());
        }
        let normalized_status = sanitize_automation_review_status(status)?;
        let mut state = self.load_or_create_state()?;
        let Some(review) = state
            .automation_reviews
            .iter_mut()
            .find(|review| review.review_id == review_id)
        else {
            return Err(format!("未找到 automation review {review_id}"));
        };
        review.status = normalized_status;
        review.updated_at = Some(model_test_timestamp());
        state.automation_reviews = sanitize_automation_reviews(state.automation_reviews);
        self.save_projected_state(state)
    }

    pub fn set_member_role(
        &self,
        user_id: &str,
        role_kind: RoleKind,
    ) -> Result<AdminConsoleState, String> {
        let user_id = user_id.trim();
        if user_id.is_empty() {
            return Err("user_id 不能为空".to_string());
        }
        if role_kind == RoleKind::Owner {
            return Err("当前入口不支持把成员直接提升为 owner".to_string());
        }

        let mut state = self.load_or_create_state()?;
        let workspace = state
            .platform
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == DEFAULT_WORKSPACE_ID)
            .or_else(|| state.platform.workspaces.first())
            .cloned()
            .ok_or_else(|| "当前没有可编辑的 workspace".to_string())?;

        if user_id == workspace.owner_user_id {
            return Err("本地 owner 角色不能在这里修改".to_string());
        }

        ensure_platform_user_exists(&mut state, &workspace.workspace_id, user_id)?;

        if let Some(membership) = state.platform.memberships.iter_mut().find(|membership| {
            membership.workspace_id == workspace.workspace_id && membership.user_id == user_id
        }) {
            membership.role_kind = role_kind;
            membership.status = MembershipStatus::Active;
        } else {
            state.platform.memberships.push(Membership {
                membership_id: format!("membership-{user_id}"),
                workspace_id: workspace.workspace_id,
                user_id: user_id.to_string(),
                role_kind,
                status: MembershipStatus::Active,
                granted_by_user_id: Some(workspace.owner_user_id),
                granted_at: None,
            });
        }

        self.save_platform_primary_state(state)
    }

    pub fn set_member_default_delivery_surface(
        &self,
        user_id: &str,
        surface: &str,
    ) -> Result<AdminConsoleState, String> {
        let user_id = user_id.trim();
        if user_id.is_empty() {
            return Err("user_id 不能为空".to_string());
        }
        let surface = normalize_delivery_surface(surface)
            .ok_or_else(|| "surface 只能是 feishu 或 weixin".to_string())?;

        let mut state = self.load_or_create_state()?;
        let workspace = state
            .platform
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == DEFAULT_WORKSPACE_ID)
            .or_else(|| state.platform.workspaces.first())
            .cloned()
            .ok_or_else(|| "当前没有可编辑的 workspace".to_string())?;

        ensure_platform_user_exists(&mut state, &workspace.workspace_id, user_id)?;

        let user = state
            .platform
            .users
            .iter_mut()
            .find(|user| user.user_id == user_id)
            .ok_or_else(|| format!("未找到 user_id={user_id} 对应的平台用户"))?;
        set_user_default_delivery_surface(user, &surface);

        self.save_platform_primary_state(state)
    }

    pub fn record_member_interactive_surface(
        &self,
        user_id: &str,
        surface: &str,
        route_key: Option<&str>,
    ) -> Result<(), String> {
        let user_id = user_id.trim();
        if user_id.is_empty() {
            return Ok(());
        }
        let surface =
            normalize_delivery_surface(surface).unwrap_or_else(|| surface.trim().to_string());
        if surface.is_empty() {
            return Ok(());
        }

        let mut state = self.load_or_create_state()?;
        let workspace = state
            .platform
            .workspaces
            .iter()
            .find(|workspace| workspace.workspace_id == DEFAULT_WORKSPACE_ID)
            .or_else(|| state.platform.workspaces.first())
            .cloned()
            .ok_or_else(|| "当前没有可编辑的 workspace".to_string())?;
        ensure_platform_user_exists(&mut state, &workspace.workspace_id, user_id)?;

        if let Some(user) = state
            .platform
            .users
            .iter_mut()
            .find(|user| user.user_id == user_id)
        {
            set_user_recent_interactive_surface(user, &surface, route_key);
            self.save_platform_primary_state(state)?;
        }

        Ok(())
    }

    pub fn save_defaults(&self, defaults: AdminDefaults) -> Result<AdminConsoleState, String> {
        let mut state = self.load_or_create_state()?;
        state.defaults = sanitize_defaults(defaults);
        self.save_projected_state(state)
    }

    pub fn dvr_recording_settings(&self) -> Result<DvrRecordingSettings, String> {
        Ok(sanitize_dvr_recording_settings(
            self.load_or_create_state()?.dvr,
        ))
    }

    pub fn save_dvr_recording_settings(
        &self,
        settings: DvrRecordingSettings,
    ) -> Result<AdminConsoleState, String> {
        let settings = sanitize_dvr_recording_settings(settings);
        fs::create_dir_all(&settings.recording_root).map_err(|error| {
            format!(
                "failed to create DVR recording_root {}: {error}",
                settings.recording_root
            )
        })?;
        fs::create_dir_all(&settings.media_library_root).map_err(|error| {
            format!(
                "failed to create DVR media_library_root {}: {error}",
                settings.media_library_root
            )
        })?;
        let mut state = self.load_or_create_state()?;
        state.dvr = settings.clone();
        upsert_dvr_knowledge_root(&mut state.knowledge, &settings);
        state.knowledge =
            validate_knowledge_settings(sanitize_knowledge_settings(state.knowledge))?;
        self.save_projected_state(state)
    }

    pub fn knowledge_settings(&self) -> Result<KnowledgeSettings, String> {
        Ok(self.load_or_create_state()?.knowledge)
    }

    pub fn save_knowledge_settings(
        &self,
        settings: KnowledgeSettings,
    ) -> Result<AdminConsoleState, String> {
        let settings = validate_knowledge_settings(sanitize_knowledge_settings(settings))?;
        let mut state = self.load_or_create_state()?;
        state.knowledge = settings;
        self.save_projected_state(state)
    }

    pub fn list_knowledge_index_jobs(&self) -> Result<Vec<KnowledgeIndexJobRecord>, String> {
        let mut jobs = self.load_or_create_state()?.knowledge_index_jobs;
        jobs.sort_by(|left, right| {
            right
                .requested_at
                .cmp(&left.requested_at)
                .then_with(|| right.job_id.cmp(&left.job_id))
        });
        Ok(jobs)
    }

    pub fn save_knowledge_index_job(
        &self,
        job: KnowledgeIndexJobRecord,
    ) -> Result<KnowledgeIndexJobRecord, String> {
        let job = sanitize_knowledge_index_job(job)
            .ok_or_else(|| "knowledge index job requires a job_id".to_string())?;
        let mut state = self.load_or_create_state()?;
        if let Some(existing) = state
            .knowledge_index_jobs
            .iter_mut()
            .find(|item| item.job_id == job.job_id)
        {
            *existing = job.clone();
        } else {
            state.knowledge_index_jobs.push(job.clone());
        }
        state.knowledge_index_jobs = sanitize_knowledge_index_jobs(state.knowledge_index_jobs);
        self.save_projected_state(state)?;
        Ok(job)
    }

    pub fn cancel_knowledge_index_job(
        &self,
        job_id: &str,
        canceled_at: String,
    ) -> Result<Option<KnowledgeIndexJobRecord>, String> {
        let job_id = job_id.trim();
        if job_id.is_empty() {
            return Err("job_id 不能为空".to_string());
        }

        let mut state = self.load_or_create_state()?;
        let Some(existing) = state
            .knowledge_index_jobs
            .iter_mut()
            .find(|item| item.job_id == job_id)
        else {
            return Ok(None);
        };

        if matches!(existing.status.as_str(), "queued" | "running") {
            existing.status = "canceled".to_string();
            existing.cancel_requested = true;
            existing.completed_at = Some(canceled_at);
            existing.progress_percent = existing.progress_percent.or(Some(0));
        }
        let updated = existing.clone();
        state.knowledge_index_jobs = sanitize_knowledge_index_jobs(state.knowledge_index_jobs);
        self.save_projected_state(state)?;
        Ok(Some(updated))
    }

    pub fn save_device_credential(
        &self,
        credential: DeviceCredentialSecret,
    ) -> Result<AdminConsoleState, String> {
        let credential = sanitize_device_credential(credential)
            .ok_or_else(|| "device credential requires a device_id".to_string())?;
        let mut state = self.load_or_create_state()?;
        if let Some(existing) = state
            .device_credentials
            .iter_mut()
            .find(|item| item.device_id == credential.device_id)
        {
            *existing = credential;
        } else {
            state.device_credentials.push(credential);
        }
        self.save_projected_state(state)
    }

    pub fn mark_device_credential_verified(
        &self,
        device_id: &str,
        verified_at: String,
    ) -> Result<AdminConsoleState, String> {
        let device_id = device_id.trim();
        if device_id.is_empty() {
            return Err("device_id 不能为空".to_string());
        }

        let mut state = self.load_or_create_state()?;
        if let Some(existing) = state
            .device_credentials
            .iter_mut()
            .find(|item| item.device_id == device_id)
        {
            existing.last_verified_at = Some(verified_at);
            return self.save_projected_state(state);
        }

        Ok(state)
    }

    pub fn forget_device(&self, device_id: &str) -> Result<AdminConsoleState, String> {
        let device_id = device_id.trim();
        if device_id.is_empty() {
            return Err("device_id 不能为空".to_string());
        }

        let mut state = self.load_or_create_state()?;
        if state.defaults.selected_camera_device_id.as_deref() == Some(device_id) {
            state.defaults.selected_camera_device_id = None;
        }
        state
            .dvr
            .enabled_device_ids
            .retain(|enabled| enabled != device_id);
        state
            .device_credentials
            .retain(|credential| credential.device_id != device_id);
        state
            .device_evidence
            .retain(|record| record.device_id != device_id);
        let remaining_devices = self.registry_store.load_devices().unwrap_or_default();
        if state.device_credentials.is_empty() && remaining_devices.is_empty() {
            state.defaults.rtsp_username = default_rtsp_username();
            state.defaults.rtsp_password.clear();
        }

        let credential_id = device_rtsp_credential_id(device_id);
        state
            .platform
            .credentials
            .retain(|credential| credential.credential_id != credential_id);
        for policy in &mut state.platform.recording_policies {
            if policy.device_id.as_deref() == Some(device_id) {
                policy.device_id = None;
            }
        }

        self.save_projected_state(state)
    }

    pub fn record_device_evidence(
        &self,
        mut record: DeviceEvidenceRecord,
    ) -> Result<AdminConsoleState, String> {
        if record.evidence_id.trim().is_empty() {
            record.evidence_id = format!("device-evidence-{}", Uuid::new_v4().simple());
        }
        let record = sanitize_device_evidence_record(record)
            .ok_or_else(|| "device evidence requires a device_id".to_string())?;
        let mut state = self.load_or_create_state()?;
        if let Some(existing) = state
            .device_evidence
            .iter_mut()
            .find(|item| item.evidence_id == record.evidence_id)
        {
            *existing = record;
        } else {
            state.device_evidence.push(record);
        }
        self.save_projected_state(state)
    }

    pub fn list_device_evidence(
        &self,
        device_id: &str,
    ) -> Result<Vec<DeviceEvidenceRecord>, String> {
        let device_id = device_id.trim();
        if device_id.is_empty() {
            return Ok(Vec::new());
        }
        let mut evidence = self
            .load_or_create_state()?
            .device_evidence
            .into_iter()
            .filter(|record| record.device_id == device_id)
            .collect::<Vec<_>>();
        evidence.sort_by(|left, right| {
            right
                .observed_at
                .cmp(&left.observed_at)
                .then(right.evidence_id.cmp(&left.evidence_id))
        });
        Ok(evidence)
    }

    pub fn save_remote_view_config(
        &self,
        config: RemoteViewConfig,
    ) -> Result<AdminConsoleState, String> {
        let mut state = self.load_or_create_state()?;
        state.remote_view = sanitize_remote_view_config(config);
        if let Some(workspace) = preferred_workspace_mut(&mut state.platform) {
            set_workspace_remote_view_projection(workspace, &state.remote_view);
        }
        self.save_platform_primary_state(state)
    }

    pub fn load_remote_view_config(&self) -> Result<RemoteViewConfig, String> {
        let state = self.load_or_create_state()?;
        Ok(resolved_remote_view_config(&state))
    }

    pub fn save_bridge_provider_status(
        &self,
        config: BridgeProviderConfig,
    ) -> Result<AdminConsoleState, String> {
        let mut state = self.load_or_create_state()?;
        state.bridge_provider = sanitize_bridge_provider_config(config);
        if state.bridge_provider.connected {
            state.binding.status = "Gateway 已连接".to_string();
            state.binding.metric = "Gateway 在线".to_string();
        } else if state.bridge_provider.configured {
            state.binding.status = "Gateway 已启用".to_string();
            state.binding.metric = "Gateway 未连通".to_string();
        } else {
            state.binding.status = "等待 Gateway".to_string();
            state.binding.metric = "Gateway 未配置".to_string();
        }
        if !state.bridge_provider.app_name.trim().is_empty() {
            state.binding.bound_user = Some(state.bridge_provider.app_name.clone());
        } else if !state.bridge_provider.platform.trim().is_empty() {
            state.binding.bound_user = Some(format!("{} gateway", state.bridge_provider.platform));
        } else {
            state.binding.bound_user = None;
        }

        state
            .platform
            .provider_accounts
            .retain(|provider| provider.provider_account_id != BRIDGE_PROVIDER_ACCOUNT_ID);
        if let Some(provider) = build_bridge_provider_account(
            &state.bridge_provider,
            &state.defaults.notification_channel,
            state.platform.identity_bindings.len(),
        ) {
            upsert_provider_account(&mut state.platform.provider_accounts, provider);
        }

        if let Some(workspace) = preferred_workspace_mut(&mut state.platform) {
            set_workspace_binding_projection(workspace, &state.binding);
        }

        self.save_platform_primary_state(state)
    }

    pub fn save_model_endpoint(
        &self,
        endpoint: ModelEndpoint,
    ) -> Result<AdminConsoleState, String> {
        let mut state = self.load_or_create_state()?;
        let mut endpoint = sanitize_model_endpoint(endpoint)?;
        if let Some(existing) = state
            .models
            .endpoints
            .iter_mut()
            .find(|existing| existing.model_endpoint_id == endpoint.model_endpoint_id)
        {
            preserve_model_endpoint_secret_metadata(existing, &mut endpoint);
            *existing = endpoint;
        } else {
            state.models.endpoints.push(endpoint);
        }
        self.save_projected_state(state)
    }

    pub fn patch_model_endpoint(
        &self,
        endpoint_id: &str,
        patch: Value,
    ) -> Result<AdminConsoleState, String> {
        let endpoint_id = endpoint_id.trim();
        if endpoint_id.is_empty() {
            return Err("model endpoint id 不能为空".to_string());
        }
        let Some(patch_object) = patch.as_object() else {
            return Err("模型端点 patch 必须是 JSON object".to_string());
        };

        let mut state = self.load_or_create_state()?;
        let endpoint = state
            .models
            .endpoints
            .iter_mut()
            .find(|existing| existing.model_endpoint_id == endpoint_id)
            .ok_or_else(|| format!("未找到模型端点 {endpoint_id}"))?;
        let existing_snapshot = endpoint.clone();

        if let Some(value) = patch_object.get("workspace_id") {
            endpoint.workspace_id = optional_trimmed_string(value);
        }
        if let Some(value) = patch_object.get("provider_account_id") {
            endpoint.provider_account_id = optional_trimmed_string(value);
        }
        if let Some(value) = patch_object.get("model_kind") {
            endpoint.model_kind = serde_json::from_value(value.clone())
                .map_err(|error| format!("invalid model_kind: {error}"))?;
        }
        if let Some(value) = patch_object.get("endpoint_kind") {
            endpoint.endpoint_kind = serde_json::from_value(value.clone())
                .map_err(|error| format!("invalid endpoint_kind: {error}"))?;
        }
        if let Some(value) = patch_object.get("provider_key") {
            endpoint.provider_key = string_value_or_empty(value);
        }
        if let Some(value) = patch_object.get("model_name") {
            endpoint.model_name = string_value_or_empty(value);
        }
        if let Some(value) = patch_object.get("capability_tags") {
            endpoint.capability_tags = string_vec(Some(value)).unwrap_or_default();
        }
        if let Some(value) = patch_object.get("cost_policy") {
            endpoint.cost_policy = value.clone();
        }
        if let Some(value) = patch_object.get("status") {
            endpoint.status = serde_json::from_value(value.clone())
                .map_err(|error| format!("invalid status: {error}"))?;
        }
        if let Some(value) = patch_object.get("metadata") {
            endpoint.metadata = merge_json_object(endpoint.metadata.clone(), value.clone())?;
        }

        preserve_model_endpoint_secret_metadata(&existing_snapshot, endpoint);
        let sanitized = sanitize_model_endpoint(endpoint.clone())?;
        *endpoint = sanitized;
        self.save_projected_state(state)
    }

    pub fn record_model_endpoint_test_result(
        &self,
        endpoint_id: &str,
        ok: bool,
        observed_status: &str,
        summary: &str,
        details: Value,
    ) -> Result<AdminConsoleState, String> {
        let endpoint_id = endpoint_id.trim();
        if endpoint_id.is_empty() {
            return Err("model endpoint id 不能为空".to_string());
        }

        let mut state = self.load_or_create_state()?;
        let endpoint = state
            .models
            .endpoints
            .iter_mut()
            .find(|existing| existing.model_endpoint_id == endpoint_id)
            .ok_or_else(|| format!("未找到模型端点 {endpoint_id}"))?;

        let observed_status = observed_status.trim().to_lowercase();
        let next_status = match observed_status.as_str() {
            "active" if endpoint.status != ModelEndpointStatus::Disabled => {
                ModelEndpointStatus::Active
            }
            "degraded" | "disabled" if endpoint.status != ModelEndpointStatus::Disabled => {
                ModelEndpointStatus::Degraded
            }
            _ => endpoint.status,
        };

        endpoint.status = next_status;
        let mut metadata = match endpoint.metadata.clone() {
            Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        metadata.insert(
            "last_test".to_string(),
            json!({
                "ok": ok,
                "status": observed_status,
                "summary": summary.trim(),
                "details": details,
                "tested_at": model_test_timestamp(),
            }),
        );
        metadata.insert("health_status".to_string(), Value::String(observed_status));
        endpoint.metadata = Value::Object(metadata);

        let sanitized = sanitize_model_endpoint(endpoint.clone())?;
        *endpoint = sanitized;
        self.save_projected_state(state)
    }

    pub fn create_model_download_job(
        &self,
        model_id: &str,
        display_name: &str,
        provider_key: &str,
        target_path: Option<String>,
        metadata: Value,
    ) -> Result<ModelDownloadJobRecord, String> {
        Ok(self
            .create_or_update_model_download_job(
                model_id,
                display_name,
                provider_key,
                target_path,
                metadata,
            )?
            .job)
    }

    pub fn create_or_update_model_download_job(
        &self,
        model_id: &str,
        display_name: &str,
        provider_key: &str,
        target_path: Option<String>,
        metadata: Value,
    ) -> Result<ModelDownloadJobCreateResult, String> {
        let _guard = model_download_write_guard()?;
        let model_id = model_id.trim();
        if model_id.is_empty() {
            return Err("model_id 不能为空".to_string());
        }
        let now = model_test_timestamp();
        let mut state = self.load_or_create_state()?;
        if let Some(index) = latest_model_download_job_index(&state.model_download_jobs, model_id) {
            let existing = &mut state.model_download_jobs[index];
            if model_download_job_record_is_active(existing) {
                return Ok(ModelDownloadJobCreateResult {
                    job: existing.clone(),
                    should_spawn_worker: false,
                });
            }
            existing.display_name = display_name.trim().to_string();
            existing.provider_key = provider_key.trim().to_string();
            existing.status = "queued".to_string();
            existing.requested_at = now.clone();
            existing.updated_at = now;
            existing.target_path = target_path;
            existing.progress_percent = Some(0);
            existing.bytes_downloaded = Some(0);
            existing.total_bytes = None;
            existing.started_at = None;
            existing.completed_at = None;
            existing.error_message = None;
            existing.message = "download job queued by explicit admin action".to_string();
            existing.metadata = metadata;
            let job = existing.clone();
            self.save_projected_state(state)?;
            return Ok(ModelDownloadJobCreateResult {
                job,
                should_spawn_worker: true,
            });
        }
        let job = ModelDownloadJobRecord {
            job_id: format!("model-download-{}", Uuid::new_v4().simple()),
            model_id: model_id.to_string(),
            display_name: display_name.trim().to_string(),
            provider_key: provider_key.trim().to_string(),
            status: "queued".to_string(),
            requested_at: now.clone(),
            updated_at: now,
            target_path,
            progress_percent: Some(0),
            bytes_downloaded: Some(0),
            total_bytes: None,
            started_at: None,
            completed_at: None,
            error_message: None,
            message: "download job queued by explicit admin action".to_string(),
            metadata,
        };

        state.model_download_jobs.push(job.clone());
        self.save_projected_state(state)?;
        Ok(ModelDownloadJobCreateResult {
            job,
            should_spawn_worker: true,
        })
    }

    pub fn save_model_store_root(&self, root: &str) -> Result<AdminConsoleState, String> {
        let root = non_empty_opt(root).ok_or_else(|| "模型保存位置不能为空".to_string())?;
        let path = Path::new(&root);
        if !path.is_absolute() {
            return Err("模型保存位置必须是 HarborOS 上的绝对路径".to_string());
        }
        let mut state = self.load_or_create_state()?;
        state.models.model_store_root = root;
        self.save_projected_state(state)
    }

    pub fn save_model_runtime(
        &self,
        runtime: ModelRuntimeRecord,
    ) -> Result<AdminConsoleState, String> {
        let mut state = self.load_or_create_state()?;
        let runtime = sanitize_model_runtime(runtime, &state.models.model_store_root)?;
        if let Some(existing) = state
            .models
            .runtimes
            .iter_mut()
            .find(|existing| existing.runtime_id == runtime.runtime_id)
        {
            *existing = runtime;
        } else {
            state.models.runtimes.push(runtime);
        }
        self.save_projected_state(state)
    }

    pub fn install_model_runtime(&self, runtime_id: &str) -> Result<ModelRuntimeRecord, String> {
        let runtime_id =
            non_empty_opt(runtime_id).ok_or_else(|| "runtime_id 不能为空".to_string())?;
        let mut state = self.load_or_create_state()?;
        let Some(existing) = state
            .models
            .runtimes
            .iter()
            .find(|runtime| runtime.runtime_id == runtime_id)
            .cloned()
        else {
            return Err(format!("未找到 runtime {runtime_id}"));
        };
        if !existing.managed {
            return Err(format!(
                "runtime {} 不是 Harbor-managed runtime",
                existing.display_name
            ));
        }
        if !existing.installable {
            return Err(format!(
                "runtime {} 当前包未提供一键安装",
                existing.display_name
            ));
        }

        let now = model_test_timestamp();
        let mut runtime = existing;
        runtime.status = "installed".to_string();
        runtime.enabled = true;
        runtime.installed_at = runtime.installed_at.or_else(|| Some(now.clone()));
        runtime.updated_at = Some(now);
        runtime.message = format!("{} 已由 Harbor Assistant 启用。", runtime.display_name);
        if let Some(existing) = state
            .models
            .runtimes
            .iter_mut()
            .find(|existing| existing.runtime_id == runtime.runtime_id)
        {
            *existing = sanitize_model_runtime(runtime.clone(), &state.models.model_store_root)?;
        }
        self.save_projected_state(state)?;
        Ok(runtime)
    }

    pub fn save_model_capability_binding(
        &self,
        capability_id: &str,
        model_id: &str,
    ) -> Result<ModelCapabilityBindingRecord, String> {
        let capability_id =
            non_empty_opt(capability_id).ok_or_else(|| "capability_id 不能为空".to_string())?;
        let model_id = non_empty_opt(model_id).ok_or_else(|| "model_id 不能为空".to_string())?;
        let mut state = self.load_or_create_state()?;
        let record = ModelCapabilityBindingRecord {
            capability_id,
            model_id,
            updated_at: model_test_timestamp(),
        };
        if let Some(existing) = state
            .models
            .capability_bindings
            .iter_mut()
            .find(|existing| existing.capability_id == record.capability_id)
        {
            *existing = record.clone();
        } else {
            state.models.capability_bindings.push(record.clone());
        }
        self.save_projected_state(state)?;
        Ok(record)
    }

    pub fn model_download_job(
        &self,
        job_id: &str,
    ) -> Result<Option<ModelDownloadJobRecord>, String> {
        let job_id = job_id.trim();
        if job_id.is_empty() {
            return Ok(None);
        }
        Ok(self
            .load_state()?
            .model_download_jobs
            .into_iter()
            .find(|job| job.job_id == job_id))
    }

    pub fn cancel_model_download_job(
        &self,
        job_id: &str,
    ) -> Result<Option<ModelDownloadJobRecord>, String> {
        let job_id = job_id.trim();
        if job_id.is_empty() {
            return Ok(None);
        }
        let _guard = model_download_write_guard()?;
        let mut state = self.load_or_create_state()?;
        let Some(job) = state
            .model_download_jobs
            .iter_mut()
            .find(|job| job.job_id == job_id)
        else {
            return Ok(None);
        };
        if !matches!(
            job.status.as_str(),
            "completed" | "failed" | "cancelled" | "canceled"
        ) {
            job.status = "canceled".to_string();
            job.updated_at = model_test_timestamp();
            job.completed_at = Some(job.updated_at.clone());
            job.message = "download job canceled by explicit admin action".to_string();
        }
        let cancelled = job.clone();
        self.save_projected_state(state)?;
        Ok(Some(cancelled))
    }

    pub fn list_model_download_jobs(&self) -> Result<Vec<ModelDownloadJobRecord>, String> {
        Ok(self.load_state()?.model_download_jobs)
    }

    pub fn save_model_download_job(
        &self,
        job: ModelDownloadJobRecord,
    ) -> Result<ModelDownloadJobRecord, String> {
        let job_id = job.job_id.trim();
        if job_id.is_empty() {
            return Err("job_id 不能为空".to_string());
        }
        let _guard = model_download_write_guard()?;
        let mut state = self.load_or_create_state()?;
        let Some(existing) = state
            .model_download_jobs
            .iter_mut()
            .find(|existing| existing.job_id == job_id)
        else {
            return Err(format!("未找到模型下载任务 {job_id}"));
        };
        *existing = job.clone();
        self.save_projected_state(state)?;
        Ok(job)
    }

    pub fn save_model_route_policies(
        &self,
        policies: Vec<ModelRoutePolicy>,
    ) -> Result<AdminConsoleState, String> {
        let mut state = self.load_or_create_state()?;
        let sanitized = if policies.is_empty() {
            default_model_route_policies()
        } else {
            let mut sanitized = Vec::new();
            for policy in policies {
                sanitized.push(sanitize_model_route_policy(policy)?);
            }
            sanitized
        };
        state.models.route_policies = sanitized;
        self.save_projected_state(state)
    }

    pub fn append_audit_record(&self, record: AuditRecord) -> Result<AuditRecord, String> {
        let mut state = self.load_or_create_state()?;
        append_bounded_audit_record(&mut state.audit_records, record);
        let record = state
            .audit_records
            .last()
            .cloned()
            .ok_or_else(|| "audit record stream append failed".to_string())?;
        self.save_projected_state(state)?;
        Ok(record)
    }

    pub fn audit_records(&self) -> Result<Vec<AuditRecord>, String> {
        Ok(self.load_or_create_state()?.audit_records)
    }

    pub fn registry_store(&self) -> &DeviceRegistryStore {
        &self.registry_store
    }

    fn bootstrap_state(&self) -> Result<AdminConsoleState, String> {
        let mut state = AdminConsoleState::default();
        self.apply_registry_hints(&mut state)?;
        Ok(state)
    }

    fn apply_registry_hints(&self, state: &mut AdminConsoleState) -> Result<(), String> {
        let devices = self.registry_store.load_devices()?;
        if let Some(hints) = derive_rtsp_hints(&devices) {
            if state.defaults.rtsp_username.trim().is_empty() {
                state.defaults.rtsp_username = hints.username.clone();
            }
            if state.defaults.rtsp_password.trim().is_empty() {
                state.defaults.rtsp_password = hints.password;
            }
            if state.defaults.rtsp_paths.is_empty() {
                state.defaults.rtsp_paths = hints.paths;
            }
        }

        normalize_loaded_admin_state(state);
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtspHints {
    pub username: String,
    pub password: String,
    pub paths: Vec<String>,
}

pub fn sanitize_defaults(mut defaults: AdminDefaults) -> AdminDefaults {
    if defaults.cidr.trim().is_empty() || defaults.cidr.trim().eq_ignore_ascii_case("auto") {
        defaults.cidr = default_scan_cidr();
    } else if let Some(normalized) = normalize_scan_cidr(defaults.cidr.trim()) {
        defaults.cidr = normalized;
    }
    if defaults.discovery.trim().is_empty() {
        defaults.discovery = "RTSP Probe".to_string();
    }
    if defaults.recording.trim().is_empty() {
        defaults.recording = "按事件录制".to_string();
    }
    if defaults.capture.trim().is_empty() {
        defaults.capture = "图片 + 摘要".to_string();
    }
    if defaults.ai.trim().is_empty() {
        defaults.ai = "人体检测 + 中文摘要".to_string();
    }
    if defaults.notification_channel.trim().is_empty() {
        defaults.notification_channel = "家庭通知频道".to_string();
    }
    if defaults.rtsp_username.trim().is_empty() {
        defaults.rtsp_username = default_rtsp_username();
    }
    if defaults.rtsp_port == 0 {
        defaults.rtsp_port = default_rtsp_port();
    }
    defaults.rtsp_paths = dedupe_rtsp_paths(defaults.rtsp_paths);
    if defaults.rtsp_paths.is_empty() {
        defaults.rtsp_paths = default_rtsp_paths();
    }
    defaults.selected_camera_device_id = defaults
        .selected_camera_device_id
        .and_then(|value| non_empty_opt(&value));
    defaults.capture_subdirectory = sanitize_capture_subdirectory(&defaults.capture_subdirectory)
        .unwrap_or_else(default_capture_subdirectory);
    defaults.clip_length_seconds = defaults.clip_length_seconds.clamp(3, 300);
    defaults.keyframe_count = defaults.keyframe_count.clamp(1, 12);
    defaults.keyframe_interval_seconds = defaults.keyframe_interval_seconds.clamp(1, 60);
    defaults
}

fn normalize_scan_cidr(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let (ip, prefix) = trimmed.split_once('/')?;
    let parsed_ip = ip.parse::<Ipv4Addr>().ok()?;
    if let Ok(parsed_prefix) = prefix.parse::<u8>() {
        if parsed_prefix <= 32 {
            let mask = if parsed_prefix == 0 {
                0
            } else {
                u32::MAX << (32 - parsed_prefix)
            };
            let network = Ipv4Addr::from(u32::from(parsed_ip) & mask);
            return Some(format!("{network}/{parsed_prefix}"));
        }
    }
    if prefix.parse::<u16>().ok().is_some_and(|port| port > 32) {
        let octets = parsed_ip.octets();
        return Some(format!("{}.{}.{}.0/24", octets[0], octets[1], octets[2]));
    }
    None
}

pub fn sanitize_knowledge_settings(settings: KnowledgeSettings) -> KnowledgeSettings {
    let mut normalized_roots = Vec::new();
    let mut seen_ids = HashSet::new();
    let mut seen_paths = HashSet::new();
    for (index, mut root) in settings.source_roots.into_iter().enumerate() {
        root.path = root.path.trim().to_string();
        if root.path.is_empty() {
            continue;
        }
        root.root_id = sanitize_knowledge_root_id(&root.root_id, &root.label, &root.path, index);
        root.label = sanitize_knowledge_root_label(&root.label, &root.path, &root.root_id);
        root.include = sanitize_string_list(root.include);
        root.exclude = sanitize_string_list(root.exclude);
        root.last_indexed_at = root.last_indexed_at.and_then(|value| non_empty_opt(&value));
        let path_key = normalized_path_key(&root.path);
        if seen_ids.insert(root.root_id.clone()) && seen_paths.insert(path_key) {
            normalized_roots.push(root);
        }
    }
    KnowledgeSettings {
        source_roots: normalized_roots,
        index_root: non_empty_opt(&settings.index_root)
            .unwrap_or_else(default_knowledge_index_root),
        privacy_level: settings.privacy_level,
        default_resource_profile: settings.default_resource_profile,
        retrieval: sanitize_knowledge_retrieval_settings(settings.retrieval),
    }
}

fn sanitize_knowledge_retrieval_settings(
    mut settings: KnowledgeRetrievalSettings,
) -> KnowledgeRetrievalSettings {
    if !settings.fusion_strategy.eq_ignore_ascii_case("rrf") {
        settings.fusion_strategy = default_retrieval_fusion_strategy();
    } else {
        settings.fusion_strategy = "rrf".to_string();
    }
    settings.rrf_k = sanitize_unit_or_positive_f32(settings.rrf_k, 1.0, 1000.0, 60.0);
    settings.lexical_weight =
        sanitize_unit_or_positive_f32(settings.lexical_weight, 0.0, 1.0, 0.35);
    settings.vector_weight = sanitize_unit_or_positive_f32(settings.vector_weight, 0.0, 1.0, 0.65);
    if settings.lexical_weight <= f32::EPSILON && settings.vector_weight <= f32::EPSILON {
        settings.lexical_weight = 0.35;
        settings.vector_weight = 0.65;
    }
    settings.candidate_limit = settings.candidate_limit.clamp(1, 500);
    settings.vector_min_score =
        sanitize_unit_or_positive_f32(settings.vector_min_score, 0.0, 1.0, 0.25);
    settings.semantic_only_min_score =
        sanitize_unit_or_positive_f32(settings.semantic_only_min_score, 0.0, 1.0, 0.55);
    settings.rerank_top_k = settings
        .rerank_top_k
        .clamp(1, settings.candidate_limit.max(1));
    settings.rerank_min_score =
        sanitize_unit_or_positive_f32(settings.rerank_min_score, 0.0, 1.0, 0.15);
    settings.mmr_lambda = sanitize_unit_or_positive_f32(settings.mmr_lambda, 0.0, 1.0, 0.70);
    settings
}

fn sanitize_unit_or_positive_f32(value: f32, min: f32, max: f32, fallback: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        fallback
    }
}

fn upsert_dvr_knowledge_root(settings: &mut KnowledgeSettings, dvr: &DvrRecordingSettings) {
    let root_id = dvr_knowledge_root_id().to_string();
    if dvr.enabled_device_ids.is_empty() {
        settings
            .source_roots
            .retain(|existing| existing.root_id != root_id);
        return;
    }

    let path = dvr.media_library_root.trim();
    if path.is_empty() {
        return;
    }
    let root = KnowledgeSourceRoot {
        root_id: root_id.clone(),
        label: "Camera DVR Library".to_string(),
        path: path.to_string(),
        enabled: true,
        include: vec![
            "**/*.mp4".to_string(),
            "**/*.jpg".to_string(),
            "**/*.jpeg".to_string(),
            "**/*.png".to_string(),
            "**/*.webp".to_string(),
            "**/*.json".to_string(),
        ],
        exclude: Vec::new(),
        last_indexed_at: None,
    };
    if let Some(existing) = settings
        .source_roots
        .iter_mut()
        .find(|existing| existing.root_id == root_id)
    {
        existing.label = root.label;
        existing.path = root.path;
        existing.enabled = true;
        existing.include = root.include;
        existing.exclude = root.exclude;
    } else {
        settings.source_roots.push(root);
    }
}

fn sanitize_knowledge_index_jobs(
    jobs: Vec<KnowledgeIndexJobRecord>,
) -> Vec<KnowledgeIndexJobRecord> {
    let mut sanitized = jobs
        .into_iter()
        .filter_map(sanitize_knowledge_index_job)
        .collect::<Vec<_>>();
    sanitized.sort_by(|left, right| {
        right
            .requested_at
            .cmp(&left.requested_at)
            .then_with(|| right.job_id.cmp(&left.job_id))
    });
    sanitized.truncate(50);
    sanitized
}

fn sanitize_knowledge_index_job(
    mut job: KnowledgeIndexJobRecord,
) -> Option<KnowledgeIndexJobRecord> {
    job.job_id = job.job_id.trim().to_string();
    if job.job_id.is_empty() {
        return None;
    }
    job.source_root_id = job.source_root_id.trim().to_string();
    job.source_root_label = job.source_root_label.trim().to_string();
    job.source_root_path = job.source_root_path.trim().to_string();
    job.status = non_empty_opt(&job.status).unwrap_or_else(|| "queued".to_string());
    job.modalities = sanitize_string_list(job.modalities);
    job.progress_percent = job.progress_percent.map(|value| value.min(100));
    Some(job)
}

pub fn validate_knowledge_settings(
    settings: KnowledgeSettings,
) -> Result<KnowledgeSettings, String> {
    let settings = sanitize_knowledge_settings(settings);
    if settings.index_root.trim().is_empty() {
        return Err("knowledge.index_root 不能为空".to_string());
    }
    for root in &settings.source_roots {
        if path_is_same_or_inside(&settings.index_root, &root.path) {
            return Err(format!(
                "knowledge.index_root 不能位于 source_root 内：index_root={} source_root={}",
                settings.index_root, root.path
            ));
        }
    }
    Ok(settings)
}

fn sanitize_knowledge_root_id(root_id: &str, label: &str, path: &str, index: usize) -> String {
    let existing = slugify_identity_component(root_id);
    if !existing.is_empty() && existing != "item" {
        return existing;
    }
    let from_label = slugify_identity_component(label);
    if !from_label.is_empty() && from_label != "item" {
        return format!("knowledge-{from_label}");
    }
    let from_path = Path::new(path)
        .file_name()
        .and_then(|value| value.to_str())
        .map(slugify_identity_component)
        .filter(|value| !value.is_empty() && value != "item")
        .unwrap_or_else(|| format!("root-{}", index + 1));
    format!("knowledge-{from_path}")
}

fn sanitize_knowledge_root_label(label: &str, path: &str, root_id: &str) -> String {
    let label = label.trim();
    if !label.is_empty() {
        return label.to_string();
    }
    Path::new(path)
        .file_name()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .unwrap_or_else(|| root_id.to_string())
}

fn sanitize_string_list(values: Vec<String>) -> Vec<String> {
    let mut output = Vec::new();
    let mut seen = HashSet::new();
    for value in values {
        let value = value.trim().to_string();
        if !value.is_empty() && seen.insert(value.clone()) {
            output.push(value);
        }
    }
    output
}

pub fn path_is_same_or_inside(child: &str, parent: &str) -> bool {
    let child = normalized_path_key(child);
    let parent = normalized_path_key(parent);
    if child.is_empty() || parent.is_empty() {
        return false;
    }
    child == parent || child.starts_with(&(parent.trim_end_matches('/').to_string() + "/"))
}

fn normalized_path_key(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let candidate = PathBuf::from(trimmed);
    let resolved = fs::canonicalize(&candidate).unwrap_or(candidate);
    let mut key = resolved.to_string_lossy().replace('\\', "/");
    while key.len() > 1 && key.ends_with('/') {
        key.pop();
    }
    if cfg!(windows) {
        key.to_ascii_lowercase()
    } else {
        key
    }
}

fn sanitize_device_credentials(
    credentials: Vec<DeviceCredentialSecret>,
) -> Vec<DeviceCredentialSecret> {
    let mut normalized = Vec::new();
    let mut seen = HashSet::new();
    for credential in credentials {
        let Some(credential) = sanitize_device_credential(credential) else {
            continue;
        };
        if seen.insert(credential.device_id.clone()) {
            normalized.push(credential);
        }
    }
    normalized
}

fn sanitize_device_credential(
    mut credential: DeviceCredentialSecret,
) -> Option<DeviceCredentialSecret> {
    credential.device_id = credential.device_id.trim().to_string();
    if credential.device_id.is_empty() {
        return None;
    }
    credential.username = credential.username.trim().to_string();
    credential.password = credential.password.trim().to_string();
    credential.rtsp_port = credential.rtsp_port.filter(|port| *port > 0);
    credential.rtsp_paths = dedupe_rtsp_paths(credential.rtsp_paths);
    credential.updated_at = credential
        .updated_at
        .and_then(|value| non_empty_opt(&value));
    credential.last_verified_at = credential
        .last_verified_at
        .and_then(|value| non_empty_opt(&value));
    Some(credential)
}

fn sanitize_device_evidence_records(
    records: Vec<DeviceEvidenceRecord>,
) -> Vec<DeviceEvidenceRecord> {
    records
        .into_iter()
        .filter_map(sanitize_device_evidence_record)
        .collect()
}

fn sanitize_device_evidence_record(
    mut record: DeviceEvidenceRecord,
) -> Option<DeviceEvidenceRecord> {
    record.evidence_id = record.evidence_id.trim().to_string();
    record.device_id = record.device_id.trim().to_string();
    record.evidence_kind = record.evidence_kind.trim().to_string();
    record.status = record.status.trim().to_string();
    record.observed_at = record.observed_at.trim().to_string();
    record.summary = redact_device_evidence_text(record.summary.trim());
    redact_device_evidence_value(&mut record.details);

    if record.evidence_id.is_empty()
        || record.device_id.is_empty()
        || record.evidence_kind.is_empty()
        || record.status.is_empty()
        || record.observed_at.is_empty()
    {
        return None;
    }
    Some(record)
}

fn redact_device_evidence_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                if is_secret_like_key(key) {
                    *value = Value::String("redacted".to_string());
                } else {
                    redact_device_evidence_value(value);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_device_evidence_value(item);
            }
        }
        Value::String(text) => {
            *text = redact_device_evidence_text(text);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn is_secret_like_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase();
    normalized.contains("password")
        || normalized.contains("secret")
        || normalized.contains("token")
        || normalized.contains("api_key")
        || normalized.contains("apikey")
        || normalized.contains("credential")
}

fn redact_device_evidence_text(value: &str) -> String {
    let mut redacted = redact_device_evidence_url_userinfo(value);
    for key in ["password", "token", "api_key", "apikey", "secret"] {
        redacted = redact_query_like_secret(&redacted, key);
    }
    redacted
}

fn redact_query_like_secret(value: &str, key: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    let needle = format!("{key}=");
    let lower = value.to_ascii_lowercase();
    while let Some(relative_start) = lower[cursor..].find(&needle) {
        let start = cursor + relative_start;
        let value_start = start + needle.len();
        let value_end = value[value_start..]
            .find(|ch: char| matches!(ch, '&' | ' ' | '\n' | '\r' | '\t' | '"' | '\'' | '<' | '>'))
            .map(|relative| value_start + relative)
            .unwrap_or(value.len());
        output.push_str(&value[cursor..value_start]);
        output.push_str("redacted");
        cursor = value_end;
    }
    output.push_str(&value[cursor..]);
    output
}

fn redact_device_evidence_url_userinfo(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    while let Some(relative_scheme_end) = value[cursor..].find("://") {
        let scheme_end = cursor + relative_scheme_end;
        let scheme_start = value[..scheme_end]
            .rfind(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '+' || ch == '-' || ch == '.'))
            .map(|index| index + 1)
            .unwrap_or(0);
        let scheme = &value[scheme_start..scheme_end];
        if !matches!(scheme, "rtsp" | "rtsps" | "http" | "https") {
            output.push_str(&value[cursor..scheme_end + 3]);
            cursor = scheme_end + 3;
            continue;
        }

        let authority_start = scheme_end + 3;
        let authority_end = value[authority_start..]
            .find(|ch: char| {
                matches!(
                    ch,
                    '/' | '?' | '#' | '"' | '\'' | '<' | '>' | ' ' | '\n' | '\r' | '\t'
                )
            })
            .map(|relative| authority_start + relative)
            .unwrap_or(value.len());
        let authority = &value[authority_start..authority_end];
        if let Some(userinfo_end) = authority.rfind('@') {
            output.push_str(&value[cursor..authority_start]);
            output.push_str("redacted:redacted@");
            output.push_str(&authority[userinfo_end + 1..]);
        } else {
            output.push_str(&value[cursor..authority_end]);
        }
        cursor = authority_end;
    }
    output.push_str(&value[cursor..]);
    output
}

fn sanitize_capture_subdirectory(value: &str) -> Option<String> {
    let sanitized = value
        .split(['/', '\\'])
        .map(str::trim)
        .filter(|segment| !segment.is_empty() && *segment != "." && *segment != "..")
        .collect::<Vec<_>>();
    if sanitized.is_empty() {
        return None;
    }
    Some(sanitized.join("/"))
}

pub fn sanitize_binding(mut binding: AdminBindingState) -> AdminBindingState {
    if binding.status.trim().is_empty() {
        binding.status = "等待扫码".to_string();
    }
    if binding.metric.trim().is_empty() {
        binding.metric = "等待绑定".to_string();
    }
    if binding.channel.trim().is_empty() {
        binding.channel = DEFAULT_BINDING_CHANNEL_LABEL.to_string();
    }
    if let Some(token_code) = normalize_binding_code(&binding.qr_token) {
        if binding.session_code.trim().is_empty() || binding.session_code != token_code {
            binding.session_code = token_code;
        }
    } else if binding.session_code.trim().is_empty() {
        binding.session_code = generate_binding_code();
    }
    if binding.qr_token.trim().is_empty()
        || normalize_binding_code(&binding.qr_token).as_deref()
            != Some(binding.session_code.as_str())
    {
        binding.qr_token = generate_qr_token(&binding.session_code);
    }
    binding
}

fn sanitize_notification_targets(
    targets: Vec<NotificationTargetRecord>,
) -> Vec<NotificationTargetRecord> {
    let mut normalized = Vec::new();
    let mut seen_ids = HashSet::new();
    let mut seen_routes = HashSet::new();
    for target in targets {
        let Ok(target) = sanitize_notification_target(target) else {
            continue;
        };
        if !seen_ids.insert(target.target_id.clone()) {
            continue;
        }
        if !seen_routes.insert(target.route_key.clone()) {
            continue;
        }
        normalized.push(target);
    }
    ensure_single_default_notification_target(&mut normalized);
    normalized
}

fn sanitize_notification_target(
    mut target: NotificationTargetRecord,
) -> Result<NotificationTargetRecord, String> {
    target.target_id = target.target_id.trim().to_string();
    if target.target_id.is_empty() {
        target.target_id = new_notification_target_id();
    }
    target.label = sanitize_notification_target_label(&target.label)?;
    target.route_key = sanitize_notification_target_route_key(&target.route_key)?;
    target.platform_hint = normalize_platform_hint(&target.platform_hint);
    Ok(target)
}

fn sanitize_notification_target_label(label: &str) -> Result<String, String> {
    let label = label.trim();
    if label.is_empty() {
        return Err("label 不能为空".to_string());
    }
    Ok(label.to_string())
}

fn sanitize_notification_target_route_key(route_key: &str) -> Result<String, String> {
    let route_key = route_key.trim();
    if route_key.is_empty() {
        return Err("route_key 不能为空".to_string());
    }
    Ok(route_key.to_string())
}

fn normalize_platform_hint(platform_hint: &str) -> String {
    platform_hint.trim().to_ascii_lowercase()
}

fn ensure_single_default_notification_target(targets: &mut [NotificationTargetRecord]) {
    let mut default_seen = false;
    for target in targets.iter_mut() {
        if target.is_default && !default_seen {
            default_seen = true;
            continue;
        }
        if target.is_default {
            target.is_default = false;
        }
    }
    if !default_seen {
        if let Some(first) = targets.first_mut() {
            first.is_default = true;
        }
    }
}

fn new_notification_target_id() -> String {
    format!("target-{}", Uuid::new_v4().simple())
}

fn sanitize_automation_reviews(reviews: Vec<AutomationRuleReview>) -> Vec<AutomationRuleReview> {
    let mut normalized = Vec::new();
    let mut seen_ids = HashSet::new();
    for review in reviews {
        let Ok(review) = sanitize_automation_review(review) else {
            continue;
        };
        if !seen_ids.insert(review.review_id.clone()) {
            continue;
        }
        normalized.push(review);
    }
    normalized.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    normalized.truncate(100);
    normalized
}

fn sanitize_automation_review(
    mut review: AutomationRuleReview,
) -> Result<AutomationRuleReview, String> {
    review.review_id = review.review_id.trim().to_string();
    if review.review_id.is_empty() {
        review.review_id = new_automation_review_id();
    }
    review.workspace_id =
        non_empty_opt(&review.workspace_id).unwrap_or_else(|| DEFAULT_WORKSPACE_ID.to_string());
    review.source = non_empty_opt(&review.source).unwrap_or_else(|| "harbor_assistant".to_string());
    review.source_channel = review
        .source_channel
        .and_then(|value| non_empty_opt(value.trim()));
    review.source_conversation_id = review
        .source_conversation_id
        .and_then(|value| non_empty_opt(value.trim()));
    review.original_prompt = review.original_prompt.trim().to_string();
    if review.original_prompt.is_empty() {
        return Err("original_prompt 不能为空".to_string());
    }
    review.status = sanitize_automation_review_status(&review.status)?;
    review.risk_level = review
        .risk_level
        .and_then(|value| non_empty_opt(value.trim()));
    review.created_at = review
        .created_at
        .and_then(|value| non_empty_opt(value.trim()));
    review.updated_at = review
        .updated_at
        .and_then(|value| non_empty_opt(value.trim()));
    review.expires_at = review
        .expires_at
        .and_then(|value| non_empty_opt(value.trim()));
    review.rule_id = review.rule_id.and_then(|value| non_empty_opt(value.trim()));
    if automation_review_has_sensitive_payload(&review) {
        return Err("automation review payload 包含敏感字段，已拒绝保存".to_string());
    }
    Ok(review)
}

fn sanitize_automation_review_status(status: &str) -> Result<String, String> {
    let normalized = status.trim().to_ascii_lowercase();
    let normalized = if normalized.is_empty() {
        "draft".to_string()
    } else {
        normalized
    };
    match normalized.as_str() {
        "draft" | "pending" | "active" | "paused" | "discarded" | "expired" => Ok(normalized),
        _ => Err(format!("不支持的 automation review status {status}")),
    }
}

fn automation_review_has_sensitive_payload(review: &AutomationRuleReview) -> bool {
    let Ok(payload) = serde_json::to_string(review) else {
        return true;
    };
    let normalized = payload.to_ascii_lowercase();
    [
        "rtsp://",
        "rtsps://",
        "ha_token",
        "home_assistant_token",
        "access_token",
        "api_key",
        "apikey",
        "private_key",
        "begin private key",
        "camera credential",
        "camera_credential",
        "upload_url",
        "presigned",
        "/var/lib/harboros-beacon",
        "/tmp/harbornavi",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
}

fn new_automation_review_id() -> String {
    format!("review-{}", Uuid::new_v4().simple())
}

pub fn gateway_manage_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.ends_with("/admin/im") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/admin/im")
    }
}

pub fn sanitize_bridge_provider_config(mut config: BridgeProviderConfig) -> BridgeProviderConfig {
    config.app_id.clear();
    config.app_secret.clear();
    config.bot_open_id.clear();
    config.platform = config.platform.trim().to_string();
    config.gateway_base_url = config.gateway_base_url.trim().to_string();
    config.app_name = config.app_name.trim().to_string();
    config.last_checked_at = config.last_checked_at.trim().to_string();
    if config.status.trim().is_empty() {
        config.status = if config.connected {
            "已连接".to_string()
        } else if config.configured {
            "已启用，待连接".to_string()
        } else {
            "未配置".to_string()
        };
    }
    config
}

pub fn sanitize_remote_view_config(mut config: RemoteViewConfig) -> RemoteViewConfig {
    if config.share_secret.trim().is_empty() {
        config.share_secret = default_share_secret();
    }
    config.share_link_ttl_minutes = config.share_link_ttl_minutes.clamp(5, 24 * 60);
    config
}

pub fn sanitize_model_center_state(state: AdminModelCenterState) -> AdminModelCenterState {
    let mut endpoints = Vec::new();
    for endpoint in state.endpoints {
        if let Ok(endpoint) = sanitize_model_endpoint(endpoint) {
            endpoints.push(endpoint);
        }
    }
    let existing_endpoint_ids = endpoints
        .iter()
        .map(|endpoint| endpoint.model_endpoint_id.clone())
        .collect::<HashSet<_>>();
    for endpoint in default_model_endpoints() {
        if existing_endpoint_ids.contains(&endpoint.model_endpoint_id) {
            continue;
        }
        if let Ok(endpoint) = sanitize_model_endpoint(endpoint) {
            endpoints.push(endpoint);
        }
    }
    endpoints.sort_by(|left, right| {
        left.model_kind
            .as_str()
            .cmp(right.model_kind.as_str())
            .then(left.model_endpoint_id.cmp(&right.model_endpoint_id))
    });

    let mut route_policies = Vec::new();
    for policy in state.route_policies {
        if let Ok(policy) = sanitize_model_route_policy(policy) {
            route_policies.push(policy);
        }
    }

    if route_policies.is_empty() {
        route_policies = default_model_route_policies();
    } else {
        let existing_policy_ids = route_policies
            .iter()
            .map(|policy| policy.route_policy_id.clone())
            .collect::<HashSet<_>>();
        for policy in default_model_route_policies() {
            if existing_policy_ids.contains(&policy.route_policy_id) {
                continue;
            }
            if let Ok(policy) = sanitize_model_route_policy(policy) {
                route_policies.push(policy);
            }
        }
    }
    for policy in &mut route_policies {
        if policy.route_policy_id == DEFAULT_POLICY_SEMANTIC_ROUTER {
            policy.privacy_level = PrivacyLevel::StrictLocal;
            policy
                .fallback_order
                .retain(|kind| !kind.eq_ignore_ascii_case("cloud"));
            if policy.fallback_order.is_empty() {
                policy.fallback_order = vec!["local".to_string(), "sidecar".to_string()];
            }
            if let Some(metadata) = policy.metadata.as_object_mut() {
                metadata.remove("cloud_fallback_scope");
                metadata.remove("redaction_required");
                metadata.insert("local_only".to_string(), json!(true));
                metadata.insert("cloud_fallback_allowed".to_string(), json!(false));
            } else {
                policy.metadata = json!({
                    "capability": "router",
                    "local_only": true,
                    "cloud_fallback_allowed": false,
                });
            }
        }
    }
    route_policies.sort_by(|left, right| left.route_policy_id.cmp(&right.route_policy_id));

    let model_store_root =
        non_empty_opt(&state.model_store_root).unwrap_or_else(default_model_store_root);

    let mut capability_bindings = Vec::new();
    let mut seen_capabilities = HashSet::new();
    for binding in state.capability_bindings {
        let Some(capability_id) = non_empty_opt(&binding.capability_id) else {
            continue;
        };
        let Some(model_id) = non_empty_opt(&binding.model_id) else {
            continue;
        };
        if !seen_capabilities.insert(capability_id.clone()) {
            continue;
        }
        capability_bindings.push(ModelCapabilityBindingRecord {
            capability_id,
            model_id,
            updated_at: non_empty_opt(&binding.updated_at).unwrap_or_else(model_test_timestamp),
        });
    }
    capability_bindings.sort_by(|left, right| left.capability_id.cmp(&right.capability_id));

    let mut runtimes = Vec::new();
    for runtime in state.runtimes {
        if let Ok(runtime) = sanitize_model_runtime(runtime, &model_store_root) {
            runtimes.push(runtime);
        }
    }
    let existing_runtime_ids = runtimes
        .iter()
        .map(|runtime| runtime.runtime_id.clone())
        .collect::<HashSet<_>>();
    for runtime in default_model_runtimes_for_store_root(&model_store_root) {
        if existing_runtime_ids.contains(&runtime.runtime_id) {
            continue;
        }
        if let Ok(runtime) = sanitize_model_runtime(runtime, &model_store_root) {
            runtimes.push(runtime);
        }
    }
    runtimes.sort_by(|left, right| left.runtime_id.cmp(&right.runtime_id));

    AdminModelCenterState {
        endpoints,
        route_policies,
        model_store_root,
        capability_bindings,
        runtimes,
    }
}

pub fn sanitize_model_runtime(
    mut runtime: ModelRuntimeRecord,
    model_store_root: &str,
) -> Result<ModelRuntimeRecord, String> {
    runtime.runtime_id = runtime.runtime_id.trim().to_string();
    if runtime.runtime_id.is_empty() {
        return Err("runtime_id 不能为空".to_string());
    }
    runtime.display_name = non_empty_opt(&runtime.display_name)
        .unwrap_or_else(|| runtime.runtime_id.replace('-', " "));
    runtime.runtime_kind =
        non_empty_opt(&runtime.runtime_kind).unwrap_or_else(|| "managed_runtime".to_string());
    runtime.provider_key =
        non_empty_opt(&runtime.provider_key).unwrap_or_else(|| "harbor".to_string());
    runtime.status = non_empty_opt(&runtime.status).unwrap_or_else(|| "not_installed".to_string());
    runtime.capabilities = sanitize_string_list(runtime.capabilities);
    runtime.runtime_profiles = sanitize_string_list(runtime.runtime_profiles);
    if runtime.capabilities.is_empty() {
        runtime.capabilities = runtime_capabilities_for_id(&runtime.runtime_id);
    }
    if runtime.runtime_profiles.is_empty() {
        runtime.runtime_profiles = runtime_profiles_for_id(&runtime.runtime_id);
    }
    runtime.bind_url = runtime.bind_url.and_then(|value| non_empty_opt(&value));
    runtime.healthz_url = runtime.healthz_url.and_then(|value| non_empty_opt(&value));
    runtime.model_store_path = non_empty_opt(&runtime.model_store_path)
        .unwrap_or_else(|| model_runtime_store_path(model_store_root, &runtime.runtime_id));
    runtime.message = non_empty_opt(&runtime.message)
        .unwrap_or_else(|| default_model_runtime_message(&runtime.runtime_id, &runtime.status));
    runtime.installed_at = runtime.installed_at.and_then(|value| non_empty_opt(&value));
    runtime.updated_at = runtime.updated_at.and_then(|value| non_empty_opt(&value));
    runtime.metadata = normalize_json_object(runtime.metadata);
    if runtime.runtime_id == "harbor-candle" {
        runtime.enabled = true;
        if runtime.status == "not_installed" {
            runtime.status = "idle".to_string();
        }
        if let Some(object) = runtime.metadata.as_object_mut() {
            object
                .entry("install_mode".to_string())
                .or_insert_with(|| json!("bundled_embedded_runtime"));
            object
                .entry("owns_model_store".to_string())
                .or_insert_with(|| json!(true));
            object
                .entry("external_endpoint".to_string())
                .or_insert_with(|| json!(false));
            object
                .entry("default_enabled".to_string())
                .or_insert_with(|| json!(true));
            object
                .entry("lazy_load_models".to_string())
                .or_insert_with(|| json!(true));
            object
                .entry("bootstrap_model_id".to_string())
                .or_insert_with(|| json!("Qwen/Qwen2.5-0.5B-Instruct"));
        }
    }
    Ok(runtime)
}

pub fn sanitize_model_endpoint(mut endpoint: ModelEndpoint) -> Result<ModelEndpoint, String> {
    endpoint.model_endpoint_id = endpoint.model_endpoint_id.trim().to_string();
    if endpoint.model_endpoint_id.is_empty() {
        endpoint.model_endpoint_id = format!(
            "{}-{}-{}",
            endpoint.model_kind.as_str(),
            slugify_identity_component(&endpoint.provider_key),
            slugify_identity_component(&endpoint.model_name)
        )
        .trim_matches('-')
        .to_string();
    }
    if endpoint.model_endpoint_id.is_empty() {
        return Err("model_endpoint_id 不能为空".to_string());
    }

    endpoint.workspace_id = endpoint
        .workspace_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    endpoint.provider_account_id = endpoint
        .provider_account_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    endpoint.provider_key = endpoint.provider_key.trim().to_string();
    if endpoint.provider_key.is_empty() {
        endpoint.provider_key = "custom".to_string();
    }
    endpoint.model_name = endpoint.model_name.trim().to_string();
    if endpoint.model_name.is_empty() {
        endpoint.model_name = endpoint.provider_key.clone();
    }
    endpoint.capability_tags = endpoint
        .capability_tags
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    endpoint.capability_tags.sort();
    endpoint.capability_tags.dedup();
    endpoint.cost_policy = normalize_json_object(endpoint.cost_policy);
    endpoint.metadata = normalize_json_object(endpoint.metadata);
    normalize_builtin_local_model_api_endpoint(&mut endpoint);
    Ok(endpoint)
}

fn normalize_builtin_local_model_api_endpoint(endpoint: &mut ModelEndpoint) {
    if !matches!(
        endpoint.model_endpoint_id.as_str(),
        "embed-local-openai-compatible" | "llm-local-openai-compatible"
    ) {
        return;
    }
    if !endpoint
        .provider_key
        .eq_ignore_ascii_case("openai_compatible")
    {
        return;
    }
    if endpoint.model_endpoint_id == "llm-local-openai-compatible" {
        ensure_model_endpoint_tag(endpoint, "assistant_input_parser");
        ensure_model_endpoint_tag(endpoint, "k3_nsp");
        ensure_model_endpoint_tag(endpoint, "semantic_router");
        set_model_endpoint_metadata_bool(endpoint, "semantic_router", true);
        set_model_endpoint_metadata_bool(endpoint, "local_only", true);
        set_model_endpoint_metadata_bool(endpoint, "cloud_fallback_allowed", false);
    }

    let base_url = model_endpoint_metadata_string(endpoint, "base_url");
    let healthz_url = model_endpoint_metadata_string(endpoint, "healthz_url");
    let legacy_base_url = base_url
        .as_deref()
        .filter(|value| is_legacy_model_api_url(value));
    let legacy_healthz_url = healthz_url
        .as_deref()
        .filter(|value| is_legacy_model_api_url(value));
    if legacy_base_url.is_none() && legacy_healthz_url.is_none() {
        return;
    }

    let Some(default_endpoint) = default_model_endpoints()
        .into_iter()
        .find(|default| default.model_endpoint_id == endpoint.model_endpoint_id)
    else {
        return;
    };

    for key in ["base_url", "healthz_url", "api_key"] {
        if let Some(value) = model_endpoint_metadata_string(&default_endpoint, key) {
            set_model_endpoint_metadata_string(endpoint, key, value);
        }
    }
    set_model_endpoint_metadata_bool(
        endpoint,
        "api_key_configured",
        model_endpoint_metadata_bool(&default_endpoint, "api_key_configured"),
    );
    set_model_endpoint_metadata_bool(endpoint, "legacy_model_api_migrated", true);
    set_model_endpoint_metadata_string(
        endpoint,
        "legacy_model_api_migrated_reason",
        "4176 standalone model API is not part of the release inference path".to_string(),
    );
    if let Some(value) = legacy_base_url {
        set_model_endpoint_metadata_string(
            endpoint,
            "legacy_model_api_migrated_from_base_url",
            value.to_string(),
        );
    }
    if let Some(value) = legacy_healthz_url {
        set_model_endpoint_metadata_string(
            endpoint,
            "legacy_model_api_migrated_from_healthz_url",
            value.to_string(),
        );
    }
}

fn ensure_model_endpoint_tag(endpoint: &mut ModelEndpoint, tag: &str) {
    if !endpoint.capability_tags.iter().any(|value| value == tag) {
        endpoint.capability_tags.push(tag.to_string());
        endpoint.capability_tags.sort();
        endpoint.capability_tags.dedup();
    }
}

fn is_legacy_model_api_url(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    normalized.contains("127.0.0.1:4176") || normalized.contains("localhost:4176")
}

fn model_endpoint_metadata_string(endpoint: &ModelEndpoint, key: &str) -> Option<String> {
    endpoint
        .metadata
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn model_endpoint_metadata_bool(endpoint: &ModelEndpoint, key: &str) -> bool {
    endpoint
        .metadata
        .get(key)
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn set_model_endpoint_metadata_string(endpoint: &mut ModelEndpoint, key: &str, value: String) {
    if let Some(object) = endpoint.metadata.as_object_mut() {
        object.insert(key.to_string(), json!(value));
    }
}

fn set_model_endpoint_metadata_bool(endpoint: &mut ModelEndpoint, key: &str, value: bool) {
    if let Some(object) = endpoint.metadata.as_object_mut() {
        object.insert(key.to_string(), json!(value));
    }
}

pub fn default_model_runtimes() -> Vec<ModelRuntimeRecord> {
    default_model_runtimes_for_store_root(&default_model_store_root())
}

pub fn default_model_runtimes_for_store_root(model_store_root: &str) -> Vec<ModelRuntimeRecord> {
    let local_base_url = local_model_api_base_url();
    let local_healthz_url = local_model_api_healthz_url(&local_base_url);
    vec![
        ModelRuntimeRecord {
            runtime_id: "harbor-candle".to_string(),
            display_name: "Harbor Candle Runtime".to_string(),
            runtime_kind: "embedded_candle".to_string(),
            provider_key: "harbor".to_string(),
            status: "idle".to_string(),
            managed: true,
            installable: true,
            enabled: true,
            capabilities: vec!["llm".to_string(), "embedding".to_string()],
            runtime_profiles: vec![
                "harbor-candle".to_string(),
                "harbor-model-api-candle".to_string(),
            ],
            bind_url: Some(local_base_url),
            healthz_url: Some(local_healthz_url),
            model_store_path: model_runtime_store_path(model_store_root, "harbor-candle"),
            message: "Harbor-managed Candle is enabled by default and lazy-loads Harbor-owned local models without using a user Ollama instance.".to_string(),
            installed_at: None,
            updated_at: None,
            metadata: json!({
                "install_mode": "bundled_embedded_runtime",
                "owns_model_store": true,
                "external_endpoint": false,
                "default_enabled": true,
                "lazy_load_models": true,
                "bootstrap_model_id": "Qwen/Qwen2.5-0.5B-Instruct",
            }),
        },
        ModelRuntimeRecord {
            runtime_id: "harbor-vlm-sidecar".to_string(),
            display_name: "Harbor Vision Runtime".to_string(),
            runtime_kind: "managed_sidecar".to_string(),
            provider_key: "harbor".to_string(),
            status: "not_available".to_string(),
            managed: true,
            installable: false,
            enabled: false,
            capabilities: vec!["vlm".to_string()],
            runtime_profiles: vec!["harbor-vlm-sidecar".to_string()],
            bind_url: None,
            healthz_url: None,
            model_store_path: model_runtime_store_path(model_store_root, "harbor-vlm-sidecar"),
            message: "Harbor Vision Runtime is reserved for managed VLM packages; use advanced OpenAI-compatible endpoint until the package is available.".to_string(),
            installed_at: None,
            updated_at: None,
            metadata: json!({
                "install_mode": "managed_sidecar_package",
                "external_endpoint": false,
            }),
        },
        ModelRuntimeRecord {
            runtime_id: "harbor-ocr-runtime".to_string(),
            display_name: "Harbor OCR Runtime".to_string(),
            runtime_kind: "managed_system_tool".to_string(),
            provider_key: "harbor".to_string(),
            status: "not_available".to_string(),
            managed: true,
            installable: false,
            enabled: false,
            capabilities: vec!["ocr".to_string()],
            runtime_profiles: vec!["harbor-ocr-runtime".to_string()],
            bind_url: None,
            healthz_url: None,
            model_store_path: model_runtime_store_path(model_store_root, "harbor-ocr-runtime"),
            message: "Harbor OCR Runtime slot is managed separately from custom OCR endpoints.".to_string(),
            installed_at: None,
            updated_at: None,
            metadata: json!({
                "install_mode": "managed_system_tool",
                "external_endpoint": false,
            }),
        },
        ModelRuntimeRecord {
            runtime_id: "harbor-asr-runtime".to_string(),
            display_name: "Harbor Speech Runtime".to_string(),
            runtime_kind: "managed_sidecar".to_string(),
            provider_key: "harbor".to_string(),
            status: "not_available".to_string(),
            managed: true,
            installable: false,
            enabled: false,
            capabilities: vec!["asr".to_string()],
            runtime_profiles: vec!["harbor-asr-runtime".to_string()],
            bind_url: None,
            healthz_url: None,
            model_store_path: model_runtime_store_path(model_store_root, "harbor-asr-runtime"),
            message: "Harbor Speech Runtime is reserved for managed ASR packages; use advanced external endpoints until available.".to_string(),
            installed_at: None,
            updated_at: None,
            metadata: json!({
                "install_mode": "managed_sidecar_package",
                "external_endpoint": false,
            }),
        },
    ]
}

fn runtime_capabilities_for_id(runtime_id: &str) -> Vec<String> {
    default_model_runtimes()
        .into_iter()
        .find(|runtime| runtime.runtime_id == runtime_id)
        .map(|runtime| runtime.capabilities)
        .unwrap_or_default()
}

fn runtime_profiles_for_id(runtime_id: &str) -> Vec<String> {
    default_model_runtimes()
        .into_iter()
        .find(|runtime| runtime.runtime_id == runtime_id)
        .map(|runtime| runtime.runtime_profiles)
        .unwrap_or_default()
}

fn default_model_runtime_message(runtime_id: &str, status: &str) -> String {
    default_model_runtimes()
        .into_iter()
        .find(|runtime| runtime.runtime_id == runtime_id)
        .map(|runtime| runtime.message)
        .unwrap_or_else(|| format!("runtime status: {status}"))
}

fn model_runtime_store_path(model_store_root: &str, runtime_id: &str) -> String {
    Path::new(model_store_root)
        .join("runtimes")
        .join(runtime_id)
        .to_string_lossy()
        .into_owned()
}

pub fn sanitize_model_route_policy(
    mut policy: ModelRoutePolicy,
) -> Result<ModelRoutePolicy, String> {
    policy.route_policy_id = policy.route_policy_id.trim().to_string();
    if policy.route_policy_id.is_empty() {
        return Err("route_policy_id 不能为空".to_string());
    }
    policy.workspace_id = policy.workspace_id.trim().to_string();
    if policy.workspace_id.is_empty() {
        policy.workspace_id = DEFAULT_MODEL_WORKSPACE_ID.to_string();
    }
    policy.domain_scope = policy.domain_scope.trim().to_string();
    if policy.domain_scope.is_empty() {
        policy.domain_scope = "retrieval".to_string();
    }
    policy.modality = policy.modality.trim().to_string();
    if policy.modality.is_empty() {
        policy.modality = "text".to_string();
    }
    policy.status = policy.status.trim().to_string();
    if policy.status.is_empty() {
        policy.status = "active".to_string();
    }
    policy.fallback_order = policy
        .fallback_order
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if policy.fallback_order.is_empty() {
        policy.fallback_order = vec![
            "local".to_string(),
            "sidecar".to_string(),
            "cloud".to_string(),
        ];
    }
    policy.metadata = normalize_json_object(policy.metadata);
    Ok(policy)
}

pub fn default_model_endpoints() -> Vec<ModelEndpoint> {
    let local_base_url = local_model_api_base_url();
    let local_healthz_url = local_model_api_healthz_url(&local_base_url);
    let local_api_key = local_model_api_token();
    vec![
        ModelEndpoint {
            model_endpoint_id: "ocr-local-tesseract".to_string(),
            workspace_id: Some(DEFAULT_MODEL_WORKSPACE_ID.to_string()),
            provider_account_id: None,
            model_kind: ModelKind::Ocr,
            endpoint_kind: ModelEndpointKind::Local,
            provider_key: "tesseract".to_string(),
            model_name: "tesseract-cli".to_string(),
            capability_tags: vec![
                "image".to_string(),
                "local_first".to_string(),
                "ocr".to_string(),
            ],
            cost_policy: json!({"cost_hint": "local_cpu"}),
            status: ModelEndpointStatus::Degraded,
            metadata: json!({
                "builtin": true,
                "binary_path": "",
                "languages": "chi_sim+eng",
                "secret_configured": false,
            }),
        },
        ModelEndpoint {
            model_endpoint_id: "embed-local-openai-compatible".to_string(),
            workspace_id: Some(DEFAULT_MODEL_WORKSPACE_ID.to_string()),
            provider_account_id: None,
            model_kind: ModelKind::Embedder,
            endpoint_kind: ModelEndpointKind::Local,
            provider_key: "openai_compatible".to_string(),
            model_name: "harbor-local-embed".to_string(),
            capability_tags: vec!["embeddings".to_string(), "local_first".to_string()],
            cost_policy: json!({"cost_hint": "local_or_sidecar"}),
            status: ModelEndpointStatus::Degraded,
            metadata: json!({
                "builtin": true,
                "base_url": local_base_url.clone(),
                "healthz_url": local_healthz_url.clone(),
                "api_key": local_api_key.clone(),
                "api_key_configured": true,
            }),
        },
        ModelEndpoint {
            model_endpoint_id: "rerank-local-compatible".to_string(),
            workspace_id: Some(DEFAULT_MODEL_WORKSPACE_ID.to_string()),
            provider_account_id: None,
            model_kind: ModelKind::Reranker,
            endpoint_kind: ModelEndpointKind::Local,
            provider_key: "rerank_compatible".to_string(),
            model_name: "local-reranker".to_string(),
            capability_tags: vec!["local_first".to_string(), "rerank".to_string()],
            cost_policy: json!({"cost_hint": "local_or_sidecar"}),
            status: ModelEndpointStatus::Disabled,
            metadata: json!({
                "builtin": true,
                "base_url": local_base_url.clone(),
                "healthz_url": local_healthz_url.clone(),
                "api_key": local_api_key.clone(),
                "api_key_configured": true,
                "rerank_path": "/rerank",
                "cloud_fallback_allowed": false,
            }),
        },
        ModelEndpoint {
            model_endpoint_id: "llm-local-openai-compatible".to_string(),
            workspace_id: Some(DEFAULT_MODEL_WORKSPACE_ID.to_string()),
            provider_account_id: None,
            model_kind: ModelKind::Llm,
            endpoint_kind: ModelEndpointKind::Local,
            provider_key: "openai_compatible".to_string(),
            model_name: "Qwen/Qwen2.5-0.5B-Instruct".to_string(),
            capability_tags: vec![
                "assistant_input_parser".to_string(),
                "chat".to_string(),
                "k3_nsp".to_string(),
                "local_first".to_string(),
                "semantic_router".to_string(),
            ],
            cost_policy: json!({"cost_hint": "local_or_sidecar"}),
            status: ModelEndpointStatus::Degraded,
            metadata: json!({
                "builtin": true,
                "base_url": local_base_url.clone(),
                "healthz_url": local_healthz_url.clone(),
                "api_key": local_api_key.clone(),
                "api_key_configured": true,
                "semantic_router": true,
                "local_only": true,
                "cloud_fallback_allowed": false,
            }),
        },
        ModelEndpoint {
            model_endpoint_id: DEFAULT_SILICONFLOW_ENDPOINT_ID.to_string(),
            workspace_id: Some(DEFAULT_MODEL_WORKSPACE_ID.to_string()),
            provider_account_id: None,
            model_kind: ModelKind::Llm,
            endpoint_kind: ModelEndpointKind::Cloud,
            provider_key: "openai_compatible".to_string(),
            model_name: DEFAULT_SILICONFLOW_MODEL.to_string(),
            capability_tags: vec![
                "chat".to_string(),
                "cloud_fallback".to_string(),
                "openai_compatible".to_string(),
            ],
            cost_policy: json!({
                "cost_hint": "cloud_metered",
                "provider": "siliconflow",
            }),
            status: ModelEndpointStatus::Disabled,
            metadata: json!({
                "builtin": true,
                "provider_label": "SiliconFlow",
                "base_url": DEFAULT_SILICONFLOW_BASE_URL,
                "healthz_url": "https://api.siliconflow.cn/v1/models",
                "api_key": "",
                "api_key_configured": false,
                "model": DEFAULT_SILICONFLOW_MODEL,
                "fallback_scope": [
                    DEFAULT_POLICY_RETRIEVAL_ANSWER,
                ],
                "secret_redaction": "endpoint_metadata",
            }),
        },
        ModelEndpoint {
            model_endpoint_id: "vlm-local-openai-compatible".to_string(),
            workspace_id: Some(DEFAULT_MODEL_WORKSPACE_ID.to_string()),
            provider_account_id: None,
            model_kind: ModelKind::Vlm,
            endpoint_kind: ModelEndpointKind::Local,
            provider_key: "openai_compatible".to_string(),
            model_name: "vision".to_string(),
            capability_tags: vec![
                "image".to_string(),
                "local_first".to_string(),
                "multimodal".to_string(),
            ],
            cost_policy: json!({"cost_hint": "local_or_sidecar"}),
            status: ModelEndpointStatus::Disabled,
            metadata: json!({
                "builtin": true,
                "base_url": local_base_url,
                "healthz_url": local_healthz_url,
                "api_key": local_api_key,
                "api_key_configured": true,
            }),
        },
    ]
}

fn local_model_api_base_url() -> String {
    env::var(MODEL_API_BASE_URL_ENV)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_MODEL_API_BASE_URL.to_string())
}

fn local_model_api_healthz_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if let Some(prefix) = trimmed.strip_suffix("/v1") {
        format!("{prefix}/healthz")
    } else {
        format!("{trimmed}/healthz")
    }
}

fn local_model_api_token() -> String {
    env::var(MODEL_API_TOKEN_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_MODEL_API_TOKEN.to_string())
}

pub fn default_model_route_policies() -> Vec<ModelRoutePolicy> {
    vec![
        ModelRoutePolicy {
            route_policy_id: DEFAULT_POLICY_RETRIEVAL_OCR.to_string(),
            workspace_id: DEFAULT_MODEL_WORKSPACE_ID.to_string(),
            domain_scope: "retrieval".to_string(),
            modality: "image".to_string(),
            privacy_level: PrivacyLevel::StrictLocal,
            local_preferred: true,
            max_cost_per_run: None,
            fallback_order: vec![
                "local".to_string(),
                "sidecar".to_string(),
                "cloud".to_string(),
            ],
            status: "active".to_string(),
            metadata: json!({"capability": "ocr"}),
        },
        ModelRoutePolicy {
            route_policy_id: DEFAULT_POLICY_RETRIEVAL_EMBED.to_string(),
            workspace_id: DEFAULT_MODEL_WORKSPACE_ID.to_string(),
            domain_scope: "retrieval".to_string(),
            modality: "text".to_string(),
            privacy_level: PrivacyLevel::StrictLocal,
            local_preferred: true,
            max_cost_per_run: None,
            fallback_order: vec![
                "local".to_string(),
                "sidecar".to_string(),
                "cloud".to_string(),
            ],
            status: "active".to_string(),
            metadata: json!({"capability": "embed"}),
        },
        ModelRoutePolicy {
            route_policy_id: DEFAULT_POLICY_RETRIEVAL_RERANK.to_string(),
            workspace_id: DEFAULT_MODEL_WORKSPACE_ID.to_string(),
            domain_scope: "retrieval".to_string(),
            modality: "text".to_string(),
            privacy_level: PrivacyLevel::StrictLocal,
            local_preferred: true,
            max_cost_per_run: None,
            fallback_order: vec!["local".to_string(), "sidecar".to_string()],
            status: "active".to_string(),
            metadata: json!({
                "capability": "rerank",
                "local_only": true,
                "cloud_fallback_allowed": false,
            }),
        },
        ModelRoutePolicy {
            route_policy_id: DEFAULT_POLICY_SEMANTIC_ROUTER.to_string(),
            workspace_id: DEFAULT_MODEL_WORKSPACE_ID.to_string(),
            domain_scope: "semantic".to_string(),
            modality: "text".to_string(),
            privacy_level: PrivacyLevel::StrictLocal,
            local_preferred: true,
            max_cost_per_run: None,
            fallback_order: vec!["local".to_string(), "sidecar".to_string()],
            status: "active".to_string(),
            metadata: json!({
                "capability": "router",
                "local_only": true,
                "cloud_fallback_allowed": false,
            }),
        },
        ModelRoutePolicy {
            route_policy_id: DEFAULT_POLICY_RETRIEVAL_ANSWER.to_string(),
            workspace_id: DEFAULT_MODEL_WORKSPACE_ID.to_string(),
            domain_scope: "retrieval".to_string(),
            modality: "text".to_string(),
            privacy_level: PrivacyLevel::AllowRedactedCloud,
            local_preferred: true,
            max_cost_per_run: None,
            fallback_order: vec![
                "local".to_string(),
                "sidecar".to_string(),
                "cloud".to_string(),
            ],
            status: "active".to_string(),
            metadata: json!({"capability": "answer"}),
        },
        ModelRoutePolicy {
            route_policy_id: DEFAULT_POLICY_RETRIEVAL_VISION_SUMMARY.to_string(),
            workspace_id: DEFAULT_MODEL_WORKSPACE_ID.to_string(),
            domain_scope: "retrieval".to_string(),
            modality: "multimodal".to_string(),
            privacy_level: PrivacyLevel::StrictLocal,
            local_preferred: true,
            max_cost_per_run: None,
            fallback_order: vec!["local".to_string(), "sidecar".to_string()],
            status: "degraded".to_string(),
            metadata: json!({
                "capability": "vision_summary",
                "cloud_fallback": false,
            }),
        },
    ]
}

fn optional_trimmed_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
}

fn string_value_or_empty(value: &Value) -> String {
    value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
        .unwrap_or_default()
}

fn normalize_json_object(value: Value) -> Value {
    if value.is_object() {
        value
    } else {
        json!({})
    }
}

fn merge_json_object(existing: Value, patch: Value) -> Result<Value, String> {
    match patch {
        Value::Null => Ok(json!({})),
        Value::Object(patch_map) => {
            let mut merged = match existing {
                Value::Object(existing_map) => existing_map,
                _ => serde_json::Map::new(),
            };
            for (key, value) in patch_map {
                merged.insert(key, value);
            }
            Ok(Value::Object(merged))
        }
        _ => Err("metadata patch 必须是 JSON object".to_string()),
    }
}

fn preserve_model_endpoint_secret_metadata(existing: &ModelEndpoint, incoming: &mut ModelEndpoint) {
    let Value::Object(existing_metadata) = &existing.metadata else {
        return;
    };
    if !incoming.metadata.is_object() {
        incoming.metadata = json!({});
    }
    let Some(incoming_metadata) = incoming.metadata.as_object_mut() else {
        return;
    };
    for key in [
        "api_key",
        "token",
        "secret",
        "password",
        "authorization",
        "bearer_token",
    ] {
        let incoming_value = incoming_metadata
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        let Some(existing_value) = existing_metadata
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        if incoming_value.is_empty() {
            incoming_metadata.insert(key.to_string(), json!(existing_value));
            incoming_metadata.insert(format!("{key}_configured"), json!(true));
        }
    }
}

fn model_test_timestamp() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

fn normalize_loaded_admin_state(state: &mut AdminConsoleState) {
    sanitize_legacy_admin_fields(state);
    hydrate_legacy_views_from_platform(state);
    sanitize_legacy_admin_fields(state);
    state.platform = sync_platform_from_legacy(state);
}

pub fn sanitize_admin_state(state: &mut AdminConsoleState) {
    sanitize_legacy_admin_fields(state);
    state.platform = sync_platform_from_legacy(state);
}

fn sanitize_legacy_admin_fields(state: &mut AdminConsoleState) {
    state.binding = sanitize_binding(state.binding.clone());
    state.defaults = sanitize_defaults(state.defaults.clone());
    state.bridge_provider = sanitize_bridge_provider_config(state.bridge_provider.clone());
    state.remote_view = sanitize_remote_view_config(state.remote_view.clone());
    state.dvr = sanitize_dvr_recording_settings(state.dvr.clone());
    upsert_dvr_knowledge_root(&mut state.knowledge, &state.dvr);
    state.notification_targets = sanitize_notification_targets(state.notification_targets.clone());
    state.automation_reviews = sanitize_automation_reviews(state.automation_reviews.clone());
    state.device_credentials = sanitize_device_credentials(state.device_credentials.clone());
    state.device_evidence = sanitize_device_evidence_records(state.device_evidence.clone());
    state.models = sanitize_model_center_state(state.models.clone());
    state.knowledge = sanitize_knowledge_settings(state.knowledge.clone());
    state.knowledge_index_jobs = sanitize_knowledge_index_jobs(state.knowledge_index_jobs.clone());
    state.home_assistant = sanitize_home_assistant_state(state.home_assistant.clone());
    state.audit_records = sanitize_audit_stream(state.audit_records.clone());
}

pub fn default_home_assistant_exposed_domains() -> Vec<String> {
    [
        "light",
        "switch",
        "sensor",
        "binary_sensor",
        "climate",
        "cover",
        "lock",
        "camera",
        "media_player",
        "scene",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

pub fn redact_home_assistant_state(mut state: HomeAssistantAdminState) -> HomeAssistantAdminState {
    state.access_token = redact_home_assistant_token(&state.access_token);
    state
}

fn sanitize_home_assistant_state(state: HomeAssistantAdminState) -> HomeAssistantAdminState {
    let exposed_domains = sanitize_home_assistant_domains(state.exposed_domains);
    let base_url = state.base_url.trim().trim_end_matches('/').to_string();
    let access_token = state.access_token.trim().to_string();
    let mut last_status = state.last_status.trim().to_ascii_lowercase();
    if last_status.is_empty() {
        last_status = if base_url.is_empty() || access_token.is_empty() {
            "not_configured".to_string()
        } else {
            "configured".to_string()
        };
    }
    HomeAssistantAdminState {
        enabled: state.enabled,
        base_url,
        access_token,
        exposed_domains,
        last_status,
        last_error: state
            .last_error
            .and_then(|value| non_empty_opt(value.trim())),
        last_test_at: state
            .last_test_at
            .and_then(|value| non_empty_opt(value.trim())),
        last_sync_at: state
            .last_sync_at
            .and_then(|value| non_empty_opt(value.trim())),
        entity_count: state.entity_count,
        service_count: state.service_count,
        version: state.version.and_then(|value| non_empty_opt(value.trim())),
        location_name: state
            .location_name
            .and_then(|value| non_empty_opt(value.trim())),
    }
}

fn sanitize_home_assistant_domains(values: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut domains = Vec::new();
    for value in values {
        let normalized = value.trim().to_ascii_lowercase();
        if normalized.is_empty() {
            continue;
        }
        if normalized
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
            && seen.insert(normalized.clone())
        {
            domains.push(normalized);
        }
    }
    if domains.is_empty() {
        return default_home_assistant_exposed_domains();
    }
    domains
}

pub fn home_assistant_token_redaction_marker() -> &'static str {
    HOME_ASSISTANT_TOKEN_REDACTION
}

fn hydrate_legacy_views_from_platform(state: &mut AdminConsoleState) {
    apply_provider_projections_to_legacy(state);
    apply_workspace_projection_to_legacy(state);
    apply_recording_policy_to_legacy(state);
    if !state.platform.identity_bindings.is_empty() {
        state.identity_bindings = legacy_identity_bindings_from_platform(&state.platform);
    }
}

fn apply_workspace_projection_to_legacy(state: &mut AdminConsoleState) {
    let workspace = state
        .platform
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == DEFAULT_WORKSPACE_ID)
        .or_else(|| state.platform.workspaces.first())
        .cloned();
    let Some(workspace) = workspace else {
        return;
    };

    if let Some(binding) = workspace.settings.get("binding") {
        assign_string(&mut state.binding.channel, binding.get("channel"));
        assign_string(&mut state.binding.status, binding.get("status"));
        assign_string(&mut state.binding.metric, binding.get("metric"));
        state.binding.bound_user = optional_string(binding.get("bound_user"));
    }

    if let Some(defaults) = workspace.settings.get("defaults") {
        assign_string(&mut state.defaults.cidr, defaults.get("cidr"));
        assign_string(&mut state.defaults.discovery, defaults.get("discovery"));
        assign_string(&mut state.defaults.capture, defaults.get("capture"));
        assign_string(&mut state.defaults.ai, defaults.get("ai"));
        assign_string(
            &mut state.defaults.notification_channel,
            defaults.get("notification_channel"),
        );
        assign_string(
            &mut state.defaults.rtsp_username,
            defaults.get("rtsp_username"),
        );
        if let Some(port) = defaults.get("rtsp_port").and_then(Value::as_u64) {
            state.defaults.rtsp_port = port as u16;
        }
        if let Some(paths) = string_vec(defaults.get("rtsp_paths")) {
            state.defaults.rtsp_paths = paths;
        }
    }

    if let Some(dvr) = workspace.settings.get("dvr") {
        assign_string(&mut state.dvr.recording_root, dvr.get("recording_root"));
        assign_string(
            &mut state.dvr.media_library_root,
            dvr.get("media_library_root"),
        );
        if let Some(days) = dvr.get("retention_days").and_then(Value::as_u64) {
            state.dvr.retention_days = days as u32;
        }
        if let Some(seconds) = dvr.get("segment_seconds").and_then(Value::as_u64) {
            state.dvr.segment_seconds = seconds as u32;
        }
        if let Some(enabled) = dvr
            .get("continuous_recording_enabled")
            .and_then(Value::as_bool)
        {
            state.dvr.continuous_recording_enabled = enabled;
        }
        if let Some(preferred) = dvr
            .get("low_bitrate_stream_preferred")
            .and_then(Value::as_bool)
        {
            state.dvr.low_bitrate_stream_preferred = preferred;
        }
        if let Some(bitrate) = dvr.get("continuous_bitrate_mbps").and_then(Value::as_u64) {
            state.dvr.continuous_bitrate_mbps = bitrate as u32;
        }
        if let Some(enabled) = dvr
            .get("high_res_event_clips_enabled")
            .and_then(Value::as_bool)
        {
            state.dvr.high_res_event_clips_enabled = enabled;
        }
        if let Some(seconds) = dvr
            .get("high_res_event_clip_seconds")
            .and_then(Value::as_u64)
        {
            state.dvr.high_res_event_clip_seconds = seconds as u32;
        }
        assign_string(
            &mut state.dvr.continuous_stream_path_hint,
            dvr.get("continuous_stream_path_hint"),
        );
        assign_string(
            &mut state.dvr.high_res_stream_path_hint,
            dvr.get("high_res_stream_path_hint"),
        );
        state.dvr.disk_budget_gb = dvr.get("disk_budget_gb").and_then(Value::as_u64);
        if let Some(count) = dvr.get("keyframe_count").and_then(Value::as_u64) {
            state.dvr.keyframe_count = count as u32;
        }
        if let Some(seconds) = dvr.get("keyframe_interval_seconds").and_then(Value::as_u64) {
            state.dvr.keyframe_interval_seconds = seconds as u32;
        }
        if let Some(device_ids) = string_vec(dvr.get("enabled_device_ids")) {
            state.dvr.enabled_device_ids = device_ids;
        }
    }

    state.remote_view = resolved_remote_view_config(state);
}

fn apply_provider_projections_to_legacy(state: &mut AdminConsoleState) {
    if let Some(local_rtsp) = state
        .platform
        .provider_accounts
        .iter()
        .find(|provider| provider.provider_account_id == LOCAL_RTSP_PROVIDER_ACCOUNT_ID)
    {
        assign_string(
            &mut state.defaults.cidr,
            local_rtsp.capabilities.get("cidr"),
        );
        assign_string(
            &mut state.defaults.discovery,
            local_rtsp.capabilities.get("discovery"),
        );
        assign_string(
            &mut state.defaults.rtsp_username,
            local_rtsp.capabilities.get("rtsp_username"),
        );
        if let Some(port) = local_rtsp
            .capabilities
            .get("rtsp_port")
            .and_then(Value::as_u64)
        {
            state.defaults.rtsp_port = port as u16;
        }
        if let Some(paths) = string_vec(local_rtsp.capabilities.get("rtsp_paths")) {
            state.defaults.rtsp_paths = paths;
        }
        assign_string(
            &mut state.defaults.capture,
            local_rtsp.metadata.get("capture_mode"),
        );
        assign_string(&mut state.defaults.ai, local_rtsp.metadata.get("ai_mode"));
    }

    if let Some(bridge_provider) = state
        .platform
        .provider_accounts
        .iter()
        .find(|provider| provider.provider_account_id == BRIDGE_PROVIDER_ACCOUNT_ID)
    {
        state.bridge_provider.connected = bridge_provider.status == ProviderAccountStatus::Active;
        state.bridge_provider.configured =
            !matches!(bridge_provider.status, ProviderAccountStatus::Disabled);
        assign_string(
            &mut state.defaults.notification_channel,
            bridge_provider.capabilities.get("channel"),
        );
        assign_string(
            &mut state.bridge_provider.platform,
            bridge_provider.metadata.get("platform"),
        );
        assign_string(
            &mut state.bridge_provider.app_name,
            bridge_provider.metadata.get("display_name"),
        );
        assign_string(
            &mut state.bridge_provider.status,
            bridge_provider.metadata.get("status"),
        );
        assign_string(
            &mut state.bridge_provider.gateway_base_url,
            bridge_provider.metadata.get("gateway_base_url"),
        );
        assign_string(
            &mut state.bridge_provider.last_checked_at,
            bridge_provider.metadata.get("last_checked_at"),
        );
        state.bridge_provider.capabilities.reply = bridge_provider
            .capabilities
            .get("reply")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        state.bridge_provider.capabilities.update = bridge_provider
            .capabilities
            .get("update")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        state.bridge_provider.capabilities.attachments = bridge_provider
            .capabilities
            .get("attachments")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    }

    if let Some(local_rtsp_credential) = state
        .platform
        .credentials
        .iter()
        .find(|credential| credential.credential_id == LOCAL_RTSP_CREDENTIAL_ID)
    {
        assign_string(
            &mut state.defaults.rtsp_username,
            local_rtsp_credential.scope.get("username"),
        );
        if let Some(port) = local_rtsp_credential
            .scope
            .get("port")
            .and_then(Value::as_u64)
        {
            state.defaults.rtsp_port = port as u16;
        }
    }
}

fn apply_recording_policy_to_legacy(state: &mut AdminConsoleState) {
    let policy = state
        .platform
        .recording_policies
        .iter()
        .find(|policy| policy.recording_policy_id == DEFAULT_RECORDING_POLICY_ID)
        .or_else(|| state.platform.recording_policies.first());
    let Some(policy) = policy else {
        return;
    };

    if let Some(label) = policy
        .metadata
        .get("recording_label")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        state.defaults.recording = label.to_string();
    } else {
        state.defaults.recording = recording_label_from_policy(policy.trigger_mode);
    }
    assign_string(
        &mut state.defaults.capture,
        policy.metadata.get("capture_mode"),
    );
    assign_string(&mut state.defaults.ai, policy.metadata.get("ai_mode"));
    assign_string(
        &mut state.defaults.notification_channel,
        policy.metadata.get("notification_channel"),
    );
    state.defaults.selected_camera_device_id = policy.device_id.as_deref().and_then(non_empty_opt);
    state.defaults.capture_subdirectory = policy
        .capture_subdirectory()
        .map(str::to_string)
        .unwrap_or_else(default_capture_subdirectory);
    state.defaults.clip_length_seconds = policy
        .clip_length_seconds_hint()
        .unwrap_or_else(default_clip_length_seconds);
    state.defaults.keyframe_count = policy
        .keyframe_count_hint()
        .unwrap_or_else(default_keyframe_count);
    state.defaults.keyframe_interval_seconds = policy
        .keyframe_interval_seconds_hint()
        .unwrap_or_else(default_keyframe_interval_seconds);
}

fn legacy_identity_bindings_from_platform(
    platform: &AdminPlatformState,
) -> Vec<IdentityBindingRecord> {
    let mut bindings = Vec::new();
    for binding in &platform.identity_bindings {
        if bindings
            .iter()
            .any(|existing: &IdentityBindingRecord| existing.open_id == binding.external_user_id)
        {
            continue;
        }
        let display_name = platform
            .users
            .iter()
            .find(|user| user.user_id == binding.user_id)
            .map(|user| user.display_name.clone())
            .or_else(|| {
                binding
                    .profile_snapshot
                    .get("display_name")
                    .and_then(Value::as_str)
                    .map(|value| value.to_string())
            })
            .unwrap_or_else(|| binding.external_user_id.clone());
        bindings.push(IdentityBindingRecord {
            open_id: binding.external_user_id.clone(),
            user_id: Some(binding.user_id.clone()),
            union_id: binding.external_union_id.clone(),
            display_name,
            chat_id: binding.external_chat_id.clone(),
        });
    }
    bindings
}

pub fn resolved_identity_binding_records(state: &AdminConsoleState) -> Vec<IdentityBindingRecord> {
    if !state.platform.identity_bindings.is_empty() {
        return legacy_identity_bindings_from_platform(&state.platform);
    }

    state
        .identity_bindings
        .iter()
        .cloned()
        .filter_map(|binding| sanitize_identity_binding_record(binding).ok())
        .collect()
}

pub fn account_management_snapshot(
    state: &AdminConsoleState,
    public_origin: Option<&str>,
) -> AccountManagementSnapshot {
    let workspace = active_workspace_projection(state);
    let bindings = resolved_identity_binding_records(state);
    let membership_counts = member_role_counts(&state.platform, &workspace.workspace_id);
    let permission_rule_count = state
        .platform
        .permission_bindings
        .iter()
        .filter(|binding| binding.workspace_id == workspace.workspace_id)
        .count();

    AccountManagementSnapshot {
        workspace: WorkspaceSummary {
            workspace_id: workspace.workspace_id.clone(),
            display_name: workspace.display_name.clone(),
            workspace_type: workspace_type_value(workspace.workspace_type).to_string(),
            status: workspace_status_value(workspace.status).to_string(),
            timezone: workspace.timezone.clone(),
            locale: workspace.locale.clone(),
            owner_user_id: workspace.owner_user_id.clone(),
            member_count: membership_counts
                .iter()
                .map(|summary| summary.member_count)
                .sum(),
            active_member_count: membership_counts
                .iter()
                .map(|summary| summary.active_member_count)
                .sum(),
            identity_binding_count: bindings.len(),
            permission_rule_count,
            provider_account_count: state
                .platform
                .provider_accounts
                .iter()
                .filter(|provider| provider.workspace_id == workspace.workspace_id)
                .count(),
            credential_count: state
                .platform
                .credentials
                .iter()
                .filter(|credential| {
                    state.platform.provider_accounts.iter().any(|provider| {
                        provider.provider_account_id == credential.provider_account_id
                            && provider.workspace_id == workspace.workspace_id
                    })
                })
                .count(),
            current_principal_user_id: Some(harboros_current_user_id()),
            current_principal_display_name: Some(harboros_current_user_display_name()),
            current_principal_auth_source: Some("harbor_os".to_string()),
        },
        member_role_counts: membership_counts,
        identity_bindings: identity_binding_details(state, &workspace, &bindings),
        access_governance: access_governance_summary(state, &workspace.workspace_id),
        gateway: gateway_status_summary(state, public_origin),
        notification_targets: state.notification_targets.clone(),
        delivery_policy: delivery_policy_summary(),
    }
}

pub fn delivery_policy_summary() -> DeliveryPolicySummary {
    DeliveryPolicySummary {
        interactive_reply: "source_bound".to_string(),
        proactive_delivery: "notification_target_default".to_string(),
    }
}

fn sync_platform_from_legacy(state: &AdminConsoleState) -> AdminPlatformState {
    let mut platform = state.platform.clone();
    let workspace = build_workspace_projection(state);
    upsert_workspace(&mut platform.workspaces, workspace.clone());

    for user in build_user_accounts(state, &workspace) {
        upsert_user(&mut platform.users, user);
    }
    for membership in build_memberships(state, &workspace) {
        let membership = preserve_custom_membership(&platform.memberships, membership);
        upsert_membership(&mut platform.memberships, membership);
    }
    for binding in build_identity_binding_projections(state) {
        upsert_identity_binding(&mut platform.identity_bindings, binding);
    }
    for permission in build_permission_bindings(&workspace) {
        upsert_permission_binding(&mut platform.permission_bindings, permission);
    }

    platform.provider_accounts.retain(|provider| {
        provider.provider_account_id != LOCAL_RTSP_PROVIDER_ACCOUNT_ID
            && provider.provider_account_id != BRIDGE_PROVIDER_ACCOUNT_ID
    });
    platform
        .provider_accounts
        .extend(build_provider_accounts(state));

    platform
        .credentials
        .retain(|credential| credential.credential_id != LOCAL_RTSP_CREDENTIAL_ID);
    dedupe_credentials_by_id(&mut platform.credentials);
    for credential in build_credentials(state) {
        upsert_credential(&mut platform.credentials, credential);
    }
    dedupe_credentials_by_id(&mut platform.credentials);

    platform
        .recording_policies
        .retain(|policy| policy.recording_policy_id != DEFAULT_RECORDING_POLICY_ID);
    platform
        .recording_policies
        .push(build_recording_policy(state));

    platform
}

fn active_workspace_projection(state: &AdminConsoleState) -> Workspace {
    state
        .platform
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == DEFAULT_WORKSPACE_ID)
        .or_else(|| state.platform.workspaces.first())
        .cloned()
        .unwrap_or_else(|| build_workspace_projection(state))
}

fn member_role_counts(platform: &AdminPlatformState, workspace_id: &str) -> Vec<MemberRoleSummary> {
    ordered_role_kinds()
        .iter()
        .map(|role_kind| {
            let (member_count, active_member_count) = platform
                .memberships
                .iter()
                .filter(|membership| membership.workspace_id == workspace_id)
                .filter(|membership| membership.role_kind == *role_kind)
                .fold(
                    (0usize, 0usize),
                    |(member_count, active_count), membership| {
                        (
                            member_count + 1,
                            active_count
                                + usize::from(membership.status == MembershipStatus::Active),
                        )
                    },
                );

            MemberRoleSummary {
                role_kind: role_kind_label(*role_kind).to_string(),
                member_count,
                active_member_count,
            }
        })
        .collect()
}

fn identity_binding_details(
    state: &AdminConsoleState,
    workspace: &Workspace,
    bindings: &[IdentityBindingRecord],
) -> Vec<IdentityBindingSummary> {
    let membership_map: HashMap<&str, &Membership> = state
        .platform
        .memberships
        .iter()
        .filter(|membership| membership.workspace_id == workspace.workspace_id)
        .map(|membership| (membership.user_id.as_str(), membership))
        .collect();
    let user_map: HashMap<&str, &UserAccount> = state
        .platform
        .users
        .iter()
        .map(|user| (user.user_id.as_str(), user))
        .collect();

    let mut summaries = bindings
        .iter()
        .map(|binding| {
            let user_id = projected_user_id_for_binding(binding);
            let membership = membership_map.get(user_id.as_str()).copied();
            let user = user_map.get(user_id.as_str()).copied();
            let proactive_delivery_surface = user
                .and_then(user_default_delivery_surface)
                .unwrap_or_else(|| DEFAULT_PROACTIVE_DELIVERY_SURFACE.to_string());
            let (binding_available, binding_availability, binding_availability_note) =
                binding_availability_for_surface(binding, &proactive_delivery_surface);
            IdentityBindingSummary {
                identity_id: format!("identity-{}", binding.open_id),
                user_id,
                display_name: user
                    .map(|user| user.display_name.clone())
                    .or_else(|| {
                        membership.and_then(|membership| {
                            user_map
                                .get(membership.user_id.as_str())
                                .map(|user| user.display_name.clone())
                        })
                    })
                    .unwrap_or_else(|| binding.display_name.clone()),
                provider_key: "im_bridge".to_string(),
                open_id: binding.open_id.clone(),
                union_id: binding.union_id.clone(),
                chat_id: binding.chat_id.clone(),
                role_kind: membership
                    .map(|membership| role_kind_label(membership.role_kind).to_string())
                    .unwrap_or_else(|| "viewer".to_string()),
                membership_status: membership
                    .map(|membership| membership_status_label(membership.status).to_string())
                    .unwrap_or_else(|| "active".to_string()),
                can_edit: membership
                    .map(|membership| membership.user_id != workspace.owner_user_id)
                    .unwrap_or(true),
                is_owner: membership
                    .map(|membership| {
                        membership.user_id == workspace.owner_user_id
                            || membership.role_kind == RoleKind::Owner
                    })
                    .unwrap_or(false),
                proactive_delivery_surface,
                binding_availability,
                binding_available,
                binding_availability_note,
                recent_interactive_surface: user.and_then(user_recent_interactive_surface),
            }
        })
        .collect::<Vec<_>>();

    summaries.sort_by(|left, right| {
        right
            .is_owner
            .cmp(&left.is_owner)
            .then_with(|| left.display_name.cmp(&right.display_name))
            .then_with(|| left.open_id.cmp(&right.open_id))
    });
    summaries
}

fn access_governance_summary(
    state: &AdminConsoleState,
    workspace_id: &str,
) -> AccessGovernanceSummary {
    let role_policies = ordered_role_kinds()
        .iter()
        .map(|role_kind| {
            let permission_rule_count = state
                .platform
                .permission_bindings
                .iter()
                .filter(|binding| binding.workspace_id == workspace_id)
                .filter(|binding| binding.role_kind == role_kind_label(*role_kind))
                .count();
            let (member_count, active_member_count) = state
                .platform
                .memberships
                .iter()
                .filter(|membership| membership.workspace_id == workspace_id)
                .filter(|membership| membership.role_kind == *role_kind)
                .fold(
                    (0usize, 0usize),
                    |(member_count, active_count), membership| {
                        (
                            member_count + 1,
                            active_count
                                + usize::from(membership.status == MembershipStatus::Active),
                        )
                    },
                );

            RoleGovernanceSummary {
                role_kind: role_kind_label(*role_kind).to_string(),
                permission_rule_count,
                member_count,
                active_member_count,
            }
        })
        .collect::<Vec<_>>();

    let member_count = state
        .platform
        .memberships
        .iter()
        .filter(|membership| membership.workspace_id == workspace_id)
        .count();
    let active_member_count = state
        .platform
        .memberships
        .iter()
        .filter(|membership| membership.workspace_id == workspace_id)
        .filter(|membership| membership.status == MembershipStatus::Active)
        .count();
    let owner_count = state
        .platform
        .memberships
        .iter()
        .filter(|membership| {
            membership.workspace_id == workspace_id
                && (membership.role_kind == RoleKind::Owner
                    || state
                        .platform
                        .workspaces
                        .iter()
                        .find(|workspace| workspace.workspace_id == workspace_id)
                        .map(|workspace| membership.user_id == workspace.owner_user_id)
                        .unwrap_or(false))
        })
        .count();

    AccessGovernanceSummary {
        permission_rule_count: state
            .platform
            .permission_bindings
            .iter()
            .filter(|binding| binding.workspace_id == workspace_id)
            .count(),
        owner_count,
        member_count,
        active_member_count,
        role_policies,
    }
}

fn gateway_status_summary(
    state: &AdminConsoleState,
    _public_origin: Option<&str>,
) -> GatewayStatusSummary {
    let configured_base_url = state.bridge_provider.gateway_base_url.trim();
    GatewayStatusSummary {
        binding_channel: state.binding.channel.clone(),
        binding_status: state.binding.status.clone(),
        binding_metric: state.binding.metric.clone(),
        binding_bound_user: state.binding.bound_user.clone(),
        manage_url: gateway_manage_url(configured_base_url),
        setup_url: String::new(),
        static_setup_url: String::new(),
        bridge_provider: state.bridge_provider.clone(),
    }
}

fn ordered_role_kinds() -> [RoleKind; 6] {
    [
        RoleKind::Owner,
        RoleKind::Admin,
        RoleKind::Operator,
        RoleKind::Member,
        RoleKind::Viewer,
        RoleKind::Guest,
    ]
}

fn role_kind_label(role_kind: RoleKind) -> &'static str {
    match role_kind {
        RoleKind::Owner => "owner",
        RoleKind::Admin => "admin",
        RoleKind::Operator => "operator",
        RoleKind::Member => "member",
        RoleKind::Viewer => "viewer",
        RoleKind::Guest => "guest",
    }
}

fn workspace_type_value(workspace_type: WorkspaceType) -> &'static str {
    match workspace_type {
        WorkspaceType::Home => "home",
        WorkspaceType::Lab => "lab",
        WorkspaceType::Managed => "managed",
    }
}

fn workspace_status_value(status: WorkspaceStatus) -> &'static str {
    match status {
        WorkspaceStatus::Active => "active",
        WorkspaceStatus::Suspended => "suspended",
        WorkspaceStatus::Archived => "archived",
    }
}

fn membership_status_label(status: MembershipStatus) -> &'static str {
    match status {
        MembershipStatus::Active => "active",
        MembershipStatus::Pending => "pending",
        MembershipStatus::Revoked => "revoked",
    }
}

fn upsert_workspace(workspaces: &mut Vec<Workspace>, workspace: Workspace) {
    if let Some(existing) = workspaces
        .iter_mut()
        .find(|existing| existing.workspace_id == workspace.workspace_id)
    {
        *existing = workspace;
    } else {
        workspaces.push(workspace);
    }
}

fn upsert_user(users: &mut Vec<UserAccount>, user: UserAccount) {
    if let Some(existing) = users
        .iter_mut()
        .find(|existing| existing.user_id == user.user_id)
    {
        *existing = user;
    } else {
        users.push(user);
    }
}

fn upsert_provider_account(providers: &mut Vec<ProviderAccount>, provider: ProviderAccount) {
    if let Some(existing) = providers
        .iter_mut()
        .find(|existing| existing.provider_account_id == provider.provider_account_id)
    {
        *existing = provider;
    } else {
        providers.push(provider);
    }
}

fn upsert_credential(credentials: &mut Vec<CredentialRecord>, credential: CredentialRecord) {
    if let Some(existing) = credentials
        .iter_mut()
        .find(|existing| existing.credential_id == credential.credential_id)
    {
        *existing = credential;
    } else {
        credentials.push(credential);
    }
}

fn dedupe_credentials_by_id(credentials: &mut Vec<CredentialRecord>) {
    let mut seen = HashSet::new();
    credentials.retain(|credential| seen.insert(credential.credential_id.clone()));
}

fn preferred_workspace_mut(platform: &mut AdminPlatformState) -> Option<&mut Workspace> {
    let index = platform
        .workspaces
        .iter()
        .position(|workspace| workspace.workspace_id == DEFAULT_WORKSPACE_ID)
        .or_else(|| (!platform.workspaces.is_empty()).then_some(0))?;
    platform.workspaces.get_mut(index)
}

fn set_workspace_binding_projection(workspace: &mut Workspace, binding: &AdminBindingState) {
    if !workspace.settings.is_object() {
        workspace.settings = json!({});
    }
    let Some(settings) = workspace.settings.as_object_mut() else {
        return;
    };
    settings.insert(
        "binding".to_string(),
        json!({
            "channel": binding.channel.clone(),
            "status": binding.status.clone(),
            "metric": binding.metric.clone(),
            "bound_user": binding.bound_user.clone(),
        }),
    );
}

fn set_workspace_remote_view_projection(workspace: &mut Workspace, remote_view: &RemoteViewConfig) {
    if !workspace.settings.is_object() {
        workspace.settings = json!({});
    }
    let Some(settings) = workspace.settings.as_object_mut() else {
        return;
    };
    settings.insert(
        "remote_view".to_string(),
        json!({
            "share_link_ttl_minutes": remote_view.share_link_ttl_minutes,
            "share_secret": remote_view.share_secret.clone(),
            "share_secret_configured": !remote_view.share_secret.trim().is_empty(),
        }),
    );
}

fn set_workspace_dvr_projection(workspace: &mut Workspace, dvr: &DvrRecordingSettings) {
    if !workspace.settings.is_object() {
        workspace.settings = json!({});
    }
    let Some(settings) = workspace.settings.as_object_mut() else {
        return;
    };
    settings.insert(
        "dvr".to_string(),
        json!({
            "recording_root": dvr.recording_root.clone(),
            "media_library_root": dvr.media_library_root.clone(),
            "retention_days": dvr.retention_days,
            "segment_seconds": dvr.segment_seconds,
            "continuous_recording_enabled": dvr.continuous_recording_enabled,
            "low_bitrate_stream_preferred": dvr.low_bitrate_stream_preferred,
            "continuous_bitrate_mbps": dvr.continuous_bitrate_mbps,
            "high_res_event_clips_enabled": dvr.high_res_event_clips_enabled,
            "high_res_event_clip_seconds": dvr.high_res_event_clip_seconds,
            "continuous_stream_path_hint": dvr.continuous_stream_path_hint.clone(),
            "high_res_stream_path_hint": dvr.high_res_stream_path_hint.clone(),
            "disk_budget_gb": dvr.disk_budget_gb,
            "keyframe_count": dvr.keyframe_count,
            "keyframe_interval_seconds": dvr.keyframe_interval_seconds,
            "enabled_device_ids": dvr.enabled_device_ids.clone(),
        }),
    );
}

pub fn resolved_remote_view_config(state: &AdminConsoleState) -> RemoteViewConfig {
    let mut config = sanitize_remote_view_config(state.remote_view.clone());
    let workspace = state
        .platform
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == DEFAULT_WORKSPACE_ID)
        .or_else(|| state.platform.workspaces.first());

    if let Some(remote_view) = workspace.and_then(|workspace| workspace.settings.get("remote_view"))
    {
        assign_string(&mut config.share_secret, remote_view.get("share_secret"));
        if let Some(ttl) = remote_view
            .get("share_link_ttl_minutes")
            .and_then(Value::as_u64)
        {
            config.share_link_ttl_minutes = ttl as u32;
        }
    }

    sanitize_remote_view_config(config)
}

fn ensure_platform_user_exists(
    state: &mut AdminConsoleState,
    workspace_id: &str,
    user_id: &str,
) -> Result<(), String> {
    if state
        .platform
        .users
        .iter()
        .any(|user| user.user_id == user_id)
    {
        return Ok(());
    }

    let bindings = resolved_identity_binding_records(state);
    if let Some(binding) = bindings
        .iter()
        .find(|binding| binding.user_id.as_deref() == Some(user_id))
    {
        state.platform.users.push(UserAccount {
            user_id: user_id.to_string(),
            display_name: binding.display_name.clone(),
            email: None,
            phone: None,
            status: UserStatus::Active,
            default_workspace_id: Some(workspace_id.to_string()),
            preferences: default_user_preferences(&binding.open_id),
        });
        return Ok(());
    }

    Err(format!("未找到 user_id={user_id} 对应的平台用户"))
}

fn upsert_membership(memberships: &mut Vec<Membership>, membership: Membership) {
    if let Some(existing) = memberships
        .iter_mut()
        .find(|existing| existing.membership_id == membership.membership_id)
    {
        *existing = membership;
    } else {
        memberships.push(membership);
    }
}

fn preserve_custom_membership(
    existing_memberships: &[Membership],
    mut membership: Membership,
) -> Membership {
    if membership.role_kind == RoleKind::Owner {
        return membership;
    }

    if let Some(existing) = existing_memberships
        .iter()
        .find(|existing| existing.membership_id == membership.membership_id)
    {
        membership.role_kind = existing.role_kind;
        membership.status = existing.status;
        membership.granted_by_user_id = existing.granted_by_user_id.clone();
        membership.granted_at = existing.granted_at.clone();
    }

    membership
}

fn upsert_identity_binding(bindings: &mut Vec<IdentityBinding>, binding: IdentityBinding) {
    if let Some(existing) = bindings
        .iter_mut()
        .find(|existing| existing.identity_id == binding.identity_id)
    {
        *existing = binding;
    } else {
        bindings.push(binding);
    }
}

fn upsert_permission_binding(
    permissions: &mut Vec<PermissionBinding>,
    permission: PermissionBinding,
) {
    if let Some(existing) = permissions
        .iter_mut()
        .find(|existing| existing.permission_binding_id == permission.permission_binding_id)
    {
        *existing = permission;
    } else {
        permissions.push(permission);
    }
}

fn sanitize_identity_binding_record(
    mut binding: IdentityBindingRecord,
) -> Result<IdentityBindingRecord, String> {
    binding.open_id = binding.open_id.trim().to_string();
    if binding.open_id.is_empty() {
        return Err("open_id 不能为空".to_string());
    }
    binding.display_name = binding.display_name.trim().to_string();
    if binding.display_name.is_empty() {
        binding.display_name = binding.open_id.clone();
    }
    binding.user_id = binding
        .user_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    binding.union_id = binding
        .union_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    binding.chat_id = binding
        .chat_id
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    Ok(binding)
}

fn assign_string(target: &mut String, value: Option<&Value>) {
    if let Some(value) = value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        *target = value.to_string();
    }
}

fn optional_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
}

fn string_vec(value: Option<&Value>) -> Option<Vec<String>> {
    let values = value?.as_array()?;
    let items: Vec<String> = values
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
        .collect();
    if items.is_empty() {
        None
    } else {
        Some(items)
    }
}

fn recording_label_from_policy(trigger_mode: RecordingTriggerMode) -> String {
    match trigger_mode {
        RecordingTriggerMode::Continuous => "持续录制".to_string(),
        RecordingTriggerMode::Event => "按事件录制".to_string(),
        RecordingTriggerMode::Manual => "手动录制".to_string(),
        RecordingTriggerMode::Schedule => "定时录制".to_string(),
    }
}

pub fn build_platform_state(state: &AdminConsoleState) -> AdminPlatformState {
    let workspace = build_workspace_projection(state);
    AdminPlatformState {
        workspaces: vec![workspace.clone()],
        users: build_user_accounts(state, &workspace),
        memberships: build_memberships(state, &workspace),
        identity_bindings: build_identity_binding_projections(state),
        permission_bindings: build_permission_bindings(&workspace),
        provider_accounts: build_provider_accounts(state),
        credentials: build_credentials(state),
        recording_policies: vec![build_recording_policy(state)],
    }
}

fn build_workspace_projection(state: &AdminConsoleState) -> Workspace {
    let mut workspace = Workspace {
        workspace_id: DEFAULT_WORKSPACE_ID.to_string(),
        workspace_type: WorkspaceType::Home,
        display_name: "Harbor Home".to_string(),
        timezone: "Asia/Shanghai".to_string(),
        locale: "zh-CN".to_string(),
        owner_user_id: harboros_current_user_id(),
        status: WorkspaceStatus::Active,
        settings: json!({
            "binding": {
                "channel": state.binding.channel.clone(),
                "status": state.binding.status.clone(),
                "metric": state.binding.metric.clone(),
                "bound_user": state.binding.bound_user.clone(),
            },
            "defaults": {
                "cidr": state.defaults.cidr.clone(),
                "discovery": state.defaults.discovery.clone(),
                "capture": state.defaults.capture.clone(),
                "capture_subdirectory": state.defaults.capture_subdirectory.clone(),
                "ai": state.defaults.ai.clone(),
                "notification_channel": state.defaults.notification_channel.clone(),
                "rtsp_port": state.defaults.rtsp_port,
                "rtsp_paths": state.defaults.rtsp_paths.clone(),
                "rtsp_username": state.defaults.rtsp_username.clone(),
                "selected_camera_device_id": state.defaults.selected_camera_device_id.clone(),
                "clip_length_seconds": state.defaults.clip_length_seconds,
                "keyframe_count": state.defaults.keyframe_count,
                "keyframe_interval_seconds": state.defaults.keyframe_interval_seconds,
                "writable_root": harboros_writable_root(),
            },
        }),
    };
    set_workspace_remote_view_projection(&mut workspace, &state.remote_view);
    set_workspace_dvr_projection(&mut workspace, &state.dvr);
    workspace
}

fn build_provider_accounts(state: &AdminConsoleState) -> Vec<ProviderAccount> {
    let bindings = resolved_identity_binding_records(state);
    let mut providers = vec![ProviderAccount {
        provider_account_id: LOCAL_RTSP_PROVIDER_ACCOUNT_ID.to_string(),
        workspace_id: DEFAULT_WORKSPACE_ID.to_string(),
        provider_key: "local_rtsp".to_string(),
        provider_kind: ProviderKind::VendorLocal,
        display_name: "本地 RTSP 默认接入".to_string(),
        owner_scope: ProviderOwnerScope::Workspace,
        owner_user_id: None,
        status: ProviderAccountStatus::Active,
        capabilities: json!({
            "cidr": state.defaults.cidr.clone(),
            "discovery": state.defaults.discovery.clone(),
            "rtsp_port": state.defaults.rtsp_port,
            "rtsp_paths": state.defaults.rtsp_paths.clone(),
            "rtsp_username": state.defaults.rtsp_username.clone(),
        }),
        metadata: json!({
            "capture_mode": state.defaults.capture.clone(),
            "ai_mode": state.defaults.ai.clone(),
        }),
    }];

    if let Some(provider) = build_bridge_provider_account(
        &state.bridge_provider,
        &state.defaults.notification_channel,
        bindings.len(),
    ) {
        providers.push(provider);
    }

    providers
}

fn build_bridge_provider_account(
    config: &BridgeProviderConfig,
    notification_channel: &str,
    bound_users: usize,
) -> Option<ProviderAccount> {
    if !config.configured
        && !config.connected
        && config.app_name.trim().is_empty()
        && config.gateway_base_url.trim().is_empty()
    {
        return None;
    }

    Some(ProviderAccount {
        provider_account_id: BRIDGE_PROVIDER_ACCOUNT_ID.to_string(),
        workspace_id: DEFAULT_WORKSPACE_ID.to_string(),
        provider_key: "im_bridge".to_string(),
        provider_kind: ProviderKind::Bridge,
        display_name: DEFAULT_PROVIDER_ACCOUNT_DISPLAY_NAME.to_string(),
        owner_scope: ProviderOwnerScope::Workspace,
        owner_user_id: None,
        status: if config.connected {
            ProviderAccountStatus::Active
        } else if config.configured {
            ProviderAccountStatus::NeedsReauth
        } else {
            ProviderAccountStatus::Disabled
        },
        capabilities: json!({
            "channel": notification_channel,
            "bound_users": bound_users,
            "reply": config.capabilities.reply,
            "update": config.capabilities.update,
            "attachments": config.capabilities.attachments,
        }),
        metadata: json!({
            "platform": config.platform.clone(),
            "display_name": config.app_name.clone(),
            "status": config.status.clone(),
            "gateway_base_url": config.gateway_base_url.clone(),
            "last_checked_at": config.last_checked_at.clone(),
        }),
    })
}

fn build_user_accounts(state: &AdminConsoleState, workspace: &Workspace) -> Vec<UserAccount> {
    let bindings = resolved_identity_binding_records(state);
    let mut users = vec![preserve_custom_user_account(
        &state.platform.users,
        UserAccount {
            user_id: workspace.owner_user_id.clone(),
            display_name: harboros_current_user_display_name(),
            email: None,
            phone: None,
            status: UserStatus::Active,
            default_workspace_id: Some(workspace.workspace_id.clone()),
            preferences: json!({
                "bootstrap": true,
                "channel": "harbor_os",
                "auth_source": "harbor_os",
            }),
        },
    )];

    for binding in &bindings {
        let user_id = projected_user_id_for_binding(binding);
        if users.iter().any(|user| user.user_id == user_id) {
            continue;
        }
        users.push(preserve_custom_user_account(
            &state.platform.users,
            build_user_account_projection(binding, &workspace.workspace_id),
        ));
    }

    users
}

fn build_user_account_projection(
    binding: &IdentityBindingRecord,
    workspace_id: &str,
) -> UserAccount {
    UserAccount {
        user_id: projected_user_id_for_binding(binding),
        display_name: binding.display_name.clone(),
        email: None,
        phone: None,
        status: UserStatus::Active,
        default_workspace_id: Some(workspace_id.to_string()),
        preferences: default_user_preferences(&binding.open_id),
    }
}

fn default_user_preferences(open_id: &str) -> Value {
    json!({
        "auth_source": "im_bridge",
        "open_id": open_id,
        "delivery": {
            "default_surface": DEFAULT_PROACTIVE_DELIVERY_SURFACE,
        }
    })
}

fn preserve_custom_user_account(
    existing_users: &[UserAccount],
    mut user: UserAccount,
) -> UserAccount {
    let Some(existing) = existing_users
        .iter()
        .find(|existing| existing.user_id == user.user_id)
    else {
        return user;
    };

    if !existing.display_name.trim().is_empty() {
        user.display_name = existing.display_name.clone();
    }
    if existing.email.is_some() {
        user.email = existing.email.clone();
    }
    if existing.phone.is_some() {
        user.phone = existing.phone.clone();
    }
    user.status = existing.status;
    if existing.default_workspace_id.is_some() {
        user.default_workspace_id = existing.default_workspace_id.clone();
    }
    let keep_existing_preferences = match &existing.preferences {
        Value::Null => false,
        Value::Object(map) => !map.is_empty(),
        _ => true,
    };
    if keep_existing_preferences {
        user.preferences = existing.preferences.clone();
    }

    user
}

pub fn normalize_delivery_surface(value: &str) -> Option<String> {
    match value.trim().to_ascii_lowercase().as_str() {
        "feishu" => Some("feishu".to_string()),
        "weixin" => Some("weixin".to_string()),
        _ => None,
    }
}

pub fn user_default_delivery_surface(user: &UserAccount) -> Option<String> {
    user.preferences
        .pointer("/delivery/default_surface")
        .and_then(Value::as_str)
        .and_then(normalize_delivery_surface)
        .or_else(|| Some(DEFAULT_PROACTIVE_DELIVERY_SURFACE.to_string()))
}

pub fn user_recent_interactive_surface(user: &UserAccount) -> Option<String> {
    user.preferences
        .pointer("/delivery/recent_interactive_surface")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
}

fn set_user_default_delivery_surface(user: &mut UserAccount, surface: &str) {
    if !user.preferences.is_object() {
        user.preferences = json!({});
    }
    let Some(root) = user.preferences.as_object_mut() else {
        return;
    };
    let delivery = root
        .entry("delivery".to_string())
        .or_insert_with(|| json!({}));
    if !delivery.is_object() {
        *delivery = json!({});
    }
    if let Some(delivery_obj) = delivery.as_object_mut() {
        delivery_obj.insert(
            "default_surface".to_string(),
            Value::String(surface.to_string()),
        );
    }
}

fn set_user_recent_interactive_surface(
    user: &mut UserAccount,
    surface: &str,
    route_key: Option<&str>,
) {
    if !user.preferences.is_object() {
        user.preferences = json!({});
    }
    let Some(root) = user.preferences.as_object_mut() else {
        return;
    };
    let delivery = root
        .entry("delivery".to_string())
        .or_insert_with(|| json!({}));
    if !delivery.is_object() {
        *delivery = json!({});
    }
    if let Some(delivery_obj) = delivery.as_object_mut() {
        delivery_obj.insert(
            "recent_interactive_surface".to_string(),
            Value::String(surface.to_string()),
        );
        if let Some(route_key) = route_key.map(str::trim).filter(|value| !value.is_empty()) {
            delivery_obj.insert(
                "recent_interactive_route_key".to_string(),
                Value::String(route_key.to_string()),
            );
        }
    }
}

fn binding_availability_for_surface(
    binding: &IdentityBindingRecord,
    surface: &str,
) -> (bool, String, String) {
    let available = binding
        .chat_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
        || !binding.open_id.trim().is_empty();
    if available {
        (
            true,
            "available".to_string(),
            format!("{surface} binding is available via HarborGate-owned identity linkage"),
        )
    } else {
        (
            false,
            "blocked".to_string(),
            format!("{surface} binding is missing an open_id/chat_id projection"),
        )
    }
}

fn build_memberships(state: &AdminConsoleState, workspace: &Workspace) -> Vec<Membership> {
    let bindings = resolved_identity_binding_records(state);
    let mut memberships = vec![Membership {
        membership_id: format!("membership-{}", workspace.owner_user_id),
        workspace_id: workspace.workspace_id.clone(),
        user_id: workspace.owner_user_id.clone(),
        role_kind: RoleKind::Owner,
        status: MembershipStatus::Active,
        granted_by_user_id: None,
        granted_at: None,
    }];

    for binding in &bindings {
        let user_id = projected_user_id_for_binding(binding);
        if user_id == workspace.owner_user_id
            || memberships
                .iter()
                .any(|membership| membership.user_id == user_id)
        {
            continue;
        }
        memberships.push(build_membership_projection(workspace, binding));
    }

    memberships
}

fn build_membership_projection(
    workspace: &Workspace,
    binding: &IdentityBindingRecord,
) -> Membership {
    let user_id = projected_user_id_for_binding(binding);
    Membership {
        membership_id: format!("membership-{user_id}"),
        workspace_id: workspace.workspace_id.clone(),
        user_id,
        role_kind: RoleKind::Viewer,
        status: MembershipStatus::Active,
        granted_by_user_id: Some(workspace.owner_user_id.clone()),
        granted_at: None,
    }
}

fn build_identity_binding_projections(state: &AdminConsoleState) -> Vec<IdentityBinding> {
    resolved_identity_binding_records(state)
        .iter()
        .map(build_identity_binding_projection)
        .collect()
}

fn build_identity_binding_projection(binding: &IdentityBindingRecord) -> IdentityBinding {
    IdentityBinding {
        identity_id: format!("identity-{}", binding.open_id),
        user_id: projected_user_id_for_binding(binding),
        auth_source: AuthSource::ImChannel,
        provider_key: "im_bridge".to_string(),
        external_user_id: binding.open_id.clone(),
        external_union_id: binding.union_id.clone(),
        external_chat_id: binding.chat_id.clone(),
        profile_snapshot: json!({
            "display_name": binding.display_name.clone(),
        }),
        last_seen_at: None,
    }
}

fn build_permission_bindings(workspace: &Workspace) -> Vec<PermissionBinding> {
    let workspace_id = workspace.workspace_id.clone();
    vec![
        allow_workspace_permission(&workspace_id, RoleKind::Owner, "*", "*"),
        allow_workspace_permission(&workspace_id, RoleKind::Admin, "*", "admin.*"),
        allow_workspace_permission(&workspace_id, RoleKind::Admin, "*", "camera.*"),
        allow_workspace_permission(&workspace_id, RoleKind::Admin, "*", "approval.*"),
        allow_workspace_permission(&workspace_id, RoleKind::Operator, "*", "admin.read_state"),
        allow_workspace_permission(&workspace_id, RoleKind::Operator, "*", "camera.view"),
        allow_workspace_permission(&workspace_id, RoleKind::Operator, "*", "camera.operate"),
        allow_workspace_permission(&workspace_id, RoleKind::Member, "*", "camera.view"),
        allow_workspace_permission(&workspace_id, RoleKind::Viewer, "*", "camera.view"),
    ]
}

fn allow_workspace_permission(
    workspace_id: &str,
    role_kind: RoleKind,
    resource_pattern: &str,
    action_pattern: &str,
) -> PermissionBinding {
    PermissionBinding {
        permission_binding_id: format!(
            "perm-{workspace_id}-{}-{}",
            role_kind_key(role_kind),
            action_pattern.replace('.', "_").replace('*', "all")
        ),
        workspace_id: workspace_id.to_string(),
        role_kind: role_kind_key(role_kind).to_string(),
        scope_kind: ScopeKind::Workspace,
        resource_pattern: resource_pattern.to_string(),
        action_pattern: action_pattern.to_string(),
        effect: PermissionEffect::Allow,
        constraints: json!({}),
    }
}

fn role_kind_key(role_kind: RoleKind) -> &'static str {
    match role_kind {
        RoleKind::Owner => "owner",
        RoleKind::Admin => "admin",
        RoleKind::Operator => "operator",
        RoleKind::Member => "member",
        RoleKind::Viewer => "viewer",
        RoleKind::Guest => "guest",
    }
}

fn projected_user_id_for_binding(binding: &IdentityBindingRecord) -> String {
    binding
        .user_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
        .unwrap_or_else(|| format!("im-user-{}", slugify_identity_component(&binding.open_id)))
}

fn slugify_identity_component(value: &str) -> String {
    let mut compact = String::new();
    for ch in value.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            compact.push(ch.to_ascii_lowercase());
        } else if matches!(ch, '-' | '_' | '.') {
            compact.push('-');
        }
    }
    while compact.contains("--") {
        compact = compact.replace("--", "-");
    }
    compact.trim_matches('-').to_string()
}

pub fn device_rtsp_credential_id(device_id: &str) -> String {
    let slug = slugify_identity_component(device_id);
    format!(
        "credential-device-rtsp-{}",
        if slug.is_empty() {
            "device"
        } else {
            slug.as_str()
        }
    )
}

fn build_credentials(state: &AdminConsoleState) -> Vec<CredentialRecord> {
    let mut credentials = Vec::new();
    if !state.defaults.rtsp_password.trim().is_empty() {
        credentials.push(CredentialRecord {
            credential_id: LOCAL_RTSP_CREDENTIAL_ID.to_string(),
            provider_account_id: LOCAL_RTSP_PROVIDER_ACCOUNT_ID.to_string(),
            credential_kind: CredentialKind::SessionSecret,
            vault_key: "admin_console.defaults.rtsp_password".to_string(),
            scope: json!({
                "username": state.defaults.rtsp_username.clone(),
                "port": state.defaults.rtsp_port,
            }),
            expires_at: None,
            rotation_state: CredentialRotationState::Valid,
            last_verified_at: None,
            metadata: json!({
                "present": true,
                "path_count": state.defaults.rtsp_paths.len(),
            }),
        });
    }
    for device_credential in &state.device_credentials {
        if device_credential.password.trim().is_empty() {
            continue;
        }
        credentials.push(CredentialRecord {
            credential_id: device_rtsp_credential_id(&device_credential.device_id),
            provider_account_id: LOCAL_RTSP_PROVIDER_ACCOUNT_ID.to_string(),
            credential_kind: CredentialKind::SessionSecret,
            vault_key: format!(
                "admin_console.device_credentials.{}.rtsp_password",
                slugify_identity_component(&device_credential.device_id)
            ),
            scope: json!({
                "device_id": device_credential.device_id.clone(),
                "username": device_credential.username.clone(),
                "port": device_credential.rtsp_port,
            }),
            expires_at: None,
            rotation_state: CredentialRotationState::Valid,
            last_verified_at: device_credential.last_verified_at.clone(),
            metadata: json!({
                "present": true,
                "redacted": true,
                "path_count": device_credential.rtsp_paths.len(),
                "updated_at": device_credential.updated_at.clone(),
            }),
        });
    }
    credentials
}

fn build_recording_policy(state: &AdminConsoleState) -> RecordingPolicy {
    let trigger_mode = recording_trigger_mode_from_label(&state.defaults.recording);
    RecordingPolicy {
        recording_policy_id: DEFAULT_RECORDING_POLICY_ID.to_string(),
        workspace_id: DEFAULT_WORKSPACE_ID.to_string(),
        device_id: state.defaults.selected_camera_device_id.clone(),
        trigger_mode,
        pre_event_seconds: if trigger_mode == RecordingTriggerMode::Event {
            15
        } else {
            0
        },
        post_event_seconds: if trigger_mode == RecordingTriggerMode::Event {
            30
        } else {
            0
        },
        clip_length_seconds: state.defaults.clip_length_seconds,
        retention_days: 30,
        storage_target: StorageTargetKind::Nas,
        metadata: json!({
            "recording_label": state.defaults.recording.clone(),
            "capture_mode": state.defaults.capture.clone(),
            "ai_mode": state.defaults.ai.clone(),
            "notification_channel": state.defaults.notification_channel.clone(),
            "capture_subdirectory": state.defaults.capture_subdirectory.clone(),
            "keyframe_count": state.defaults.keyframe_count,
            "keyframe_interval_seconds": state.defaults.keyframe_interval_seconds,
        }),
    }
}

fn recording_trigger_mode_from_label(label: &str) -> RecordingTriggerMode {
    let normalized = label.trim().to_lowercase();
    if normalized.contains("continuous") || normalized.contains("持续") {
        RecordingTriggerMode::Continuous
    } else if normalized.contains("manual") || normalized.contains("手动") {
        RecordingTriggerMode::Manual
    } else if normalized.contains("schedule")
        || normalized.contains("计划")
        || normalized.contains("定时")
    {
        RecordingTriggerMode::Schedule
    } else {
        RecordingTriggerMode::Event
    }
}

pub fn default_rtsp_username() -> String {
    "admin".to_string()
}

pub fn default_share_secret() -> String {
    let primary = Uuid::new_v4().simple().to_string();
    let secondary = Uuid::new_v4().simple().to_string();
    format!("{primary}{secondary}")
}

pub fn default_share_link_ttl_minutes() -> u32 {
    120
}

pub fn default_scan_cidr() -> String {
    detect_primary_private_ipv4_cidr().unwrap_or_else(|| "192.168.3.0/24".to_string())
}

pub fn default_rtsp_port() -> u16 {
    554
}

pub fn default_rtsp_paths() -> Vec<String> {
    crate::runtime::discovery::default_rtsp_paths()
}

pub fn default_capture_subdirectory() -> String {
    "camera-archive".to_string()
}

pub fn default_clip_length_seconds() -> u32 {
    12
}

pub fn default_keyframe_count() -> u32 {
    4
}

pub fn default_keyframe_interval_seconds() -> u32 {
    3
}

fn default_true() -> bool {
    true
}

pub fn default_knowledge_index_root() -> String {
    Path::new(&harboros_writable_root())
        .join(DEFAULT_KNOWLEDGE_INDEX_SUBDIR)
        .to_string_lossy()
        .into_owned()
}

pub fn default_model_store_root() -> String {
    Path::new(&harboros_writable_root())
        .join("model-store")
        .to_string_lossy()
        .into_owned()
}

pub fn harboros_current_user_id() -> String {
    env::var(HARBOROS_CURRENT_USER_ENV)
        .ok()
        .and_then(|value| non_empty_opt(&value))
        .unwrap_or_else(|| DEFAULT_WORKSPACE_OWNER_ID.to_string())
}

pub fn harboros_current_user_display_name() -> String {
    harboros_current_user_id()
}

pub fn harboros_writable_root() -> String {
    env::var(HARBOROS_WRITABLE_ROOT_ENV)
        .ok()
        .and_then(|value| non_empty_opt(&value))
        .unwrap_or_else(|| DEFAULT_HARBOROS_WRITABLE_ROOT.to_string())
}

fn detect_primary_private_ipv4_cidr() -> Option<String> {
    let interfaces = get_if_addrs().ok()?;
    for iface in interfaces {
        let IfAddr::V4(v4) = iface.addr else {
            continue;
        };
        if v4.ip.is_loopback() || !is_private_ipv4(v4.ip) {
            continue;
        }
        let prefix = netmask_prefix(v4.netmask)?;
        let network = Ipv4Addr::from(u32::from(v4.ip) & u32::from(v4.netmask));
        return Some(format!("{network}/{prefix}"));
    }
    None
}

fn is_private_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 10
        || (octets[0] == 172 && (16..=31).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 168)
}

fn netmask_prefix(netmask: Ipv4Addr) -> Option<u8> {
    let ones = u32::from(netmask).count_ones();
    (ones <= 32).then_some(ones as u8)
}

pub fn dedupe_rtsp_paths(paths: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut normalized = Vec::new();
    for path in paths {
        let trimmed = path.trim();
        if trimmed.is_empty() {
            continue;
        }
        let formatted = if trimmed.starts_with('/') {
            trimmed.to_string()
        } else {
            format!("/{trimmed}")
        };
        if seen.insert(formatted.clone()) {
            normalized.push(formatted);
        }
    }
    normalized
}

pub fn generate_binding_code() -> String {
    let token = Uuid::new_v4().simple().to_string().to_uppercase();
    format!("{}-{}", &token[0..4], &token[4..8])
}

pub fn generate_qr_token(session_code: &str) -> String {
    format!("hub://bind/im_bridge/{session_code}")
}

pub fn normalize_binding_code(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    let without_prefix = trimmed
        .strip_prefix("hub://bind/")
        .map(|value| value.rsplit('/').next().unwrap_or(value))
        .unwrap_or(trimmed)
        .trim();
    let compact: String = without_prefix
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_uppercase())
        .collect();
    if compact.len() != 8 {
        return None;
    }

    Some(format!("{}-{}", &compact[0..4], &compact[4..8]))
}

pub fn derive_rtsp_hints(devices: &[CameraDevice]) -> Option<RtspHints> {
    let mut paths = Vec::new();
    for device in devices {
        let url = device.primary_stream.url.trim();
        if let Some((username, password, path)) = parse_rtsp_auth(url) {
            paths.push(path);
            return Some(RtspHints {
                username,
                password,
                paths: dedupe_rtsp_paths(paths),
            });
        }

        if let Some(path) = parse_rtsp_path(url) {
            paths.push(path);
        }
    }

    None
}

pub fn parse_rtsp_auth(url: &str) -> Option<(String, String, String)> {
    let without_scheme = url.strip_prefix("rtsp://")?;
    let at_index = without_scheme.find('@')?;
    let auth = &without_scheme[..at_index];
    let path = parse_rtsp_path(url)?;
    let mut parts = auth.splitn(2, ':');
    let username = parts.next()?.trim();
    let password = parts.next()?.trim();
    if username.is_empty() || password.is_empty() {
        return None;
    }
    Some((username.to_string(), password.to_string(), path))
}

pub fn parse_rtsp_path(url: &str) -> Option<String> {
    let without_scheme = url.strip_prefix("rtsp://")?;
    let slash_index = without_scheme.find('/')?;
    let path = &without_scheme[slash_index..];
    if path.is_empty() {
        None
    } else {
        Some(path.to_string())
    }
}

fn latest_model_download_job_index(
    jobs: &[ModelDownloadJobRecord],
    model_id: &str,
) -> Option<usize> {
    jobs.iter()
        .enumerate()
        .filter(|(_, job)| job.model_id == model_id)
        .max_by(|(_, left), (_, right)| {
            model_download_job_record_sort_key(left).cmp(&model_download_job_record_sort_key(right))
        })
        .map(|(index, _)| index)
}

fn model_download_job_record_sort_key(job: &ModelDownloadJobRecord) -> (u64, u64, &str) {
    (
        model_download_job_record_timestamp(&job.updated_at),
        model_download_job_record_timestamp(&job.requested_at),
        job.job_id.as_str(),
    )
}

fn model_download_job_record_timestamp(value: &str) -> u64 {
    value.trim().parse::<u64>().unwrap_or(0)
}

fn model_download_job_record_is_active(job: &ModelDownloadJobRecord) -> bool {
    matches!(
        job.status.as_str(),
        "queued" | "running" | "downloading" | "installing"
    )
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use crate::control_plane::auth::{AuthSource, IdentityBinding};
    use crate::control_plane::credentials::{
        CredentialKind, CredentialRecord, CredentialRotationState,
    };
    use crate::control_plane::media::RecordingTriggerMode;
    use crate::control_plane::models::{
        ModelEndpoint, ModelEndpointKind, ModelEndpointStatus, ModelKind, PrivacyLevel,
    };
    use crate::control_plane::users::{
        Membership, MembershipStatus, RoleKind, UserAccount, UserStatus,
    };
    use crate::runtime::registry::CameraDevice;
    use serde_json::json;

    use super::{
        account_management_snapshot, build_platform_state, dedupe_rtsp_paths,
        default_model_endpoints, default_model_route_policies, default_model_runtimes,
        default_model_store_root, default_rtsp_paths, derive_rtsp_hints, device_rtsp_credential_id,
        normalize_binding_code, normalize_loaded_admin_state, parse_rtsp_auth, parse_rtsp_path,
        resolved_identity_binding_records, resolved_remote_view_config,
        sanitize_bridge_provider_config, sanitize_defaults, sanitize_knowledge_settings,
        sanitize_model_center_state, sync_platform_from_legacy, user_default_delivery_surface,
        user_recent_interactive_surface, AdminConsoleState, AdminConsoleStore, AdminDefaults,
        AdminModelCenterState, AutomationRuleReview, BridgeProviderCapabilities,
        BridgeProviderConfig, DeviceCredentialSecret, DeviceEvidenceRecord, DvrRecordingSettings,
        HomeAssistantConfigUpdate, IdentityBindingRecord, KnowledgeSettings, KnowledgeSourceRoot,
        RemoteViewConfig, BRIDGE_PROVIDER_ACCOUNT_ID, LOCAL_RTSP_CREDENTIAL_ID,
        LOCAL_RTSP_PROVIDER_ACCOUNT_ID,
    };

    fn temp_path(name: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("harborbeacon-{name}-{unique}.json"))
    }

    #[test]
    fn old_knowledge_settings_json_defaults_retrieval_config() {
        let settings = serde_json::from_value::<KnowledgeSettings>(json!({
            "source_roots": [],
            "index_root": ".harborbeacon/knowledge-index",
            "privacy_level": "strict_local",
            "default_resource_profile": "cpu_only"
        }))
        .expect("old knowledge settings json");
        let settings = sanitize_knowledge_settings(settings);

        assert_eq!(settings.retrieval.fusion_strategy, "rrf");
        assert_eq!(settings.retrieval.candidate_limit, 80);
        assert!(settings.retrieval.rerank_enabled);
        assert!(settings.retrieval.mmr_enabled);
    }

    #[test]
    fn default_rerank_policy_is_strict_local_without_cloud_fallback() {
        let endpoint = default_model_endpoints()
            .into_iter()
            .find(|endpoint| endpoint.model_endpoint_id == "rerank-local-compatible")
            .expect("default rerank endpoint");
        assert_eq!(endpoint.model_kind, ModelKind::Reranker);
        assert_eq!(endpoint.endpoint_kind, ModelEndpointKind::Local);
        assert_eq!(endpoint.provider_key, "rerank_compatible");
        assert_eq!(endpoint.status, ModelEndpointStatus::Disabled);

        let policy = default_model_route_policies()
            .into_iter()
            .find(|policy| policy.route_policy_id == "retrieval.rerank")
            .expect("default rerank policy");
        assert_eq!(policy.privacy_level, PrivacyLevel::StrictLocal);
        assert_eq!(
            policy.fallback_order,
            vec!["local".to_string(), "sidecar".to_string()]
        );
        assert!(!policy
            .fallback_order
            .iter()
            .any(|kind| kind.eq_ignore_ascii_case("cloud")));
    }

    #[test]
    fn parse_rtsp_auth_extracts_user_pass_and_path() {
        let parsed = parse_rtsp_auth("rtsp://admin:secret@192.168.3.73:554/ch1/main")
            .expect("auth should parse");
        assert_eq!(parsed.0, "admin");
        assert_eq!(parsed.1, "secret");
        assert_eq!(parsed.2, "/ch1/main");
    }

    #[test]
    fn parse_rtsp_path_handles_urls_without_auth() {
        let path = parse_rtsp_path("rtsp://192.168.3.73:554/Streaming/Channels/101").expect("path");
        assert_eq!(path, "/Streaming/Channels/101");
    }

    #[test]
    fn dedupe_rtsp_paths_normalizes_leading_slashes() {
        let paths = dedupe_rtsp_paths(vec![
            "ch1/main".to_string(),
            "/ch1/main".to_string(),
            " /Streaming/Channels/101 ".to_string(),
        ]);
        assert_eq!(paths, vec!["/ch1/main", "/Streaming/Channels/101"]);
    }

    #[test]
    fn default_rtsp_paths_include_tp_link_stream_candidates() {
        let paths = default_rtsp_paths();
        assert!(paths.contains(&"/stream1".to_string()));
        assert!(paths.contains(&"/stream2".to_string()));
    }

    #[test]
    fn save_home_assistant_config_redacts_and_preserves_secret() {
        let registry_path = temp_path("registry-ha-config");
        let admin_path = temp_path("admin-ha-config");
        let registry = crate::runtime::registry::DeviceRegistryStore::new(registry_path.clone());
        let store = AdminConsoleStore::new(admin_path.clone(), registry);

        store
            .save_home_assistant_config(HomeAssistantConfigUpdate {
                enabled: true,
                base_url: " http://ha.local:8123/ ".to_string(),
                access_token: Some("ha-token".to_string()),
                exposed_domains: vec![
                    "Light".to_string(),
                    "sensor".to_string(),
                    "bad-domain".to_string(),
                ],
            })
            .expect("save ha config");

        let secret = store
            .home_assistant_secret_state()
            .expect("load secret ha state");
        assert_eq!(secret.base_url, "http://ha.local:8123");
        assert_eq!(secret.access_token, "ha-token");
        assert_eq!(secret.exposed_domains, vec!["light", "sensor"]);

        let redacted = store
            .home_assistant_state()
            .expect("load redacted ha state");
        assert_eq!(
            redacted.access_token,
            super::home_assistant_token_redaction_marker()
        );

        store
            .save_home_assistant_config(HomeAssistantConfigUpdate {
                enabled: true,
                base_url: "http://ha-new.local:8123".to_string(),
                access_token: Some(String::new()),
                exposed_domains: vec!["camera".to_string()],
            })
            .expect("save redacted ha config");
        let preserved = store
            .home_assistant_secret_state()
            .expect("reload secret ha state");
        assert_eq!(preserved.base_url, "http://ha-new.local:8123");
        assert_eq!(preserved.access_token, "ha-token");
    }

    #[test]
    fn save_device_credential_projects_redacted_platform_record() {
        let registry_path = temp_path("registry-device-credential");
        let admin_path = temp_path("admin-device-credential");
        let registry = crate::runtime::registry::DeviceRegistryStore::new(registry_path.clone());
        let store = AdminConsoleStore::new(admin_path.clone(), registry);

        let state = store
            .save_device_credential(DeviceCredentialSecret {
                device_id: "cam-1".to_string(),
                username: "admin".to_string(),
                password: "secret".to_string(),
                rtsp_port: Some(8554),
                rtsp_paths: vec!["stream1".to_string(), "/stream1".to_string()],
                updated_at: Some("123".to_string()),
                last_verified_at: None,
            })
            .expect("save device credential");

        let credential = state
            .platform
            .credentials
            .iter()
            .find(|credential| {
                credential
                    .scope
                    .get("device_id")
                    .and_then(serde_json::Value::as_str)
                    == Some("cam-1")
            })
            .expect("platform credential projection");
        assert_eq!(credential.scope["username"], json!("admin"));
        assert_eq!(credential.scope["port"], json!(8554));
        assert_eq!(credential.metadata["redacted"], json!(true));
        assert_eq!(credential.metadata["path_count"], json!(1));
        assert!(!format!("{credential:?}").contains("secret"));

        let _ = std::fs::remove_file(admin_path);
        let _ = std::fs::remove_file(registry_path);
    }

    #[test]
    fn device_evidence_records_redact_stream_credentials_and_tokens() {
        let registry_path = temp_path("registry-device-evidence");
        let admin_path = temp_path("admin-device-evidence");
        let registry = crate::runtime::registry::DeviceRegistryStore::new(registry_path.clone());
        let store = AdminConsoleStore::new(admin_path.clone(), registry);

        store
            .record_device_evidence(DeviceEvidenceRecord {
                evidence_id: "evidence-1".to_string(),
                device_id: "cam-1".to_string(),
                evidence_kind: "rtsp_check".to_string(),
                status: "passed".to_string(),
                observed_at: "123".to_string(),
                summary: "ok rtsp://admin:secret@192.168.1.10/stream1?token=abc".to_string(),
                details: json!({
                    "stream_url": "rtsp://admin:secret@192.168.1.10/stream1",
                    "api_token": "raw-token",
                    "nested": {
                        "snapshot_url": "http://admin:secret@192.168.1.10/snap.jpg?token=abc"
                    }
                }),
            })
            .expect("record evidence");

        let evidence = store.list_device_evidence("cam-1").expect("list evidence");
        let payload = serde_json::to_string(&evidence).expect("serialize evidence");

        assert!(!payload.contains("admin:secret"));
        assert!(!payload.contains("raw-token"));
        assert!(!payload.contains("token=abc"));
        assert!(payload.contains("redacted:redacted@192.168.1.10"));
        assert!(payload.contains("token=redacted"));

        let _ = std::fs::remove_file(admin_path);
        let _ = std::fs::remove_file(registry_path);
    }

    #[test]
    fn forget_device_clears_admin_references() {
        let registry_path = temp_path("registry-forget-device");
        let admin_path = temp_path("admin-forget-device");
        let registry = crate::runtime::registry::DeviceRegistryStore::new(registry_path.clone());
        let store = AdminConsoleStore::new(admin_path.clone(), registry);

        store
            .save_defaults(AdminDefaults {
                selected_camera_device_id: Some("cam-1".to_string()),
                rtsp_username: "admin".to_string(),
                rtsp_password: "secret".to_string(),
                ..Default::default()
            })
            .expect("save defaults");
        store
            .save_dvr_recording_settings(DvrRecordingSettings {
                enabled_device_ids: vec!["cam-1".to_string(), "cam-2".to_string()],
                ..Default::default()
            })
            .expect("save dvr settings");
        store
            .save_device_credential(DeviceCredentialSecret {
                device_id: "cam-1".to_string(),
                username: "admin".to_string(),
                password: "secret".to_string(),
                rtsp_port: Some(554),
                rtsp_paths: vec!["/stream1".to_string()],
                updated_at: Some("123".to_string()),
                last_verified_at: Some("124".to_string()),
            })
            .expect("save credential");
        store
            .record_device_evidence(DeviceEvidenceRecord {
                evidence_id: "evidence-1".to_string(),
                device_id: "cam-1".to_string(),
                evidence_kind: "rtsp_check".to_string(),
                status: "passed".to_string(),
                observed_at: "125".to_string(),
                summary: "ok".to_string(),
                details: json!({}),
            })
            .expect("record evidence");

        let updated = store.forget_device("cam-1").expect("forget device");

        assert_eq!(updated.defaults.selected_camera_device_id, None);
        assert_eq!(updated.defaults.rtsp_username, "admin");
        assert!(updated.defaults.rtsp_password.is_empty());
        assert_eq!(updated.dvr.enabled_device_ids, vec!["cam-2"]);
        assert!(updated.device_credentials.is_empty());
        assert!(updated.device_evidence.is_empty());
        assert!(!updated
            .platform
            .credentials
            .iter()
            .any(|credential| credential.credential_id == device_rtsp_credential_id("cam-1")));
        assert!(updated
            .platform
            .recording_policies
            .iter()
            .all(|policy| policy.device_id.as_deref() != Some("cam-1")));

        let _ = std::fs::remove_file(admin_path);
        let _ = std::fs::remove_file(registry_path);
    }

    #[test]
    fn sanitize_defaults_treats_cidr_slash_port_as_rtsp_port_typo() {
        let sanitized = sanitize_defaults(AdminDefaults {
            cidr: "192.168.3.0/554".to_string(),
            rtsp_port: 554,
            ..Default::default()
        });

        assert_eq!(sanitized.cidr, "192.168.3.0/24");
        assert_eq!(sanitized.rtsp_port, 554);

        let sanitized = sanitize_defaults(AdminDefaults {
            cidr: "192.168.3.73/30".to_string(),
            ..Default::default()
        });
        assert_eq!(sanitized.cidr, "192.168.3.72/30");
    }

    #[test]
    fn derive_rtsp_hints_uses_first_authenticated_stream() {
        let device = CameraDevice::new(
            "cam-1",
            "Living Room",
            "rtsp://admin:MZBEHH@192.168.3.73:554/ch1/main",
        );
        let hints = derive_rtsp_hints(&[device]).expect("hints");
        assert_eq!(hints.username, "admin");
        assert_eq!(hints.password, "MZBEHH");
        assert_eq!(hints.paths, vec!["/ch1/main"]);
    }

    #[test]
    fn load_or_create_state_bootstraps_from_registry() {
        let registry_path = temp_path("registry");
        let admin_path = temp_path("admin");
        let registry = crate::runtime::registry::DeviceRegistryStore::new(registry_path.clone());
        let device = CameraDevice::new(
            "cam-1",
            "Living Room",
            "rtsp://admin:MZBEHH@192.168.3.73:554/ch1/main",
        );
        registry
            .save_devices(&[device])
            .expect("save device registry");

        let store = AdminConsoleStore::new(admin_path.clone(), registry);
        let state = store.load_or_create_state().expect("state");

        assert_eq!(state.defaults.rtsp_username, "admin");
        assert_eq!(state.defaults.rtsp_password, "MZBEHH");
        assert_eq!(state.platform.workspaces.len(), 1);
        assert_eq!(state.platform.users.len(), 1);
        assert_eq!(state.platform.memberships.len(), 1);
        assert!(!state.platform.permission_bindings.is_empty());
        assert_eq!(state.platform.provider_accounts.len(), 1);
        assert_eq!(state.platform.credentials.len(), 1);
        assert_eq!(state.platform.recording_policies.len(), 1);
        assert!(store.path().exists());

        let _ = std::fs::remove_file(admin_path);
        let _ = std::fs::remove_file(registry_path);
    }

    #[test]
    fn normalize_binding_code_accepts_token_or_compact_code() {
        assert_eq!(
            normalize_binding_code("hub://bind/feishu/5b86-a98f"),
            Some("5B86-A98F".to_string())
        );
        assert_eq!(
            normalize_binding_code("hub://bind/im_bridge/5b86-a98f"),
            Some("5B86-A98F".to_string())
        );
        assert_eq!(
            normalize_binding_code("5b86a98f"),
            Some("5B86-A98F".to_string())
        );
    }

    #[test]
    fn bind_identity_user_persists_mapping() {
        let registry_path = temp_path("registry-bind");
        let admin_path = temp_path("admin-bind");
        let registry = crate::runtime::registry::DeviceRegistryStore::new(registry_path.clone());
        let store = AdminConsoleStore::new(admin_path.clone(), registry);
        let state = store.load_or_create_state().expect("state");

        let updated = store
            .bind_identity_user(
                &state.binding.qr_token,
                IdentityBindingRecord {
                    open_id: "ou_demo".to_string(),
                    user_id: Some("u_demo".to_string()),
                    union_id: Some("on_demo".to_string()),
                    display_name: "Bean".to_string(),
                    chat_id: Some("oc_demo".to_string()),
                },
            )
            .expect("bind");

        assert_eq!(updated.binding.bound_user.as_deref(), Some("Bean"));
        assert_eq!(updated.identity_bindings.len(), 1);
        assert_eq!(updated.identity_bindings[0].open_id, "ou_demo");
        assert_eq!(updated.platform.users.len(), 2);
        assert_eq!(updated.platform.memberships.len(), 2);
        assert_eq!(updated.platform.identity_bindings.len(), 1);
        assert_eq!(
            updated.platform.identity_bindings[0].external_user_id,
            "ou_demo"
        );

        let _ = std::fs::remove_file(admin_path);
        let _ = std::fs::remove_file(registry_path);
    }

    #[test]
    fn save_defaults_returns_updated_platform_projection() {
        let registry_path = temp_path("registry-defaults");
        let admin_path = temp_path("admin-defaults");
        let registry = crate::runtime::registry::DeviceRegistryStore::new(registry_path.clone());
        let store = AdminConsoleStore::new(admin_path.clone(), registry);

        let updated = store
            .save_defaults(AdminDefaults {
                cidr: "10.42.0.0/24".to_string(),
                discovery: "ONVIF".to_string(),
                recording: "持续录制".to_string(),
                capture: "仅图片".to_string(),
                ai: "快速摘要".to_string(),
                notification_channel: "平台频道".to_string(),
                rtsp_username: "platform-user".to_string(),
                rtsp_password: "secret-rtsp".to_string(),
                rtsp_port: 8554,
                rtsp_paths: vec!["/alt/main".to_string()],
                selected_camera_device_id: Some("camera-selected".to_string()),
                capture_subdirectory: "release-v1".to_string(),
                clip_length_seconds: 12,
                keyframe_count: 5,
                keyframe_interval_seconds: 2,
            })
            .expect("save defaults");

        assert_eq!(updated.platform.workspaces.len(), 1);
        assert_eq!(
            updated.platform.workspaces[0].settings["defaults"]["cidr"],
            json!("10.42.0.0/24")
        );
        assert!(updated.platform.provider_accounts.iter().any(|provider| {
            provider.provider_account_id == LOCAL_RTSP_PROVIDER_ACCOUNT_ID
                && provider.capabilities["rtsp_port"] == json!(8554)
        }));
        assert!(updated.platform.credentials.iter().any(|credential| {
            credential.credential_id == LOCAL_RTSP_CREDENTIAL_ID
                && credential.vault_key == "admin_console.defaults.rtsp_password"
        }));
        assert_eq!(
            updated.platform.recording_policies[0].trigger_mode,
            RecordingTriggerMode::Continuous
        );

        let reloaded = store.load_or_create_state().expect("reload");
        assert_eq!(reloaded.defaults.cidr, "10.42.0.0/24");
        assert_eq!(reloaded.defaults.rtsp_port, 8554);

        let _ = std::fs::remove_file(admin_path);
        let _ = std::fs::remove_file(registry_path);
    }

    #[test]
    fn platform_sync_dedupes_projected_credentials() {
        let mut state = AdminConsoleState::default();
        state.defaults.rtsp_password = "secret-rtsp".to_string();
        state.platform.credentials = vec![
            CredentialRecord {
                credential_id: LOCAL_RTSP_CREDENTIAL_ID.to_string(),
                provider_account_id: LOCAL_RTSP_PROVIDER_ACCOUNT_ID.to_string(),
                credential_kind: CredentialKind::SessionSecret,
                vault_key: "stale".to_string(),
                scope: json!({}),
                expires_at: None,
                rotation_state: CredentialRotationState::Valid,
                last_verified_at: None,
                metadata: json!({"stale": true}),
            },
            CredentialRecord {
                credential_id: LOCAL_RTSP_CREDENTIAL_ID.to_string(),
                provider_account_id: LOCAL_RTSP_PROVIDER_ACCOUNT_ID.to_string(),
                credential_kind: CredentialKind::SessionSecret,
                vault_key: "stale-duplicate".to_string(),
                scope: json!({}),
                expires_at: None,
                rotation_state: CredentialRotationState::Valid,
                last_verified_at: None,
                metadata: json!({"stale": true}),
            },
        ];

        let platform = sync_platform_from_legacy(&state);
        let matching = platform
            .credentials
            .iter()
            .filter(|credential| credential.credential_id == LOCAL_RTSP_CREDENTIAL_ID)
            .collect::<Vec<_>>();

        assert_eq!(matching.len(), 1);
        assert_eq!(
            matching[0].vault_key,
            "admin_console.defaults.rtsp_password"
        );
    }

    #[test]
    fn save_knowledge_settings_rejects_index_root_inside_source_root() {
        let registry_path = temp_path("registry-knowledge-invalid");
        let admin_path = temp_path("admin-knowledge-invalid");
        let registry = crate::runtime::registry::DeviceRegistryStore::new(registry_path.clone());
        let store = AdminConsoleStore::new(admin_path.clone(), registry);
        let source_root = std::env::temp_dir().join("harborbeacon-knowledge-source");
        let index_root = source_root.join("vectors");

        let result = store.save_knowledge_settings(KnowledgeSettings {
            source_roots: vec![KnowledgeSourceRoot {
                root_id: "sample".to_string(),
                label: "Sample".to_string(),
                path: source_root.to_string_lossy().into_owned(),
                enabled: true,
                include: Vec::new(),
                exclude: Vec::new(),
                last_indexed_at: None,
            }],
            index_root: index_root.to_string_lossy().into_owned(),
            ..Default::default()
        });

        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .contains("knowledge.index_root 不能位于 source_root 内"));

        let _ = std::fs::remove_file(admin_path);
        let _ = std::fs::remove_file(registry_path);
    }

    #[test]
    fn save_knowledge_settings_persists_sanitized_roots() {
        let registry_path = temp_path("registry-knowledge-valid");
        let admin_path = temp_path("admin-knowledge-valid");
        let registry = crate::runtime::registry::DeviceRegistryStore::new(registry_path.clone());
        let store = AdminConsoleStore::new(admin_path.clone(), registry);
        let source_root = std::env::temp_dir().join("harborbeacon-knowledge-source-valid");
        let index_root = std::env::temp_dir().join("harborbeacon-knowledge-index-valid");

        let updated = store
            .save_knowledge_settings(KnowledgeSettings {
                source_roots: vec![KnowledgeSourceRoot {
                    root_id: "  ".to_string(),
                    label: "  Family Docs  ".to_string(),
                    path: format!(" {} ", source_root.to_string_lossy()),
                    enabled: true,
                    include: vec![" **/*.md ".to_string(), "**/*.md".to_string()],
                    exclude: vec![" tmp/** ".to_string()],
                    last_indexed_at: Some("  ".to_string()),
                }],
                index_root: index_root.to_string_lossy().into_owned(),
                ..Default::default()
            })
            .expect("save knowledge settings");

        let user_roots = updated
            .knowledge
            .source_roots
            .iter()
            .filter(|root| root.root_id != "camera-dvr-recordings")
            .collect::<Vec<_>>();
        assert_eq!(user_roots.len(), 1);
        assert_eq!(user_roots[0].label, "Family Docs");
        assert_eq!(user_roots[0].root_id, "knowledge-familydocs");
        assert_eq!(user_roots[0].include, vec!["**/*.md"]);
        assert_eq!(user_roots[0].exclude, vec!["tmp/**"]);
        assert_eq!(user_roots[0].last_indexed_at, None);

        let reloaded = store
            .knowledge_settings()
            .expect("reload knowledge settings");
        assert_eq!(reloaded.source_roots[0].label, "Family Docs");

        let _ = std::fs::remove_file(admin_path);
        let _ = std::fs::remove_file(registry_path);
    }

    #[test]
    fn save_dvr_recording_settings_upserts_video_knowledge_root() {
        let registry_path = temp_path("registry-dvr-settings");
        let admin_path = temp_path("admin-dvr-settings");
        let registry = crate::runtime::registry::DeviceRegistryStore::new(registry_path.clone());
        let store = AdminConsoleStore::new(admin_path.clone(), registry);
        let recording_root = std::env::temp_dir().join("harborbeacon-dvr-recordings-valid");
        let media_library_root = recording_root.join("library");

        let updated = store
            .save_dvr_recording_settings(DvrRecordingSettings {
                recording_root: recording_root.to_string_lossy().into_owned(),
                retention_days: 14,
                segment_seconds: 600,
                continuous_bitrate_mbps: 2,
                enabled_device_ids: vec!["camera-main".to_string()],
                ..Default::default()
            })
            .expect("save dvr settings");

        assert_eq!(updated.dvr.retention_days, 14);
        assert_eq!(updated.dvr.segment_seconds, 600);
        assert!(updated.knowledge.source_roots.iter().any(|root| {
            root.root_id == "camera-dvr-recordings"
                && root.path == media_library_root.to_string_lossy()
                && root.enabled
                && root.include.iter().any(|pattern| pattern == "**/*.mp4")
        }));
        assert_eq!(
            updated.platform.workspaces[0].settings["dvr"]["recording_root"],
            json!(recording_root.to_string_lossy())
        );
        assert_eq!(
            updated.platform.workspaces[0].settings["dvr"]["media_library_root"],
            json!(media_library_root.to_string_lossy())
        );

        let reloaded = store.dvr_recording_settings().expect("reload dvr");
        assert_eq!(reloaded.retention_days, 14);
        assert_eq!(reloaded.enabled_device_ids, vec!["camera-main"]);

        let _ = std::fs::remove_dir_all(recording_root);
        let _ = std::fs::remove_file(admin_path);
        let _ = std::fs::remove_file(registry_path);
    }

    #[test]
    fn save_remote_view_config_returns_updated_platform_projection() {
        let registry_path = temp_path("registry-remote-view");
        let admin_path = temp_path("admin-remote-view");
        let registry = crate::runtime::registry::DeviceRegistryStore::new(registry_path.clone());
        let store = AdminConsoleStore::new(admin_path.clone(), registry);

        let updated = store
            .save_remote_view_config(RemoteViewConfig {
                share_secret: "platform-share-secret".to_string(),
                share_link_ttl_minutes: 45,
            })
            .expect("save remote view");

        assert_eq!(updated.remote_view.share_secret, "platform-share-secret");
        assert_eq!(updated.remote_view.share_link_ttl_minutes, 45);
        assert_eq!(
            updated.platform.workspaces[0].settings["remote_view"]["share_secret"],
            json!("platform-share-secret")
        );
        assert_eq!(
            updated.platform.workspaces[0].settings["remote_view"]["share_link_ttl_minutes"],
            json!(45)
        );

        let reloaded = store.load_or_create_state().expect("reload");
        assert_eq!(reloaded.remote_view.share_secret, "platform-share-secret");
        assert_eq!(reloaded.remote_view.share_link_ttl_minutes, 45);
        assert_eq!(
            store
                .load_remote_view_config()
                .expect("resolved remote view"),
            RemoteViewConfig {
                share_secret: "platform-share-secret".to_string(),
                share_link_ttl_minutes: 45,
            }
        );

        let _ = std::fs::remove_file(admin_path);
        let _ = std::fs::remove_file(registry_path);
    }

    #[test]
    fn set_member_role_persists_custom_role() {
        let registry_path = temp_path("registry-role");
        let admin_path = temp_path("admin-role");
        let registry = crate::runtime::registry::DeviceRegistryStore::new(registry_path.clone());
        let store = AdminConsoleStore::new(admin_path.clone(), registry);
        let state = store.load_or_create_state().expect("state");

        store
            .bind_identity_user(
                &state.binding.qr_token,
                IdentityBindingRecord {
                    open_id: "ou_operator".to_string(),
                    user_id: Some("user-operator".to_string()),
                    union_id: None,
                    display_name: "Operator".to_string(),
                    chat_id: None,
                },
            )
            .expect("bind");

        let updated = store
            .set_member_role("user-operator", RoleKind::Operator)
            .expect("set role");

        assert!(updated.platform.memberships.iter().any(|membership| {
            membership.user_id == "user-operator" && membership.role_kind == RoleKind::Operator
        }));

        let reloaded = store.load_or_create_state().expect("reload");
        assert!(reloaded.platform.memberships.iter().any(|membership| {
            membership.user_id == "user-operator" && membership.role_kind == RoleKind::Operator
        }));

        let _ = std::fs::remove_file(admin_path);
        let _ = std::fs::remove_file(registry_path);
    }

    #[test]
    fn set_member_default_delivery_surface_persists_member_preference() {
        let registry_path = temp_path("registry-delivery-surface");
        let admin_path = temp_path("admin-delivery-surface");
        let registry = crate::runtime::registry::DeviceRegistryStore::new(registry_path.clone());
        let store = AdminConsoleStore::new(admin_path.clone(), registry);
        let state = store.load_or_create_state().expect("state");

        store
            .bind_identity_user(
                &state.binding.qr_token,
                IdentityBindingRecord {
                    open_id: "ou_weixin_member".to_string(),
                    user_id: Some("user-weixin".to_string()),
                    union_id: None,
                    display_name: "Weixin Member".to_string(),
                    chat_id: Some("oc_weixin_member".to_string()),
                },
            )
            .expect("bind");

        let updated = store
            .set_member_default_delivery_surface("user-weixin", "weixin")
            .expect("set default delivery surface");
        let user = updated
            .platform
            .users
            .iter()
            .find(|user| user.user_id == "user-weixin")
            .expect("user");
        assert_eq!(
            user_default_delivery_surface(user).as_deref(),
            Some("weixin")
        );

        let reloaded = store.load_or_create_state().expect("reload");
        let user = reloaded
            .platform
            .users
            .iter()
            .find(|user| user.user_id == "user-weixin")
            .expect("user");
        assert_eq!(
            user_default_delivery_surface(user).as_deref(),
            Some("weixin")
        );

        let _ = std::fs::remove_file(admin_path);
        let _ = std::fs::remove_file(registry_path);
    }

    #[test]
    fn notification_targets_persist_with_single_default() {
        let registry_path = temp_path("registry-notification-targets");
        let admin_path = temp_path("admin-notification-targets");
        let registry = crate::runtime::registry::DeviceRegistryStore::new(registry_path.clone());
        let store = AdminConsoleStore::new(admin_path.clone(), registry);

        let updated = store
            .upsert_notification_target(None, "我的微信", "gw_route_weixin", "weixin", true)
            .expect("save target");
        assert_eq!(updated.notification_targets.len(), 1);
        assert!(updated.notification_targets[0].is_default);
        assert_eq!(updated.notification_targets[0].platform_hint, "weixin");

        let updated = store
            .upsert_notification_target(None, "值班飞书", "gw_route_feishu", "feishu", false)
            .expect("save second target");
        assert_eq!(updated.notification_targets.len(), 2);
        assert_eq!(
            updated
                .notification_targets
                .iter()
                .filter(|target| target.is_default)
                .count(),
            1
        );
        assert_eq!(
            updated
                .notification_targets
                .iter()
                .find(|target| target.is_default)
                .map(|target| target.route_key.as_str()),
            Some("gw_route_weixin")
        );

        let target_id = updated
            .notification_targets
            .iter()
            .find(|target| target.route_key == "gw_route_feishu")
            .map(|target| target.target_id.clone())
            .expect("feishu target");
        let updated = store
            .set_default_notification_target(&target_id)
            .expect("set default target");
        assert_eq!(
            updated
                .notification_targets
                .iter()
                .find(|target| target.is_default)
                .map(|target| target.route_key.as_str()),
            Some("gw_route_feishu")
        );

        let _ = std::fs::remove_file(admin_path);
        let _ = std::fs::remove_file(registry_path);
    }

    #[test]
    fn automation_reviews_persist_and_update_status() {
        let registry_path = temp_path("registry-automation-reviews");
        let admin_path = temp_path("admin-automation-reviews");
        let registry = crate::runtime::registry::DeviceRegistryStore::new(registry_path.clone());
        let store = AdminConsoleStore::new(admin_path.clone(), registry);

        let updated = store
            .upsert_automation_review(AutomationRuleReview {
                review_id: String::new(),
                workspace_id: String::new(),
                source: String::new(),
                source_channel: Some("k3-webui-smoke".to_string()),
                source_conversation_id: None,
                original_prompt: "当门口检测到有人时，生成家庭通知草稿。".to_string(),
                status: String::new(),
                trigger_definition: Some(json!({"event_type": "person_detected"})),
                condition_definition: None,
                action_plan: Some(json!({"action": "draft_notification"})),
                device_refs: vec![json!({"camera_id": "camera.front_door"})],
                risk_level: Some("low".to_string()),
                requires_approval: true,
                created_at: None,
                updated_at: None,
                expires_at: None,
                rule_id: None,
                run_summaries: Vec::new(),
                metadata: Some(json!({"smoke": "k3-assistant-parity"})),
            })
            .expect("save review");
        assert_eq!(updated.automation_reviews.len(), 1);
        let review_id = updated.automation_reviews[0].review_id.clone();
        assert!(review_id.starts_with("review-"));
        assert_eq!(updated.automation_reviews[0].status, "draft");

        let updated = store
            .set_automation_review_status(&review_id, "discarded")
            .expect("discard review");
        assert_eq!(updated.automation_reviews[0].status, "discarded");

        let reloaded = store.load_or_create_state().expect("reload");
        assert_eq!(reloaded.automation_reviews[0].status, "discarded");

        let _ = std::fs::remove_file(admin_path);
        let _ = std::fs::remove_file(registry_path);
    }

    #[test]
    fn automation_reviews_reject_sensitive_payloads() {
        let registry_path = temp_path("registry-automation-review-secret");
        let admin_path = temp_path("admin-automation-review-secret");
        let registry = crate::runtime::registry::DeviceRegistryStore::new(registry_path.clone());
        let store = AdminConsoleStore::new(admin_path.clone(), registry);

        let error = store
            .upsert_automation_review(AutomationRuleReview {
                review_id: "review-secret".to_string(),
                workspace_id: String::new(),
                source: String::new(),
                source_channel: None,
                source_conversation_id: None,
                original_prompt: "请把 rtsp://user:pass@example.local/stream 发到群里".to_string(),
                status: "draft".to_string(),
                trigger_definition: None,
                condition_definition: None,
                action_plan: None,
                device_refs: Vec::new(),
                risk_level: None,
                requires_approval: false,
                created_at: None,
                updated_at: None,
                expires_at: None,
                rule_id: None,
                run_summaries: Vec::new(),
                metadata: None,
            })
            .expect_err("sensitive review should be rejected");
        assert!(error.contains("敏感字段"));

        let _ = std::fs::remove_file(admin_path);
        let _ = std::fs::remove_file(registry_path);
    }

    #[test]
    fn load_state_reads_without_rewriting_admin_file() {
        let registry_path = temp_path("registry-readonly-load");
        let admin_path = temp_path("admin-readonly-load");
        let registry = crate::runtime::registry::DeviceRegistryStore::new(registry_path.clone());
        let store = AdminConsoleStore::new(admin_path.clone(), registry);

        store.load_or_create_state().expect("bootstrap state");
        let before = std::fs::read_to_string(&admin_path).expect("read before");

        store.load_state().expect("read-only load");

        let after = std::fs::read_to_string(&admin_path).expect("read after");
        assert_eq!(after, before);

        let _ = std::fs::remove_file(admin_path);
        let _ = std::fs::remove_file(registry_path);
    }

    #[test]
    fn record_member_interactive_surface_persists_recent_surface() {
        let registry_path = temp_path("registry-recent-surface");
        let admin_path = temp_path("admin-recent-surface");
        let registry = crate::runtime::registry::DeviceRegistryStore::new(registry_path.clone());
        let store = AdminConsoleStore::new(admin_path.clone(), registry);
        let state = store.load_or_create_state().expect("state");

        store
            .bind_identity_user(
                &state.binding.qr_token,
                IdentityBindingRecord {
                    open_id: "ou_recent_member".to_string(),
                    user_id: Some("user-recent".to_string()),
                    union_id: None,
                    display_name: "Recent Member".to_string(),
                    chat_id: Some("oc_recent_member".to_string()),
                },
            )
            .expect("bind");
        store
            .record_member_interactive_surface("user-recent", "weixin", Some("gw_route_recent"))
            .expect("record surface");

        let reloaded = store.load_or_create_state().expect("reload");
        let user = reloaded
            .platform
            .users
            .iter()
            .find(|user| user.user_id == "user-recent")
            .expect("user");
        assert_eq!(
            user_recent_interactive_surface(user).as_deref(),
            Some("weixin")
        );
        assert_eq!(
            user.preferences["delivery"]["recent_interactive_route_key"],
            json!("gw_route_recent")
        );

        let _ = std::fs::remove_file(admin_path);
        let _ = std::fs::remove_file(registry_path);
    }

    #[test]
    fn record_model_endpoint_test_result_persists_health_and_last_test() {
        let registry_path = temp_path("registry-model-test");
        let admin_path = temp_path("admin-model-test");
        let registry = crate::runtime::registry::DeviceRegistryStore::new(registry_path.clone());
        let store = AdminConsoleStore::new(admin_path.clone(), registry);

        store
            .save_model_endpoint(ModelEndpoint {
                model_endpoint_id: "vlm-test".to_string(),
                workspace_id: Some("home-1".to_string()),
                provider_account_id: None,
                model_kind: ModelKind::Vlm,
                endpoint_kind: ModelEndpointKind::Local,
                provider_key: "openai_compatible".to_string(),
                model_name: "vision".to_string(),
                capability_tags: vec!["multimodal".to_string()],
                cost_policy: json!({}),
                status: ModelEndpointStatus::Active,
                metadata: json!({}),
            })
            .expect("save model endpoint");

        let updated = store
            .record_model_endpoint_test_result(
                "vlm-test",
                false,
                "degraded",
                "HTTP probe failed",
                json!({"http_status": 502}),
            )
            .expect("record test result");

        let endpoint = updated
            .models
            .endpoints
            .iter()
            .find(|endpoint| endpoint.model_endpoint_id == "vlm-test")
            .expect("endpoint");
        assert_eq!(endpoint.status, ModelEndpointStatus::Degraded);
        assert_eq!(endpoint.metadata["health_status"], json!("degraded"));
        assert_eq!(endpoint.metadata["last_test"]["ok"], json!(false));
        assert_eq!(
            endpoint.metadata["last_test"]["summary"],
            json!("HTTP probe failed")
        );

        let reloaded = store.load_or_create_state().expect("reload");
        let endpoint = reloaded
            .models
            .endpoints
            .iter()
            .find(|endpoint| endpoint.model_endpoint_id == "vlm-test")
            .expect("endpoint");
        assert_eq!(endpoint.status, ModelEndpointStatus::Degraded);
        assert_eq!(
            endpoint.metadata["last_test"]["details"]["http_status"],
            json!(502)
        );

        let _ = std::fs::remove_file(admin_path);
        let _ = std::fs::remove_file(registry_path);
    }

    #[test]
    fn default_model_center_includes_siliconflow_cloud_fallback_preset() {
        let endpoints = default_model_endpoints();
        let endpoint = endpoints
            .iter()
            .find(|endpoint| endpoint.model_endpoint_id == "llm-cloud-siliconflow")
            .expect("siliconflow endpoint");
        assert_eq!(endpoint.endpoint_kind, ModelEndpointKind::Cloud);
        assert_eq!(endpoint.provider_key, "openai_compatible");
        assert_eq!(endpoint.status, ModelEndpointStatus::Disabled);
        assert_eq!(
            endpoint.metadata["base_url"],
            json!("https://api.siliconflow.cn/v1")
        );
        assert_eq!(endpoint.metadata["api_key_configured"], json!(false));

        let policies = default_model_route_policies();
        let router_policy = policies
            .iter()
            .find(|policy| policy.route_policy_id == "semantic.router")
            .expect("semantic router policy");
        assert_eq!(
            router_policy.privacy_level,
            crate::control_plane::models::PrivacyLevel::StrictLocal
        );
        assert!(!router_policy
            .fallback_order
            .iter()
            .any(|kind| kind == "cloud"));

        let vlm_policy = policies
            .iter()
            .find(|policy| policy.route_policy_id == "retrieval.vision_summary")
            .expect("vlm policy");
        assert!(!vlm_policy.fallback_order.iter().any(|kind| kind == "cloud"));

        let runtimes = default_model_runtimes();
        let candle = runtimes
            .iter()
            .find(|runtime| runtime.runtime_id == "harbor-candle")
            .expect("harbor candle runtime");
        assert!(candle.managed);
        assert!(candle.installable);
        assert!(candle.enabled);
        assert_eq!(candle.status, "idle");
        assert_eq!(
            candle.metadata["bootstrap_model_id"],
            json!("Qwen/Qwen2.5-0.5B-Instruct")
        );
        assert!(candle
            .runtime_profiles
            .iter()
            .any(|profile| profile == "harbor-candle"));
        assert!(candle
            .model_store_path
            .replace('\\', "/")
            .ends_with("runtimes/harbor-candle"));
    }

    #[test]
    fn install_model_runtime_enables_harbor_managed_candle() {
        let registry_path = temp_path("registry-model-runtime-install");
        let admin_path = temp_path("admin-model-runtime-install");
        let registry = crate::runtime::registry::DeviceRegistryStore::new(registry_path.clone());
        let store = AdminConsoleStore::new(admin_path.clone(), registry);

        let installed = store
            .install_model_runtime("harbor-candle")
            .expect("install runtime");
        assert_eq!(installed.runtime_id, "harbor-candle");
        assert!(installed.enabled);
        assert_eq!(installed.status, "installed");

        let state = store.load_or_create_state().expect("reload state");
        let candle = state
            .models
            .runtimes
            .iter()
            .find(|runtime| runtime.runtime_id == "harbor-candle")
            .expect("persisted candle runtime");
        assert!(candle.enabled);
        assert_eq!(candle.status, "installed");

        let _ = std::fs::remove_file(admin_path);
        let _ = std::fs::remove_file(registry_path);
    }

    #[test]
    fn sanitize_model_center_migrates_legacy_builtin_model_api_urls() {
        let state = AdminModelCenterState {
            endpoints: vec![ModelEndpoint {
                model_endpoint_id: "embed-local-openai-compatible".to_string(),
                workspace_id: Some("home-1".to_string()),
                provider_account_id: None,
                model_kind: ModelKind::Embedder,
                endpoint_kind: ModelEndpointKind::Local,
                provider_key: "openai_compatible".to_string(),
                model_name: "Qwen/Qwen3-Embedding-0.6B".to_string(),
                capability_tags: vec!["embeddings".to_string(), "local_first".to_string()],
                cost_policy: json!({}),
                status: ModelEndpointStatus::Active,
                metadata: json!({
                    "builtin": true,
                    "base_url": "http://127.0.0.1:4176/v1",
                    "healthz_url": "http://127.0.0.1:4176/healthz",
                    "api_key": "legacy-token",
                    "api_key_configured": true,
                }),
            }],
            route_policies: default_model_route_policies(),
            model_store_root: default_model_store_root(),
            capability_bindings: Vec::new(),
            runtimes: Vec::new(),
        };

        let sanitized = sanitize_model_center_state(state);
        let endpoint = sanitized
            .endpoints
            .iter()
            .find(|endpoint| endpoint.model_endpoint_id == "embed-local-openai-compatible")
            .expect("embed endpoint");

        assert_eq!(
            endpoint.metadata["base_url"],
            json!("http://127.0.0.1:4174/api/inference/v1")
        );
        assert_eq!(
            endpoint.metadata["healthz_url"],
            json!("http://127.0.0.1:4174/api/inference/healthz")
        );
        assert_eq!(endpoint.metadata["legacy_model_api_migrated"], json!(true));
    }

    #[test]
    fn sanitize_model_center_keeps_semantic_router_local_only() {
        let mut state = AdminModelCenterState {
            endpoints: default_model_endpoints(),
            route_policies: default_model_route_policies(),
            model_store_root: default_model_store_root(),
            capability_bindings: Vec::new(),
            runtimes: Vec::new(),
        };
        let router = state
            .route_policies
            .iter_mut()
            .find(|policy| policy.route_policy_id == "semantic.router")
            .expect("router policy");
        router.privacy_level = PrivacyLevel::AllowRedactedCloud;
        router.fallback_order = vec![
            "local".to_string(),
            "sidecar".to_string(),
            "cloud".to_string(),
        ];
        router.metadata = json!({
            "capability": "router",
            "cloud_fallback_scope": "semantic_router_only",
            "redaction_required": true,
        });

        let sanitized = sanitize_model_center_state(state);
        let router = sanitized
            .route_policies
            .iter()
            .find(|policy| policy.route_policy_id == "semantic.router")
            .expect("sanitized router policy");
        assert_eq!(router.privacy_level, PrivacyLevel::StrictLocal);
        assert!(!router
            .fallback_order
            .iter()
            .any(|kind| kind.eq_ignore_ascii_case("cloud")));
        assert_eq!(router.metadata["local_only"], json!(true));
        assert_eq!(router.metadata["cloud_fallback_allowed"], json!(false));
        assert!(router.metadata.get("cloud_fallback_scope").is_none());
    }

    #[test]
    fn save_model_endpoint_preserves_existing_secret_when_api_key_is_blank() {
        let registry_path = temp_path("registry-model-secret");
        let admin_path = temp_path("admin-model-secret");
        let registry = crate::runtime::registry::DeviceRegistryStore::new(registry_path.clone());
        let store = AdminConsoleStore::new(admin_path.clone(), registry);

        let mut endpoint = default_model_endpoints()
            .into_iter()
            .find(|endpoint| endpoint.model_endpoint_id == "llm-cloud-siliconflow")
            .expect("siliconflow endpoint");
        endpoint.status = ModelEndpointStatus::Active;
        endpoint.metadata["api_key"] = json!("sk-secret");
        endpoint.metadata["api_key_configured"] = json!(true);
        store
            .save_model_endpoint(endpoint)
            .expect("save configured endpoint");

        let mut redacted_payload = default_model_endpoints()
            .into_iter()
            .find(|endpoint| endpoint.model_endpoint_id == "llm-cloud-siliconflow")
            .expect("siliconflow endpoint");
        redacted_payload.status = ModelEndpointStatus::Active;
        redacted_payload.metadata["api_key"] = json!("");
        redacted_payload.metadata["api_key_configured"] = json!(true);
        redacted_payload.metadata["model"] = json!("deepseek-ai/DeepSeek-V4-Flash");
        let updated = store
            .save_model_endpoint(redacted_payload)
            .expect("save redacted payload");
        let endpoint = updated
            .models
            .endpoints
            .iter()
            .find(|endpoint| endpoint.model_endpoint_id == "llm-cloud-siliconflow")
            .expect("saved endpoint");
        assert_eq!(endpoint.metadata["api_key"], json!("sk-secret"));
        assert_eq!(endpoint.metadata["api_key_configured"], json!(true));

        let patched = store
            .patch_model_endpoint(
                "llm-cloud-siliconflow",
                json!({
                    "metadata": {
                        "api_key": "",
                        "model": "deepseek-ai/DeepSeek-V4-Flash",
                    }
                }),
            )
            .expect("patch redacted payload");
        let endpoint = patched
            .models
            .endpoints
            .iter()
            .find(|endpoint| endpoint.model_endpoint_id == "llm-cloud-siliconflow")
            .expect("patched endpoint");
        assert_eq!(endpoint.metadata["api_key"], json!("sk-secret"));

        let _ = std::fs::remove_file(admin_path);
        let _ = std::fs::remove_file(registry_path);
    }

    #[test]
    fn save_bridge_provider_status_returns_updated_platform_projection() {
        let registry_path = temp_path("registry-bridge");
        let admin_path = temp_path("admin-bridge");
        let registry = crate::runtime::registry::DeviceRegistryStore::new(registry_path.clone());
        let store = AdminConsoleStore::new(admin_path.clone(), registry);

        let updated = store
            .save_bridge_provider_status(BridgeProviderConfig {
                configured: true,
                connected: true,
                platform: "feishu".to_string(),
                gateway_base_url: "http://gateway.local:4180".to_string(),
                app_name: "HarborBeacon Bot".to_string(),
                status: "已连接".to_string(),
                last_checked_at: "2026-04-18T10:00:00Z".to_string(),
                capabilities: BridgeProviderCapabilities {
                    reply: true,
                    update: true,
                    attachments: true,
                },
                ..Default::default()
            })
            .expect("save bridge provider status");

        assert_eq!(updated.binding.metric, "Gateway 在线");
        assert_eq!(
            updated.binding.bound_user.as_deref(),
            Some("HarborBeacon Bot")
        );
        assert!(updated.platform.provider_accounts.iter().any(|provider| {
            provider.provider_account_id == BRIDGE_PROVIDER_ACCOUNT_ID
                && provider.metadata["platform"] == json!("feishu")
                && provider.metadata["display_name"] == json!("HarborBeacon Bot")
        }));
        assert!(updated.platform.credentials.is_empty());

        let reloaded = store.load_or_create_state().expect("reload");
        assert_eq!(reloaded.bridge_provider.app_name, "HarborBeacon Bot");
        assert_eq!(
            reloaded.bridge_provider.gateway_base_url,
            "http://gateway.local:4180"
        );
        assert_eq!(reloaded.bridge_provider.app_secret, "");
        assert_eq!(reloaded.bridge_provider.bot_open_id, "");

        let _ = std::fs::remove_file(admin_path);
        let _ = std::fs::remove_file(registry_path);
    }

    #[test]
    fn account_management_snapshot_surfaces_workspace_member_identity_and_governance_views() {
        let registry_path = temp_path("registry-account");
        let admin_path = temp_path("admin-account");
        let registry = crate::runtime::registry::DeviceRegistryStore::new(registry_path.clone());
        let store = AdminConsoleStore::new(admin_path.clone(), registry);
        let mut state = store.load_or_create_state().expect("state");

        state.platform.memberships.push(Membership {
            membership_id: "membership-u_demo".to_string(),
            workspace_id: "home-1".to_string(),
            user_id: "u_demo".to_string(),
            role_kind: RoleKind::Viewer,
            status: MembershipStatus::Active,
            granted_by_user_id: Some("local-owner".to_string()),
            granted_at: None,
        });
        state.platform.users.push(UserAccount {
            user_id: "u_demo".to_string(),
            display_name: "Bean".to_string(),
            email: None,
            phone: None,
            status: UserStatus::Active,
            default_workspace_id: Some("home-1".to_string()),
            preferences: json!({}),
        });
        state.platform.identity_bindings.push(IdentityBinding {
            identity_id: "identity-ou_demo".to_string(),
            user_id: "u_demo".to_string(),
            auth_source: AuthSource::ImChannel,
            provider_key: "im_bridge".to_string(),
            external_user_id: "ou_demo".to_string(),
            external_union_id: Some("on_demo".to_string()),
            external_chat_id: Some("oc_demo".to_string()),
            profile_snapshot: json!({
                "display_name": "Bean",
            }),
            last_seen_at: None,
        });
        state.bridge_provider = BridgeProviderConfig {
            configured: true,
            connected: true,
            platform: "feishu".to_string(),
            gateway_base_url: "http://gateway.local:4180".to_string(),
            app_name: "HarborBeacon Bot".to_string(),
            status: "已连接".to_string(),
            last_checked_at: "2026-04-18T10:00:00Z".to_string(),
            capabilities: BridgeProviderCapabilities {
                reply: true,
                update: true,
                attachments: true,
            },
            ..Default::default()
        };

        let snapshot = account_management_snapshot(&state, Some("http://harborbeacon.local:4174"));

        assert_eq!(snapshot.workspace.workspace_id, "home-1");
        assert_eq!(snapshot.workspace.member_count, 2);
        assert_eq!(snapshot.workspace.identity_binding_count, 1);
        assert_eq!(snapshot.member_role_counts.len(), 6);
        assert!(snapshot
            .member_role_counts
            .iter()
            .any(|summary| summary.role_kind == "owner" && summary.member_count == 1));
        assert!(snapshot
            .member_role_counts
            .iter()
            .any(|summary| summary.role_kind == "viewer" && summary.member_count == 1));
        assert_eq!(snapshot.identity_bindings.len(), 1);
        assert_eq!(snapshot.identity_bindings[0].open_id, "ou_demo");
        assert_eq!(snapshot.identity_bindings[0].role_kind, "viewer");
        assert_eq!(
            snapshot.identity_bindings[0].proactive_delivery_surface,
            "feishu"
        );
        assert_eq!(
            snapshot.identity_bindings[0].binding_availability,
            "available"
        );
        assert!(snapshot.identity_bindings[0].binding_available);
        assert_eq!(snapshot.access_governance.permission_rule_count, 9);
        assert_eq!(snapshot.access_governance.owner_count, 1);
        assert_eq!(snapshot.access_governance.member_count, 2);
        assert_eq!(snapshot.access_governance.role_policies.len(), 6);
        assert_eq!(snapshot.gateway.setup_url, "");
        assert_eq!(snapshot.gateway.static_setup_url, "");
        assert_eq!(
            snapshot.gateway.manage_url,
            "http://gateway.local:4180/admin/im"
        );
        assert_eq!(snapshot.delivery_policy.interactive_reply, "source_bound");
        assert_eq!(
            snapshot.delivery_policy.proactive_delivery,
            "notification_target_default"
        );

        let _ = std::fs::remove_file(admin_path);
        let _ = std::fs::remove_file(registry_path);
    }

    #[test]
    fn sanitize_bridge_provider_status_keeps_platform_empty_without_feishu_fallback() {
        let sanitized = sanitize_bridge_provider_config(BridgeProviderConfig {
            configured: false,
            connected: false,
            platform: "   ".to_string(),
            gateway_base_url: "  http://gateway.local:4180  ".to_string(),
            app_name: "  HarborBeacon Bot  ".to_string(),
            ..Default::default()
        });

        assert_eq!(sanitized.platform, "");
        assert_eq!(sanitized.gateway_base_url, "http://gateway.local:4180");
        assert_eq!(sanitized.app_name, "HarborBeacon Bot");
        assert_eq!(sanitized.status, "未配置");
    }

    #[test]
    fn platform_projection_adds_bridge_provider_without_secret_metadata() {
        let mut state = super::AdminConsoleState::default();
        state.bridge_provider = BridgeProviderConfig {
            configured: true,
            connected: true,
            platform: "feishu".to_string(),
            gateway_base_url: "http://gateway.local:4180".to_string(),
            app_name: "HarborBeacon Bot".to_string(),
            status: "已连接".to_string(),
            last_checked_at: "2026-04-18T10:00:00Z".to_string(),
            capabilities: BridgeProviderCapabilities {
                reply: true,
                update: false,
                attachments: true,
            },
            ..Default::default()
        };
        state.identity_bindings.push(IdentityBindingRecord {
            open_id: "ou_demo".to_string(),
            user_id: Some("u_demo".to_string()),
            union_id: None,
            display_name: "Bean".to_string(),
            chat_id: None,
        });

        let platform = build_platform_state(&state);

        assert_eq!(platform.users.len(), 2);
        assert_eq!(platform.memberships.len(), 2);
        assert_eq!(platform.identity_bindings.len(), 1);
        assert!(!platform.permission_bindings.is_empty());
        assert_eq!(platform.provider_accounts.len(), 2);
        assert!(platform.credentials.is_empty());
        assert_eq!(platform.provider_accounts[1].provider_key, "im_bridge");
        assert_eq!(
            platform.provider_accounts[1].metadata["display_name"],
            json!("HarborBeacon Bot")
        );
        assert_eq!(
            platform.provider_accounts[1].capabilities["reply"],
            json!(true)
        );
    }

    #[test]
    fn resolved_identity_binding_records_prefers_platform_projection() {
        let mut state = super::AdminConsoleState::default();
        state.identity_bindings.push(IdentityBindingRecord {
            open_id: "ou_legacy".to_string(),
            user_id: Some("legacy-user".to_string()),
            union_id: None,
            display_name: "Legacy".to_string(),
            chat_id: Some("oc_legacy".to_string()),
        });
        state.platform.users.push(UserAccount {
            user_id: "viewer-1".to_string(),
            display_name: "Viewer".to_string(),
            email: None,
            phone: None,
            status: UserStatus::Active,
            default_workspace_id: Some("home-1".to_string()),
            preferences: json!({}),
        });
        state.platform.identity_bindings.push(IdentityBinding {
            identity_id: "identity-ou_viewer".to_string(),
            user_id: "viewer-1".to_string(),
            auth_source: AuthSource::ImChannel,
            provider_key: "im_bridge".to_string(),
            external_user_id: "ou_viewer".to_string(),
            external_union_id: None,
            external_chat_id: Some("oc_viewer".to_string()),
            profile_snapshot: json!({
                "display_name": "Viewer",
            }),
            last_seen_at: None,
        });

        let bindings = resolved_identity_binding_records(&state);

        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].open_id, "ou_viewer");
        assert_eq!(bindings[0].display_name, "Viewer");
        assert_eq!(bindings[0].chat_id.as_deref(), Some("oc_viewer"));
    }

    #[test]
    fn loaded_state_prefers_platform_projection_and_preserves_custom_memberships() {
        let mut state = super::AdminConsoleState::default();
        state.platform = build_platform_state(&state);
        state.remote_view.share_secret = "legacy-share-secret".to_string();
        state.platform.workspaces[0].settings = json!({
            "binding": {
                "channel": "Platform Bridge",
                "status": "平台已绑定",
                "metric": "平台在线",
                "bound_user": "Platform Admin",
            },
            "defaults": {
                "cidr": "10.42.0.0/24",
                "discovery": "ONVIF",
                "capture": "仅图片",
                "ai": "快速摘要",
                "notification_channel": "平台频道",
                "rtsp_port": 8554,
                "rtsp_paths": ["/alt/main"],
                "rtsp_username": "platform-user",
            },
            "remote_view": {
                "share_link_ttl_minutes": 45,
                "share_secret": "platform-share-secret",
                "share_secret_configured": true,
            }
        });
        state.platform.users.push(UserAccount {
            user_id: "viewer-1".to_string(),
            display_name: "Viewer".to_string(),
            email: None,
            phone: None,
            status: UserStatus::Active,
            default_workspace_id: Some("home-1".to_string()),
            preferences: json!({}),
        });
        state.platform.memberships.push(Membership {
            membership_id: "membership-viewer-1".to_string(),
            workspace_id: "home-1".to_string(),
            user_id: "viewer-1".to_string(),
            role_kind: RoleKind::Admin,
            status: MembershipStatus::Active,
            granted_by_user_id: Some("local-owner".to_string()),
            granted_at: None,
        });
        state.platform.identity_bindings.push(IdentityBinding {
            identity_id: "identity-ou_viewer".to_string(),
            user_id: "viewer-1".to_string(),
            auth_source: AuthSource::ImChannel,
            provider_key: "im_bridge".to_string(),
            external_user_id: "ou_viewer".to_string(),
            external_union_id: Some("on_viewer".to_string()),
            external_chat_id: Some("oc_viewer".to_string()),
            profile_snapshot: json!({
                "display_name": "Viewer",
            }),
            last_seen_at: None,
        });
        state.platform.recording_policies[0].metadata = json!({
            "recording_label": "持续录制",
            "capture_mode": "仅图片",
            "ai_mode": "快速摘要",
            "notification_channel": "平台频道",
        });

        normalize_loaded_admin_state(&mut state);

        assert_eq!(state.binding.channel, "Platform Bridge");
        assert_eq!(state.defaults.cidr, "10.42.0.0/24");
        assert_eq!(state.defaults.rtsp_port, 8554);
        assert_eq!(state.defaults.recording, "持续录制");
        assert_eq!(state.remote_view.share_link_ttl_minutes, 45);
        assert_eq!(state.remote_view.share_secret, "platform-share-secret");
        assert_eq!(state.identity_bindings.len(), 1);
        assert_eq!(state.identity_bindings[0].open_id, "ou_viewer");
        assert_eq!(state.identity_bindings[0].display_name, "Viewer");
        assert!(state.platform.memberships.iter().any(|membership| {
            membership.user_id == "viewer-1" && membership.role_kind == RoleKind::Admin
        }));
    }

    #[test]
    fn resolved_remote_view_config_prefers_workspace_projection() {
        let mut state = super::AdminConsoleState::default();
        state.remote_view = RemoteViewConfig {
            share_secret: "legacy-share-secret".to_string(),
            share_link_ttl_minutes: 120,
        };
        state.platform = build_platform_state(&state);
        state.platform.workspaces[0].settings["remote_view"] = json!({
            "share_secret": "platform-share-secret",
            "share_link_ttl_minutes": 30,
            "share_secret_configured": true,
        });

        let resolved = resolved_remote_view_config(&state);

        assert_eq!(
            resolved,
            RemoteViewConfig {
                share_secret: "platform-share-secret".to_string(),
                share_link_ttl_minutes: 30,
            }
        );
    }
}
