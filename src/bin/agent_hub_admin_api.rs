use std::collections::{BTreeMap, HashMap, HashSet};
use std::env;
use std::fmt::Write as _;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use hf_hub::{
    api::{sync::ApiBuilder as HfApiBuilder, Progress as HfProgress},
    Cache as HfCache, Repo, RepoType,
};
use reqwest::blocking::Client;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, ResponseBox, Server, StatusCode};
use uuid::Uuid;

use harborbeacon_local_agent::adapters::rtsp::{CommandRtspAdapter, RtspProbeAdapter};
#[cfg(test)]
use harborbeacon_local_agent::connectors::home_assistant::validate_home_assistant_service_fields as validate_home_assistant_service_fields_shared;
use harborbeacon_local_agent::connectors::home_assistant::{
    normalize_home_assistant_service_action_request,
    validate_home_assistant_service_action_request, HomeAssistantClient, HomeAssistantClientConfig,
    HomeAssistantEntity, HomeAssistantServiceActionRequest, HomeAssistantServiceCallResponse,
    HomeAssistantServiceDomain,
};
use harborbeacon_local_agent::connectors::im_gateway::GatewayPlatformStatus;
use harborbeacon_local_agent::connectors::notifications::{
    NotificationDeliveryError, NotificationDeliveryRecord, NotificationDeliveryService,
};
use harborbeacon_local_agent::connectors::storage::StorageTarget;
use harborbeacon_local_agent::control_plane::audit::{
    build_audit_summary, build_metadata_audit_record, query_audit_records, AuditActorKind,
    AuditRecord, AuditRecordQuery,
};
use harborbeacon_local_agent::control_plane::events::EventRecord;
use harborbeacon_local_agent::control_plane::media::{
    MediaAsset, MediaAssetKind, MediaSession, MediaSessionStatus, ShareLink,
};
use harborbeacon_local_agent::control_plane::models::{
    ModelEndpoint, ModelEndpointKind, ModelEndpointStatus, ModelKind, ModelRoutePolicy,
    PrivacyLevel,
};
use harborbeacon_local_agent::control_plane::routing::{
    build_routing_status, RoutingRuntimeProjection,
};
use harborbeacon_local_agent::control_plane::tasks::TaskStepRun;
use harborbeacon_local_agent::control_plane::users::{MembershipStatus, RoleKind};
use harborbeacon_local_agent::runtime::access_control::{
    authorize_access, AccessAction, AccessIdentityHints, AccessPrincipal,
};
use harborbeacon_local_agent::runtime::admin_console::{
    account_management_snapshot, dedupe_rtsp_paths, default_capture_subdirectory,
    default_clip_length_seconds, default_keyframe_count, default_keyframe_interval_seconds,
    default_model_endpoints, default_model_runtimes_for_store_root, default_model_store_root,
    device_rtsp_credential_id, harboros_writable_root, normalize_delivery_surface,
    path_is_same_or_inside, user_default_delivery_surface, user_recent_interactive_surface,
    validate_knowledge_settings, AccountManagementSnapshot, AdminConsoleState, AdminConsoleStore,
    AdminDefaults, AdminModelCenterState, AutomationRuleReview, BridgeProviderConfig,
    DeviceCredentialSecret, DeviceEvidenceRecord, GatewayStatusSummary, HomeAssistantAdminState,
    HomeAssistantConfigUpdate, KnowledgeIndexJobRecord, KnowledgeSettings, KnowledgeSourceRoot,
    ModelDownloadJobRecord, ModelRuntimeRecord, NotificationTargetRecord, RagResourceProfile,
};
use harborbeacon_local_agent::runtime::discovery::RtspProbeRequest;
use harborbeacon_local_agent::runtime::dvr::{
    apply_retention_policy, build_status_response, dvr_media_preview_path, media_library_root_path,
    scan_timeline, store_snapshot_bytes, DvrRecordingSettings, DvrRuntime, DvrTimelineSegment,
};
use harborbeacon_local_agent::runtime::evt_readiness::{
    build_evt_evidence_bundle, build_evt_readiness_report, evt_preflight_workflow_summary,
    evt_readiness_workflow_summary, run_evt_preflight_report,
};
use harborbeacon_local_agent::runtime::family_memory::{
    append_family_memory_feedback_default, apply_family_memory_overlays_to_events,
    build_family_memory_event_views, build_family_memory_stats,
    compact_family_memory_feedback_evidence, family_memory_overlays_default,
    family_memory_stats_default, list_family_memory_feedback_default, FamilyMemoryEventFilter,
    FamilyMemoryEventView, FamilyMemoryFeedbackRecord, FamilyMemoryFeedbackRequest,
    FamilyMemoryStats, DEFAULT_FAMILY_MEMORY_FEEDBACK_LIMIT,
};
use harborbeacon_local_agent::runtime::family_timeline::{
    build_family_timeline_digest_from_query, build_family_timeline_from_query,
    FamilyTimelineDigest, FamilyTimelineQueryOptions, FamilyTimelineResponse,
};
use harborbeacon_local_agent::runtime::home_guardian::{
    append_home_guardian_run_summary, automation_review_is_home_guardian,
    build_home_guardian_activity_response_with_activity, build_home_guardian_evaluation_response,
    build_home_guardian_rule_evaluation_plan, compact_home_guardian_evaluation_evidence,
    compact_home_guardian_proposal_evidence, complete_home_guardian_rule_execution,
    home_guardian_ha_service_request, home_guardian_idempotent_action_result,
    home_guardian_redacted_action_plan, home_guardian_rule_run_summary,
    home_guardian_run_already_recorded, home_guardian_unsupported_action_result,
    select_home_guardian_reviews, HomeGuardianActionExecutionKind, HomeGuardianEvaluationResponse,
    HomeGuardianRuleEvaluationPlan,
};
use harborbeacon_local_agent::runtime::hub::{
    CameraConnectRequest, CameraHubService, HubManualAddSummary, HubScanRequest, HubScanSummary,
    HubStateSnapshot,
};
use harborbeacon_local_agent::runtime::knowledge::{
    KnowledgeSearchRequest, KnowledgeSearchService,
};
use harborbeacon_local_agent::runtime::knowledge_index::{
    load_embedding_store, KnowledgeEmbeddingWarmupStats, KnowledgeIndexConfig,
    KnowledgeIndexManifest, KnowledgeIndexService, KnowledgeIndexSnapshot, KnowledgeModality,
};
use harborbeacon_local_agent::runtime::media::{SnapshotCaptureRequest, SnapshotFormat};
use harborbeacon_local_agent::runtime::media_tools::{ffmpeg_resolution_hint, resolve_ffmpeg_bin};
use harborbeacon_local_agent::runtime::model_center::{
    load_model_center_state, redact_model_endpoint, run_vlm_summary_with_state,
    test_model_endpoint, vlm_endpoint_readiness, vlm_execution_runtime_snapshot,
    ModelEndpointTestResult, ADMIN_STATE_PATH_ENV,
};
use harborbeacon_local_agent::runtime::privacy_gateway::PRIVACY_GATEWAY_POLICY_VERSION;
use harborbeacon_local_agent::runtime::registry::{
    CameraCapabilities, CameraDevice, DeviceRegistryStore, HomeAssistantRegistryEntity,
};
use harborbeacon_local_agent::runtime::remote_view;
use harborbeacon_local_agent::runtime::task_api::{
    TaskApiService, TaskApprovalSummary, TaskIntent, TaskRequest, TaskResponse, TaskSource,
    TaskStatus,
};
use harborbeacon_local_agent::runtime::task_session::TaskConversationStore;
use harborbeacon_local_agent::runtime::vision_event::{
    attach_vlm_summary_to_event, build_local_vision_notification_intent,
    ingest_local_vision_event_default, list_recent_local_vision_events_default,
    local_vision_event_store_stats_default, LocalVisionEvent, StoredLocalVisionEvent,
    MAX_LOCAL_VISION_EVENT_JSON_BYTES,
};

const DEFAULT_HF_ENDPOINT: &str = "https://hf-mirror.com";
const KNOWLEDGE_EMBEDDING_WARMUP_TIMEOUT_MS_ENV: &str =
    "HARBORBEACON_KNOWLEDGE_EMBEDDING_WARMUP_TIMEOUT_MS";
const DEFAULT_KNOWLEDGE_EMBEDDING_WARMUP_TIMEOUT_MS: u64 = 120_000;
const HOME_GUARDIAN_EVALUATION_QUEUE_CAPACITY: usize = 128;
const HOME_GUARDIAN_AUTO_EVALUATION_MIN_INTERVAL_SECONDS: u64 = 60;
const PRIVACY_GATEWAY_AUDIT_ACTION: &str = "privacy_gateway.rag_answer.evaluate";
const PRIVACY_GATEWAY_READINESS_AUDIT_LIMIT: usize = 20;
const HOME_GUARDIAN_ACTION_COOLDOWN_SECONDS: u64 = 300;
const HOME_GUARDIAN_IDEMPOTENCY_MEMORY_SECONDS: u64 = 7 * 24 * 60 * 60;
const ADMIN_HTTP_WORKER_COUNT: usize = 32;
const ADMIN_HTTP_REQUEST_QUEUE_CAPACITY: usize = 256;
const MAX_VISION_EVENT_INGEST_INFLIGHT: usize = 8;
const HOME_GUARDIAN_ACTIVITY_LIMIT: usize = 500;
const MAX_FAMILY_MEMORY_FEEDBACK_JSON_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone)]
struct Cli {
    bind: String,
    admin_state: PathBuf,
    device_registry: PathBuf,
    conversations: PathBuf,
    harbor_assistant_dist: PathBuf,
    public_origin: String,
}

fn take_value(args: &[String], index: &mut usize, flag: &str) -> String {
    *index += 1;
    if *index >= args.len() {
        fail(&format!("missing value for {flag}"));
    }
    args[*index].clone()
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(2);
}

fn print_usage() {
    eprintln!(
        "Usage: agent-hub-admin-api [--bind ADDR] [--admin-state PATH] [--device-registry PATH] [--conversations PATH] [--harbor-assistant-dist PATH] [--public-origin URL]"
    );
}

impl Default for Cli {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:4174".to_string(),
            admin_state: PathBuf::from(".harborbeacon/admin-console.json"),
            device_registry: PathBuf::from(".harborbeacon/device-registry.json"),
            conversations: PathBuf::from(".harborbeacon/task-api-conversations.json"),
            harbor_assistant_dist: PathBuf::from("frontend/harbor-assistant/dist/harbor-assistant"),
            public_origin: "http://harborbeacon.local:4174".to_string(),
        }
    }
}

impl Cli {
    fn parse() -> Self {
        let args = std::env::args().skip(1).collect::<Vec<_>>();
        if matches!(args.first().map(String::as_str), Some("--help" | "-h")) {
            print_usage();
            std::process::exit(0);
        }

        let mut cli = Self::default();
        let mut index = 0;
        while index < args.len() {
            let arg = &args[index];
            match arg.as_str() {
                "--bind" => cli.bind = take_value(&args, &mut index, "--bind"),
                value if value.starts_with("--bind=") => {
                    cli.bind = value["--bind=".len()..].to_string();
                }
                "--admin-state" => {
                    cli.admin_state = PathBuf::from(take_value(&args, &mut index, "--admin-state"))
                }
                value if value.starts_with("--admin-state=") => {
                    cli.admin_state = PathBuf::from(value["--admin-state=".len()..].to_string())
                }
                "--device-registry" => {
                    cli.device_registry =
                        PathBuf::from(take_value(&args, &mut index, "--device-registry"))
                }
                value if value.starts_with("--device-registry=") => {
                    cli.device_registry =
                        PathBuf::from(value["--device-registry=".len()..].to_string())
                }
                "--conversations" => {
                    cli.conversations =
                        PathBuf::from(take_value(&args, &mut index, "--conversations"))
                }
                value if value.starts_with("--conversations=") => {
                    cli.conversations = PathBuf::from(value["--conversations=".len()..].to_string())
                }
                "--harbor-assistant-dist" => {
                    cli.harbor_assistant_dist =
                        PathBuf::from(take_value(&args, &mut index, "--harbor-assistant-dist"))
                }
                value if value.starts_with("--harbor-assistant-dist=") => {
                    cli.harbor_assistant_dist =
                        PathBuf::from(value["--harbor-assistant-dist=".len()..].to_string())
                }
                "--public-origin" => {
                    cli.public_origin = take_value(&args, &mut index, "--public-origin")
                }
                value if value.starts_with("--public-origin=") => {
                    cli.public_origin = value["--public-origin=".len()..].to_string();
                }
                "--help" | "-h" => {
                    print_usage();
                    std::process::exit(0);
                }
                value if value.starts_with('-') => {
                    fail(&format!("unknown flag: {value}"));
                }
                value => {
                    fail(&format!("unexpected positional argument: {value}"));
                }
            }
            index += 1;
        }

        cli
    }
}

#[derive(Clone)]
pub struct AdminApi {
    admin_store: AdminConsoleStore,
    task_service: TaskApiService,
    dvr_runtime: DvrRuntime,
    hls_live_runtime: HlsLiveRuntime,
    harbor_assistant_dist: PathBuf,
    public_origin: String,
    model_runtime_activation: Option<ModelRuntimeActivationHandler>,
    last_event_notification_attempt: Arc<Mutex<Option<Value>>>,
    last_home_assistant_service_action: Arc<Mutex<Option<Value>>>,
    last_evt_preflight: Arc<Mutex<Option<Value>>>,
    last_vlm_enrichment: Arc<Mutex<Option<Value>>>,
    last_family_timeline_digest: Arc<Mutex<Option<Value>>>,
    last_family_memory_feedback: Arc<Mutex<Option<Value>>>,
    last_guardian_rule_proposal: Arc<Mutex<Option<Value>>>,
    last_guardian_rule_evaluation: Arc<Mutex<Option<Value>>>,
    guardian_queue: HomeGuardianEvaluationQueue,
    guardian_auto_eval_cooldowns: Arc<Mutex<HashMap<String, u64>>>,
    guardian_cooldowns: Arc<Mutex<HashMap<String, u64>>>,
    guardian_action_runs: Arc<Mutex<HashMap<String, u64>>>,
    guardian_rule_cache: Arc<Mutex<HomeGuardianRuleCache>>,
    guardian_activity_log_lock: Arc<Mutex<()>>,
    http_runtime: HttpRuntimeCounters,
    vision_ingest_limiter: VisionIngestLimiter,
}

#[derive(Clone, Default)]
struct HomeGuardianRuleCache {
    active_reviews: Vec<AutomationRuleReview>,
    loaded_at_epoch: u64,
    file_bytes: u64,
    modified_epoch_ms: Option<u128>,
    refresh_error: Option<String>,
}

#[derive(Clone)]
struct HttpRuntimeCounters {
    active: Arc<AtomicUsize>,
    accepted_total: Arc<AtomicU64>,
    rejected_total: Arc<AtomicU64>,
    completed_total: Arc<AtomicU64>,
}

impl HttpRuntimeCounters {
    fn new() -> Self {
        Self {
            active: Arc::new(AtomicUsize::new(0)),
            accepted_total: Arc::new(AtomicU64::new(0)),
            rejected_total: Arc::new(AtomicU64::new(0)),
            completed_total: Arc::new(AtomicU64::new(0)),
        }
    }

    fn record_accepted(&self) {
        self.accepted_total.fetch_add(1, Ordering::Relaxed);
    }

    fn record_rejected(&self) {
        self.rejected_total.fetch_add(1, Ordering::Relaxed);
    }

    fn begin_request(&self) -> HttpRequestGuard {
        self.active.fetch_add(1, Ordering::Relaxed);
        HttpRequestGuard {
            counters: self.clone(),
        }
    }

    fn snapshot(&self) -> Value {
        let active = self.active.load(Ordering::Relaxed);
        let accepted_total = self.accepted_total.load(Ordering::Relaxed);
        let completed_total = self.completed_total.load(Ordering::Relaxed);
        let queued_pending = accepted_total
            .saturating_sub(completed_total)
            .saturating_sub(active as u64);
        json!({
            "worker_count": ADMIN_HTTP_WORKER_COUNT,
            "queue_capacity": ADMIN_HTTP_REQUEST_QUEUE_CAPACITY,
            "active_requests": active,
            "queued_pending": queued_pending,
            "accepted_total": accepted_total,
            "rejected_total": self.rejected_total.load(Ordering::Relaxed),
            "completed_total": completed_total,
            "process_thread_count": current_process_thread_count(),
            "bounded": true,
            "metadata_only": true,
        })
    }
}

struct HttpRequestGuard {
    counters: HttpRuntimeCounters,
}

impl Drop for HttpRequestGuard {
    fn drop(&mut self) {
        self.counters.active.fetch_sub(1, Ordering::Relaxed);
        self.counters
            .completed_total
            .fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Clone)]
struct VisionIngestLimiter {
    active: Arc<AtomicUsize>,
    accepted_total: Arc<AtomicU64>,
    rejected_total: Arc<AtomicU64>,
}

impl VisionIngestLimiter {
    fn new() -> Self {
        Self {
            active: Arc::new(AtomicUsize::new(0)),
            accepted_total: Arc::new(AtomicU64::new(0)),
            rejected_total: Arc::new(AtomicU64::new(0)),
        }
    }

    fn try_acquire(&self) -> Option<VisionIngestGuard> {
        loop {
            let active = self.active.load(Ordering::Relaxed);
            if active >= MAX_VISION_EVENT_INGEST_INFLIGHT {
                self.rejected_total.fetch_add(1, Ordering::Relaxed);
                return None;
            }
            if self
                .active
                .compare_exchange_weak(active, active + 1, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                self.accepted_total.fetch_add(1, Ordering::Relaxed);
                return Some(VisionIngestGuard {
                    limiter: self.clone(),
                });
            }
        }
    }

    fn snapshot(&self) -> Value {
        json!({
            "max_inflight": MAX_VISION_EVENT_INGEST_INFLIGHT,
            "active_inflight": self.active.load(Ordering::Relaxed),
            "accepted_total": self.accepted_total.load(Ordering::Relaxed),
            "rejected_total": self.rejected_total.load(Ordering::Relaxed),
            "bounded": true,
            "metadata_only": true,
        })
    }
}

struct VisionIngestGuard {
    limiter: VisionIngestLimiter,
}

impl Drop for VisionIngestGuard {
    fn drop(&mut self) {
        self.limiter.active.fetch_sub(1, Ordering::Release);
    }
}

#[derive(Clone)]
struct HomeGuardianEvaluationQueue {
    sender: SyncSender<StoredLocalVisionEvent>,
    queued: Arc<AtomicU64>,
    dropped: Arc<AtomicU64>,
    processed: Arc<AtomicU64>,
    failed: Arc<AtomicU64>,
    coalesced: Arc<AtomicU64>,
    disconnected: Arc<AtomicU64>,
}

impl HomeGuardianEvaluationQueue {
    fn new(sender: SyncSender<StoredLocalVisionEvent>) -> Self {
        Self {
            sender,
            queued: Arc::new(AtomicU64::new(0)),
            dropped: Arc::new(AtomicU64::new(0)),
            processed: Arc::new(AtomicU64::new(0)),
            failed: Arc::new(AtomicU64::new(0)),
            coalesced: Arc::new(AtomicU64::new(0)),
            disconnected: Arc::new(AtomicU64::new(0)),
        }
    }

    fn enqueue(&self, event: StoredLocalVisionEvent) -> &'static str {
        match self.sender.try_send(event) {
            Ok(()) => {
                self.queued.fetch_add(1, Ordering::Relaxed);
                "queued"
            }
            Err(TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                "dropped"
            }
            Err(TrySendError::Disconnected(_)) => {
                self.disconnected.fetch_add(1, Ordering::Relaxed);
                "disconnected"
            }
        }
    }

    fn record_processed(&self) {
        self.processed.fetch_add(1, Ordering::Relaxed);
    }

    fn record_failed(&self) {
        self.failed.fetch_add(1, Ordering::Relaxed);
    }

    fn record_coalesced(&self) {
        self.coalesced.fetch_add(1, Ordering::Relaxed);
    }

    fn snapshot(&self) -> Value {
        json!({
            "capacity": HOME_GUARDIAN_EVALUATION_QUEUE_CAPACITY,
            "auto_evaluation_min_interval_seconds": HOME_GUARDIAN_AUTO_EVALUATION_MIN_INTERVAL_SECONDS,
            "queued_total": self.queued.load(Ordering::Relaxed),
            "dropped_total": self.dropped.load(Ordering::Relaxed),
            "processed_total": self.processed.load(Ordering::Relaxed),
            "failed_total": self.failed.load(Ordering::Relaxed),
            "coalesced_total": self.coalesced.load(Ordering::Relaxed),
            "disconnected_total": self.disconnected.load(Ordering::Relaxed),
            "bounded": true,
            "metadata_only": true,
        })
    }
}

pub(crate) type ModelRuntimeActivationHandler = Arc<
    dyn Fn(ModelRuntimeActivationRequest) -> Result<ModelRuntimeActivationResult, String>
        + Send
        + Sync,
>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelRuntimeActivationRequest {
    pub capability_id: String,
    pub model_id: String,
    pub model_kind: ModelKind,
    pub local_path: Option<String>,
    pub runtime_profiles: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelRuntimeActivationResult {
    pub activated: bool,
    pub status: String,
    pub message: String,
    pub runtime_model_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ManualAddRequest {
    name: String,
    room: Option<String>,
    ip: String,
    path: Option<String>,
    snapshot_url: Option<String>,
    username: Option<String>,
    password: Option<String>,
    port: Option<u16>,
}

#[derive(Debug, Deserialize)]
struct DefaultsRequest {
    cidr: String,
    discovery: String,
    recording: String,
    capture: String,
    ai: String,
    #[serde(alias = "feishu_group")]
    notification_channel: String,
    #[serde(default)]
    rtsp_username: String,
    #[serde(default)]
    rtsp_password: String,
    #[serde(default)]
    rtsp_port: Option<u16>,
    #[serde(default)]
    rtsp_paths: Vec<String>,
    #[serde(default)]
    selected_camera_device_id: Option<String>,
    #[serde(default)]
    capture_subdirectory: Option<String>,
    #[serde(default)]
    clip_length_seconds: Option<u32>,
    #[serde(default)]
    keyframe_count: Option<u32>,
    #[serde(default)]
    keyframe_interval_seconds: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct DefaultCameraRequest {
    #[serde(default)]
    device_id: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct DeviceCredentialsRequest {
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    rtsp_port: Option<u16>,
    #[serde(default)]
    rtsp_paths: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
struct RtspCheckRequest {
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    rtsp_port: Option<u16>,
    #[serde(default)]
    rtsp_paths: Vec<String>,
}

#[derive(Debug, Deserialize, Default)]
struct DeviceMetadataPatchRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    room: Option<String>,
    #[serde(default)]
    vendor: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    ip_address: Option<String>,
    #[serde(default)]
    snapshot_url: Option<String>,
    #[serde(default)]
    primary_stream_url: Option<String>,
    #[serde(default)]
    rtsp_path: Option<String>,
    #[serde(default)]
    rtsp_port: Option<u16>,
    #[serde(default)]
    requires_auth: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct BridgeConfigRequest {}

#[derive(Debug, Deserialize, Default)]
struct HomeAssistantConfigRequest {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    base_url: String,
    #[serde(default)]
    access_token: Option<String>,
    #[serde(default)]
    exposed_domains: Vec<String>,
}

#[derive(Debug, Serialize)]
struct HomeAssistantStatusResponse {
    configured: bool,
    enabled: bool,
    base_url: String,
    token_configured: bool,
    token_redacted: bool,
    exposed_domains: Vec<String>,
    status: String,
    #[serde(default)]
    last_error: Option<String>,
    #[serde(default)]
    last_test_at: Option<String>,
    #[serde(default)]
    last_sync_at: Option<String>,
    entity_count: usize,
    service_count: usize,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    location_name: Option<String>,
}

#[derive(Debug, Serialize)]
struct HomeAssistantConfigResponse {
    status: HomeAssistantStatusResponse,
}

#[derive(Debug, Serialize)]
struct HomeAssistantEntitiesResponse {
    entities: Vec<HomeAssistantEntity>,
}

#[derive(Debug, Serialize)]
struct HomeAssistantServicesResponse {
    services: Vec<HomeAssistantServiceDomain>,
}

type HomeAssistantServiceSmokeRequest = HomeAssistantServiceActionRequest;

#[derive(Debug, Serialize)]
struct HomeAssistantServiceSmokeResponse {
    status: String,
    allowed: bool,
    executed: bool,
    domain: String,
    service: String,
    entity_id: String,
    message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result: Option<HomeAssistantServiceCallResponse>,
    audit_record: Value,
}

#[derive(Debug, Serialize)]
struct HomeAssistantServiceActionResponse {
    action_id: String,
    status: String,
    allowed: bool,
    executed: bool,
    domain: String,
    service: String,
    entity_id: String,
    message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result: Option<HomeAssistantServiceCallResponse>,
    audit_record: Value,
}

#[derive(Debug, Serialize)]
struct HomeAssistantSyncResponse {
    status: HomeAssistantStatusResponse,
    entities: Vec<HomeAssistantEntity>,
    service_domains: Vec<HomeAssistantServiceDomain>,
}

#[derive(Debug, Serialize)]
struct InferenceHealthAliasResponse {
    status: String,
    ready: bool,
    service: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    backend_kind: Option<String>,
    backend: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    chat_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    embedding_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct RedactedDiagnosticsBundleResponse {
    generated_at: String,
    host: Value,
    services: Vec<Value>,
    memory: Value,
    cameras: Value,
    events: Value,
    workflow: Value,
    home_assistant: HomeAssistantStatusResponse,
    models: Value,
    security: Value,
    audit_record: Value,
}

#[derive(Debug, Deserialize, Default)]
struct HomeAssistantInstallRequest {
    #[serde(default)]
    dry_run: bool,
}

#[derive(Debug, Serialize)]
struct HomeAssistantInstallStatusResponse {
    app_id: String,
    status: String,
    managed: bool,
    runtime: String,
    #[serde(default)]
    container_name: Option<String>,
    #[serde(default)]
    onboarding_url: Option<String>,
    message: String,
}

#[derive(Debug, Serialize)]
struct HomeAssistantInstallPlanResponse {
    app_id: String,
    target: String,
    runtime: String,
    image: String,
    container_name: String,
    ports: Vec<String>,
    volumes: Vec<String>,
    next_step: String,
}

#[derive(Debug, Serialize)]
struct HomeAssistantInstallResponse {
    status: String,
    dry_run: bool,
    plan: HomeAssistantInstallPlanResponse,
    message: String,
}

#[derive(Debug, Deserialize)]
struct NotificationTargetUpsertRequest {
    #[serde(default)]
    target_id: Option<String>,
    label: String,
    route_key: String,
    #[serde(default)]
    platform_hint: String,
    #[serde(default)]
    is_default: bool,
}

#[derive(Debug, Deserialize)]
struct NotificationTargetDefaultRequest {
    target_id: String,
}

#[derive(Debug, Serialize)]
struct NotificationTargetsResponse {
    targets: Vec<harborbeacon_local_agent::runtime::admin_console::NotificationTargetRecord>,
}

#[derive(Debug, Deserialize)]
struct AutomationReviewRequest {
    #[serde(default)]
    review_id: Option<String>,
    #[serde(default)]
    workspace_id: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    source_channel: Option<String>,
    #[serde(default)]
    source_conversation_id: Option<String>,
    original_prompt: String,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    trigger_definition: Option<Value>,
    #[serde(default)]
    condition_definition: Option<Value>,
    #[serde(default)]
    action_plan: Option<Value>,
    #[serde(default)]
    device_refs: Vec<Value>,
    #[serde(default)]
    risk_level: Option<String>,
    #[serde(default)]
    requires_approval: bool,
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    rule_id: Option<String>,
    #[serde(default)]
    run_summaries: Vec<Value>,
    #[serde(default)]
    metadata: Option<Value>,
}

#[derive(Debug, Serialize)]
struct AutomationReviewsResponse {
    generated_at: String,
    pending_count: usize,
    reviews: Vec<AutomationRuleReview>,
}

#[derive(Debug, Deserialize, Default)]
struct ApprovalDecisionRequest {
    #[serde(default)]
    approver_user_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct MembershipRoleUpdateRequest {
    role_kind: String,
}

#[derive(Debug, Deserialize)]
struct DefaultDeliverySurfaceUpdateRequest {
    surface: String,
}

#[derive(Debug, Serialize)]
struct ModelEndpointsResponse {
    endpoints: Vec<ModelEndpoint>,
}

#[derive(Debug, Serialize)]
struct ModelPoliciesResponse {
    route_policies: Vec<ModelRoutePolicy>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct FeatureAvailabilityResponse {
    groups: Vec<FeatureAvailabilityGroup>,
}

#[derive(Debug, Serialize)]
struct VisionEventsResponse {
    generated_at: String,
    limit: usize,
    events: Vec<StoredLocalVisionEvent>,
}

#[derive(Debug, Serialize)]
struct FamilyTimelineApiResponse {
    #[serde(flatten)]
    timeline: FamilyTimelineResponse,
    memory_overlay: FamilyMemoryStats,
}

#[derive(Debug, Serialize)]
struct FamilyTimelineDigestApiResponse {
    #[serde(flatten)]
    digest: FamilyTimelineDigest,
    memory_overlay: FamilyMemoryStats,
}

#[derive(Debug, Serialize)]
struct FamilyMemoryEventsResponse {
    generated_at: String,
    limit: usize,
    metadata_only: bool,
    secret_scan: String,
    memory_overlay: FamilyMemoryStats,
    events: Vec<FamilyMemoryEventView>,
}

#[derive(Debug, Serialize)]
struct FamilyMemoryEventResponse {
    generated_at: String,
    metadata_only: bool,
    secret_scan: String,
    event: FamilyMemoryEventView,
}

#[derive(Debug, Serialize)]
struct FamilyMemoryFeedbackResponse {
    status: String,
    event_id: String,
    feedback: FamilyMemoryFeedbackRecord,
    memory_overlay: FamilyMemoryStats,
    evidence: Value,
    metadata_only: bool,
    secret_scan: String,
}

#[derive(Debug, Serialize, Clone)]
struct LocalVisionEventNotificationResponse {
    event_id: String,
    status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notification_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delivery_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    target_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    platform_hint: Option<String>,
    message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    delivery_record: Option<Value>,
    audit_record: Value,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct FeatureAvailabilityGroup {
    group_id: String,
    label: String,
    items: Vec<FeatureAvailabilityItem>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct FeatureAvailabilityItem {
    feature_id: String,
    label: String,
    owner_lane: String,
    status: String,
    source_of_truth: String,
    current_option: String,
    fallback_order: Vec<String>,
    blocker: String,
    evidence: Vec<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct ReleaseReadinessResponse {
    generated_at: String,
    checked_at: String,
    status: String,
    summary: String,
    overall_status: String,
    harbor_desk: ReadinessSurfaceSummary,
    groups: Vec<ReleaseReadinessGroup>,
    checklist: Vec<ReleaseReadinessItem>,
    status_cards: Vec<ReleaseReadinessStatusCard>,
    deep_links: Vec<ReleaseReadinessDeepLink>,
    blockers: Vec<String>,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct ReadinessSurfaceSummary {
    admin_origin: String,
    admin_port: u16,
    harboros_webui: String,
    note: String,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct ReleaseReadinessGroup {
    group_id: String,
    label: String,
    owner_lane: String,
    status: String,
    items: Vec<ReleaseReadinessItem>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct ReleaseReadinessItem {
    id: String,
    item_id: String,
    label: String,
    lane: String,
    owner_lane: String,
    status: String,
    summary: String,
    detail: String,
    endpoint: String,
    source_of_truth: String,
    deep_link: String,
    next_action: String,
    action_path: String,
    last_verified_at: Option<String>,
    blocking_reason: String,
    blockers: Vec<String>,
    evidence: Vec<String>,
    evidence_records: Vec<ReadinessEvidenceRecord>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct ReadinessEvidenceRecord {
    generated_at: String,
    lane: String,
    status: String,
    action_path: String,
    blocking_reason: String,
    evidence: Vec<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct ReleaseReadinessStatusCard {
    id: String,
    label: String,
    value: String,
    status: String,
    detail: String,
    endpoint: String,
    deep_link: String,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct ReleaseReadinessDeepLink {
    label: String,
    href: String,
    detail: String,
    endpoint: String,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct ReleaseReadinessHistoryResponse {
    generated_at: String,
    entries: Vec<ReleaseReadinessResponse>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct HardwareReadinessResponse {
    generated_at: String,
    status: String,
    cpu: HardwareComponentReadiness,
    memory: HardwareComponentReadiness,
    gpu: HardwareComponentReadiness,
    npu: HardwareComponentReadiness,
    memory_mb: Option<u64>,
    gpu_vram_total_mb: Option<u64>,
    gpu_vram_free_mb: Option<u64>,
    hardware_class: String,
    recommended_model_profile: String,
    blockers: Vec<String>,
    evidence: Vec<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct HardwareComponentReadiness {
    status: String,
    summary: String,
    detail: String,
    evidence: Vec<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct HarborOsStatusResponse {
    generated_at: String,
    status: String,
    version: String,
    webui_url: String,
    system_domain_only: bool,
    services: Vec<HarborOsServiceStatus>,
    jobs_alerts: HarborOsServiceStatus,
    storage_files_entry: HarborOsServiceStatus,
    evidence: Vec<String>,
    blockers: Vec<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct HarborOsServiceStatus {
    service_id: String,
    label: String,
    status: String,
    detail: String,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct HarborOsImCapabilityMapResponse {
    generated_at: String,
    source: String,
    items: Vec<HarborOsImCapabilityItem>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct HarborOsImCapabilityItem {
    capability_id: String,
    label: String,
    capability_class: String,
    im_ready: bool,
    risk_level: String,
    approval_required: bool,
    harboros_surface: String,
    notes: String,
}

#[derive(Debug, Serialize, Clone, PartialEq)]
struct RagReadinessResponse {
    generated_at: String,
    status: String,
    summary: String,
    source_roots: RagReadinessComponent,
    index_directory: RagReadinessComponent,
    embedding_model: RagReadinessComponent,
    model_readiness: Vec<RagModelReadinessCard>,
    resource_profiles: Vec<RagResourceProfileStatus>,
    capability_profiles: Vec<RagCapabilityReadinessCard>,
    privacy_policy: RagReadinessComponent,
    privacy_gateway: RagPrivacyGatewayReadiness,
    media_parser: RagReadinessComponent,
    storage_writable: RagReadinessComponent,
    index_jobs: Vec<KnowledgeIndexJobRecord>,
    blockers: Vec<String>,
    warnings: Vec<String>,
    evidence: Vec<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct RagReadinessComponent {
    status: String,
    summary: String,
    detail: String,
    evidence: Vec<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct RagPrivacyGatewayReadiness {
    status: String,
    summary: String,
    policy_version: String,
    covered_routes: Vec<String>,
    privacy_level: String,
    default_resource_profile: String,
    recent_audit_count: usize,
    decision_counts: BTreeMap<String, usize>,
    risk_counts: BTreeMap<String, usize>,
    latest_privacy_transform_id: Option<String>,
    latest_evaluated_at: Option<String>,
    warning_count: usize,
    metadata_only: bool,
    secret_scan: String,
    evidence: Vec<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct RagModelReadinessCard {
    model_kind: String,
    label: String,
    status: String,
    endpoint_id: Option<String>,
    endpoint_kind: Option<String>,
    provider_key: Option<String>,
    model_name: Option<String>,
    detail: String,
    blocker: Option<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct RagResourceProfileStatus {
    profile: String,
    label: String,
    status: String,
    detail: String,
    blockers: Vec<String>,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct RagCapabilityReadinessCard {
    capability_id: String,
    label: String,
    status: String,
    summary: String,
    blockers: Vec<String>,
    warnings: Vec<String>,
    evidence: Vec<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct KnowledgeIndexRunResponse {
    generated_at: String,
    job_ids: Vec<String>,
    status: String,
    index_root: String,
    root_count: usize,
    indexed_roots: Vec<KnowledgeIndexRootStatus>,
    errors: Vec<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
struct KnowledgeSearchApiRequest {
    query: String,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    include_documents: Option<bool>,
    #[serde(default)]
    include_images: Option<bool>,
    #[serde(default)]
    include_videos: Option<bool>,
    #[serde(default)]
    source_scope: Option<String>,
    #[serde(default)]
    camera_id: Option<String>,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
}

impl KnowledgeSearchApiRequest {
    fn has_dvr_focus(&self) -> bool {
        [
            self.camera_id.as_deref(),
            self.from.as_deref(),
            self.to.as_deref(),
        ]
        .into_iter()
        .flatten()
        .any(|value| !value.trim().is_empty())
    }
}

#[derive(Debug, Serialize, Clone, PartialEq)]
struct KnowledgeIndexStatusResponse {
    generated_at: String,
    status: String,
    settings: KnowledgeSettings,
    index_root_exists: bool,
    index_root_writable: bool,
    manifest_count: usize,
    manifest_entry_count: usize,
    document_count: usize,
    image_count: usize,
    audio_count: usize,
    video_count: usize,
    content_indexed_image_count: usize,
    vlm_indexed_image_count: usize,
    ocr_indexed_image_count: usize,
    image_content_missing_count: usize,
    image_text_source_counts: BTreeMap<String, usize>,
    embedding_cache_count: usize,
    embedding_entry_count: usize,
    storage_usage_bytes: u64,
    last_indexed_at: Option<String>,
    source_roots: Vec<KnowledgeIndexRootStatus>,
    blockers: Vec<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct KnowledgeIndexRootStatus {
    root_id: String,
    label: String,
    path: String,
    enabled: bool,
    exists: bool,
    last_indexed_at: Option<String>,
    status: String,
    detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct KnowledgeIndexStorageSummary {
    manifest_count: usize,
    manifest_entry_count: usize,
    document_count: usize,
    image_count: usize,
    audio_count: usize,
    video_count: usize,
    content_indexed_image_count: usize,
    vlm_indexed_image_count: usize,
    ocr_indexed_image_count: usize,
    image_content_missing_count: usize,
    image_text_source_counts: BTreeMap<String, usize>,
    embedding_cache_count: usize,
    embedding_entry_count: usize,
    storage_usage_bytes: u64,
    last_indexed_at: Option<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct FilesBrowseResponse {
    path: String,
    parent: Option<String>,
    readonly: bool,
    allowed_roots: Vec<String>,
    entries: Vec<FileBrowseEntry>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct FileBrowseEntry {
    name: String,
    path: String,
    is_dir: bool,
    size_bytes: Option<u64>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct LocalModelCatalogResponse {
    generated_at: String,
    checked_at: String,
    status: String,
    cache_roots: Vec<String>,
    models: Vec<LocalModelCatalogItem>,
    download_jobs: Vec<ModelDownloadJobRecord>,
    downloads: Vec<ModelDownloadJobRecord>,
    blockers: Vec<String>,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct LocalModelCatalogItem {
    model_id: String,
    label: String,
    display_name: String,
    provider: String,
    provider_key: String,
    model_kind: String,
    recommended_hardware: String,
    status: String,
    installed: bool,
    local_path: Option<String>,
    size_bytes: Option<u64>,
    download_job_id: Option<String>,
    download_size_hint: String,
    hardware_fit: String,
    fit_reason: String,
    recommendation_group: String,
    detail: String,
    source_kind: String,
    installable: bool,
    manual_only: bool,
    repo_id: Option<String>,
    revision: Option<String>,
    file_policy: String,
    default_hf_endpoint: Option<String>,
    runtime_profiles: Vec<String>,
    expected_capabilities: Vec<String>,
    acceptance_note: Option<String>,
    evidence: Vec<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct ModelCapabilitiesResponse {
    generated_at: String,
    checked_at: String,
    status: String,
    model_store: ModelStoreStatusResponse,
    runtime_manager: ModelRuntimeManagerResponse,
    capabilities: Vec<ModelCapabilityStatus>,
    blockers: Vec<String>,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct ModelStoreStatusResponse {
    path: String,
    status: String,
    writable: bool,
    runtime_readable: bool,
    next_action: String,
    blockers: Vec<String>,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct ModelCapabilityStatus {
    capability_id: String,
    label: String,
    model_kind: String,
    status: String,
    selected_model_id: Option<String>,
    runtime_model_id: Option<String>,
    current_model: Option<ModelCapabilityCurrentModel>,
    installed_models: Vec<ModelCapabilityInstallableModel>,
    installable_models: Vec<ModelCapabilityInstallableModel>,
    download_jobs: Vec<ModelDownloadJobRecord>,
    next_action: String,
    runtime_ready: bool,
    required_runtime_profile: Option<String>,
    runtime_installed: bool,
    runtime_installable: bool,
    runtime_status: Option<String>,
    runtime_next_action: Option<String>,
    source_of_truth: String,
    evidence: Vec<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct ModelCapabilityCurrentModel {
    model_endpoint_id: String,
    model_name: String,
    provider_key: String,
    status: String,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct ModelCapabilityInstallableModel {
    model_id: String,
    display_name: String,
    provider_key: String,
    model_kind: String,
    status: String,
    installed: bool,
    local_path: Option<String>,
    download_job_id: Option<String>,
    download_size_hint: String,
    hardware_fit: String,
    fit_reason: String,
    recommendation_group: String,
    source_kind: String,
    repo_id: Option<String>,
    file_policy: String,
    runtime_profiles: Vec<String>,
    expected_capabilities: Vec<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct ModelDownloadJobResponse {
    #[serde(flatten)]
    record: ModelDownloadJobRecord,
    job: ModelDownloadJobRecord,
}

impl ModelDownloadJobResponse {
    fn new(job: ModelDownloadJobRecord) -> Self {
        Self {
            record: job.clone(),
            job,
        }
    }
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct ModelDownloadJobsResponse {
    generated_at: String,
    checked_at: String,
    status: String,
    jobs: Vec<ModelDownloadJobRecord>,
    downloads: Vec<ModelDownloadJobRecord>,
    blockers: Vec<String>,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct ModelRuntimeManagerResponse {
    generated_at: String,
    checked_at: String,
    status: String,
    runtimes: Vec<ModelRuntimeStatus>,
    blockers: Vec<String>,
    warnings: Vec<String>,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct ModelRuntimeStatus {
    #[serde(flatten)]
    record: ModelRuntimeRecord,
    installed: bool,
    active: bool,
    next_action: String,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct ModelRuntimeInstallResponse {
    runtime: ModelRuntimeStatus,
    runtime_manager: ModelRuntimeManagerResponse,
    message: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct LocalModelRuntimeProjection {
    base_url: String,
    healthz_url: String,
    api_key_configured: bool,
    ready: bool,
    backend_ready: bool,
    backend_kind: Option<String>,
    chat_model: Option<String>,
    embedding_model: Option<String>,
    note: Option<String>,
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ModelPoliciesRequest {
    #[serde(default)]
    route_policies: Vec<ModelRoutePolicy>,
}

#[derive(Debug, Deserialize, Default)]
struct ModelDownloadRequest {
    model_id: String,
    #[serde(default)]
    capability_id: Option<String>,
    #[serde(default)]
    display_name: Option<String>,
    #[serde(default)]
    provider_key: Option<String>,
    #[serde(default)]
    target_path: Option<String>,
    #[serde(default)]
    hf_endpoint: Option<String>,
    #[serde(default)]
    metadata: Value,
}

#[derive(Debug, Deserialize)]
struct ModelStoreUpdateRequest {
    path: String,
}

#[derive(Debug, Deserialize)]
struct ModelCapabilitySelectionRequest {
    model_id: String,
}

#[derive(Debug, Serialize)]
struct CameraTaskResponse {
    task_response: TaskResponse,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    media_item: Option<DvrTimelineSegment>,
}

#[derive(Debug, Serialize)]
struct ApprovalDecisionResponse {
    approval: TaskApprovalSummary,
    #[serde(default)]
    task_response: Option<TaskResponse>,
}

#[derive(Debug, Serialize)]
struct AdminStateResponse {
    #[serde(flatten)]
    state: StateResponse,
    account_management: AccountManagementSnapshot,
    #[serde(default)]
    device_credential_statuses: Vec<DeviceCredentialStatusResponse>,
}

#[derive(Debug, Serialize)]
struct AccessMemberSummary {
    user_id: String,
    display_name: String,
    role_kind: String,
    membership_status: String,
    source: String,
    #[serde(default)]
    open_id: Option<String>,
    #[serde(default)]
    chat_id: Option<String>,
    can_edit: bool,
    is_owner: bool,
    #[serde(default)]
    proactive_delivery_surface: String,
    #[serde(default)]
    proactive_delivery_default: bool,
    #[serde(default)]
    binding_availability: String,
    #[serde(default)]
    binding_available: bool,
    #[serde(default)]
    binding_availability_note: String,
    #[serde(default)]
    recent_interactive_surface: Option<String>,
}

#[derive(Debug, Serialize)]
struct ShareLinkSummary {
    share_link_id: String,
    media_session_id: String,
    device_id: String,
    device_name: String,
    #[serde(default)]
    opened_by_user_id: Option<String>,
    access_scope: String,
    session_status: String,
    status: String,
    #[serde(default)]
    expires_at: Option<String>,
    #[serde(default)]
    revoked_at: Option<String>,
    #[serde(default)]
    started_at: Option<String>,
    #[serde(default)]
    ended_at: Option<String>,
    can_revoke: bool,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct DeviceCredentialStatusResponse {
    device_id: String,
    configured: bool,
    redacted: bool,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    rtsp_port: Option<u16>,
    path_count: usize,
    source: String,
    #[serde(default)]
    updated_at: Option<String>,
    #[serde(default)]
    last_verified_at: Option<String>,
}

#[derive(Debug, Serialize, Clone)]
struct RtspCheckResponse {
    device_id: String,
    reachable: bool,
    #[serde(default)]
    stream_url: Option<String>,
    transport: String,
    requires_auth: bool,
    capabilities: CameraCapabilities,
    #[serde(default)]
    error_message: Option<String>,
    checked_at: String,
}

#[derive(Debug, Serialize)]
struct DeviceEvidenceResponse {
    device_id: String,
    generated_at: String,
    credential_status: DeviceCredentialStatusResponse,
    #[serde(default)]
    recent_rtsp_check: Option<DeviceEvidenceRecord>,
    #[serde(default)]
    recent_snapshot_check: Option<DeviceEvidenceRecord>,
    #[serde(default)]
    share_links: Vec<ShareLinkSummary>,
    #[serde(default)]
    evidence: Vec<DeviceEvidenceRecord>,
}

#[derive(Debug, Serialize)]
struct DeviceValidationRunResponse {
    validation_id: String,
    device_id: String,
    status: String,
    rtsp_check: DeviceEvidenceRecord,
    snapshot_check: DeviceEvidenceRecord,
    evidence: DeviceEvidenceResponse,
}

type StateResponse = HubStateSnapshot;
type ScanRequest = HubScanRequest;
type ScanResponse = HubScanSummary;
type ManualAddResponse = HubManualAddSummary;

impl AdminApi {
    pub fn new(
        admin_store: AdminConsoleStore,
        task_service: TaskApiService,
        harbor_assistant_dist: PathBuf,
        public_origin: String,
    ) -> Self {
        let (guardian_sender, guardian_receiver) =
            sync_channel(HOME_GUARDIAN_EVALUATION_QUEUE_CAPACITY);
        let api = Self {
            admin_store,
            task_service,
            dvr_runtime: DvrRuntime::default(),
            hls_live_runtime: HlsLiveRuntime::default(),
            harbor_assistant_dist,
            public_origin,
            model_runtime_activation: None,
            last_event_notification_attempt: Arc::new(Mutex::new(None)),
            last_home_assistant_service_action: Arc::new(Mutex::new(None)),
            last_evt_preflight: Arc::new(Mutex::new(None)),
            last_vlm_enrichment: Arc::new(Mutex::new(None)),
            last_family_timeline_digest: Arc::new(Mutex::new(None)),
            last_family_memory_feedback: Arc::new(Mutex::new(None)),
            last_guardian_rule_proposal: Arc::new(Mutex::new(None)),
            last_guardian_rule_evaluation: Arc::new(Mutex::new(None)),
            guardian_queue: HomeGuardianEvaluationQueue::new(guardian_sender),
            guardian_auto_eval_cooldowns: Arc::new(Mutex::new(HashMap::new())),
            guardian_cooldowns: Arc::new(Mutex::new(HashMap::new())),
            guardian_action_runs: Arc::new(Mutex::new(HashMap::new())),
            guardian_rule_cache: Arc::new(Mutex::new(HomeGuardianRuleCache::default())),
            guardian_activity_log_lock: Arc::new(Mutex::new(())),
            http_runtime: HttpRuntimeCounters::new(),
            vision_ingest_limiter: VisionIngestLimiter::new(),
        };
        api.refresh_home_guardian_rule_cache_best_effort();
        api.start_home_guardian_worker(guardian_receiver);
        api
    }

    pub(crate) fn with_model_runtime_activation_handler(
        mut self,
        handler: ModelRuntimeActivationHandler,
    ) -> Self {
        self.model_runtime_activation = Some(handler);
        self
    }

    fn hub(&self) -> CameraHubService {
        CameraHubService::new(self.admin_store.clone())
    }

    fn start_home_guardian_worker(&self, receiver: Receiver<StoredLocalVisionEvent>) {
        let worker = self.clone();
        let _ = thread::Builder::new()
            .name("home-guardian-eval".to_string())
            .spawn(move || worker.run_home_guardian_worker(receiver));
    }

    fn run_home_guardian_worker(self, receiver: Receiver<StoredLocalVisionEvent>) {
        for stored in receiver {
            match self.evaluate_home_guardian_rules_for_event(&stored, None, true) {
                Ok(evaluation) => {
                    self.guardian_queue.record_processed();
                    self.record_last_guardian_rule_evaluation(
                        &serde_json::to_value(&evaluation).unwrap_or_else(|_| json!({})),
                    );
                }
                Err(error) => {
                    self.guardian_queue.record_failed();
                    self.record_last_guardian_rule_evaluation(&json!({
                        "status": "failed",
                        "event_id": stored.event.event_id,
                        "reason": redact_admin_string(&error),
                        "metadata_only": true,
                        "secret_scan": "clean",
                    }));
                }
            }
        }
    }

    fn enqueue_home_guardian_evaluation(&self, stored: &StoredLocalVisionEvent) -> &'static str {
        let now_epoch = current_epoch_secs();
        if !self.guardian_auto_evaluation_due(stored, now_epoch) {
            self.guardian_queue.record_coalesced();
            return "coalesced";
        }
        self.guardian_queue.enqueue(stored.clone())
    }

    fn home_guardian_queue_snapshot(&self) -> Value {
        self.guardian_queue.snapshot()
    }

    fn refresh_home_guardian_rule_cache_best_effort(&self) {
        if let Err(error) = self.refresh_home_guardian_rule_cache() {
            if let Ok(mut guard) = self.guardian_rule_cache.lock() {
                guard.refresh_error = Some(redact_admin_string(&error));
            }
        }
    }

    fn refresh_home_guardian_rule_cache(&self) -> Result<(), String> {
        let state = self.admin_store.load_state()?;
        let active_reviews = state
            .automation_reviews
            .into_iter()
            .filter(|review| {
                automation_review_is_home_guardian(review) && review.status.as_str() == "active"
            })
            .collect::<Vec<_>>();
        let (file_bytes, modified_epoch_ms) = admin_state_file_fingerprint(self.admin_store.path());
        let mut guard = self
            .guardian_rule_cache
            .lock()
            .map_err(|_| "home guardian rule cache lock poisoned".to_string())?;
        *guard = HomeGuardianRuleCache {
            active_reviews,
            loaded_at_epoch: current_epoch_secs(),
            file_bytes,
            modified_epoch_ms,
            refresh_error: None,
        };
        Ok(())
    }

    fn cached_home_guardian_reviews_for_auto(&self) -> Result<Vec<AutomationRuleReview>, String> {
        let (file_bytes, modified_epoch_ms) = admin_state_file_fingerprint(self.admin_store.path());
        let needs_refresh = self
            .guardian_rule_cache
            .lock()
            .map(|guard| {
                guard.loaded_at_epoch == 0
                    || guard.file_bytes != file_bytes
                    || guard.modified_epoch_ms != modified_epoch_ms
            })
            .unwrap_or(true);
        if needs_refresh {
            self.refresh_home_guardian_rule_cache()?;
        }
        let guard = self
            .guardian_rule_cache
            .lock()
            .map_err(|_| "home guardian rule cache lock poisoned".to_string())?;
        if let Some(error) = guard.refresh_error.as_ref() {
            return Err(error.clone());
        }
        Ok(guard.active_reviews.clone())
    }

    fn guardian_rule_cache_snapshot(&self) -> Value {
        match self.guardian_rule_cache.lock() {
            Ok(guard) => json!({
                "active_rule_count": guard.active_reviews.len(),
                "loaded_at_epoch": guard.loaded_at_epoch,
                "file_bytes": guard.file_bytes,
                "modified_epoch_ms": guard.modified_epoch_ms.map(|value| value.to_string()),
                "refresh_error": guard.refresh_error.as_deref().map(redact_admin_string),
                "metadata_only": true,
            }),
            Err(_) => json!({
                "status": "unavailable",
                "reason": "home guardian rule cache lock poisoned",
                "metadata_only": true,
            }),
        }
    }

    fn guardian_action_run_seen(&self, idempotency_key: &str, now_epoch: u64) -> bool {
        let Ok(mut guard) = self.guardian_action_runs.lock() else {
            return false;
        };
        guard.retain(|_, last_epoch| {
            now_epoch.saturating_sub(*last_epoch) <= HOME_GUARDIAN_IDEMPOTENCY_MEMORY_SECONDS
        });
        guard.contains_key(idempotency_key)
    }

    fn mark_guardian_action_run_seen(&self, idempotency_key: &str, now_epoch: u64) {
        if let Ok(mut guard) = self.guardian_action_runs.lock() {
            guard.insert(idempotency_key.to_string(), now_epoch);
        }
    }

    fn record_http_request_accepted(&self) {
        self.http_runtime.record_accepted();
    }

    fn record_http_request_rejected(&self) {
        self.http_runtime.record_rejected();
    }

    fn begin_http_request(&self) -> HttpRequestGuard {
        self.http_runtime.begin_request()
    }

    fn runtime_backpressure_snapshot(&self) -> Value {
        json!({
            "http": self.http_runtime.snapshot(),
            "vision_ingest": self.vision_ingest_limiter.snapshot(),
            "vlm": vlm_execution_runtime_snapshot(),
            "home_guardian": self.home_guardian_queue_snapshot(),
            "home_guardian_rule_cache": self.guardian_rule_cache_snapshot(),
            "metadata_only": true,
        })
    }

    fn home_guardian_activity_log_path(&self) -> PathBuf {
        self.admin_store
            .path()
            .with_file_name("home_guardian_activity.jsonl")
    }

    fn load_home_guardian_activity_summaries(&self) -> Vec<Value> {
        let path = self.home_guardian_activity_log_path();
        let Ok(text) = fs::read_to_string(path) else {
            return Vec::new();
        };
        let summaries = text
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line.trim()).ok())
            .collect::<Vec<_>>();
        latest_home_guardian_activity_summaries(summaries)
    }

    fn append_home_guardian_activity_summary(&self, summary: &Value) {
        let Ok(_guard) = self.guardian_activity_log_lock.lock() else {
            return;
        };
        let path = self.home_guardian_activity_log_path();
        if let Some(parent) = path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let mut summaries = self.load_home_guardian_activity_summaries();
        summaries.push(summary.clone());
        summaries = latest_home_guardian_activity_summaries(summaries);
        let payload = summaries
            .into_iter()
            .filter_map(|summary| serde_json::to_string(&summary).ok())
            .collect::<Vec<_>>()
            .join("\n");
        let _ = fs::write(path, format!("{payload}\n"));
    }

    fn guardian_action_in_cooldown(
        &self,
        review: &AutomationRuleReview,
        stored: &StoredLocalVisionEvent,
        action_key: &str,
        now_epoch: u64,
    ) -> bool {
        let key = home_guardian_action_cooldown_key(review, stored, action_key);
        let Ok(mut guard) = self.guardian_cooldowns.lock() else {
            return false;
        };
        guard.retain(|_, last_epoch| {
            now_epoch.saturating_sub(*last_epoch) <= HOME_GUARDIAN_ACTION_COOLDOWN_SECONDS * 2
        });
        guard.get(&key).is_some_and(|last_epoch| {
            now_epoch.saturating_sub(*last_epoch) < HOME_GUARDIAN_ACTION_COOLDOWN_SECONDS
        })
    }

    fn mark_guardian_action_cooldown(
        &self,
        review: &AutomationRuleReview,
        stored: &StoredLocalVisionEvent,
        action_key: &str,
        now_epoch: u64,
    ) {
        let key = home_guardian_action_cooldown_key(review, stored, action_key);
        if let Ok(mut guard) = self.guardian_cooldowns.lock() {
            guard.insert(key, now_epoch);
        }
    }

    fn guardian_auto_evaluation_due(
        &self,
        stored: &StoredLocalVisionEvent,
        now_epoch: u64,
    ) -> bool {
        let key = home_guardian_auto_evaluation_cooldown_key(stored);
        let Ok(mut guard) = self.guardian_auto_eval_cooldowns.lock() else {
            return true;
        };
        guard.retain(|_, last_epoch| {
            now_epoch.saturating_sub(*last_epoch)
                <= HOME_GUARDIAN_AUTO_EVALUATION_MIN_INTERVAL_SECONDS * 2
        });
        if guard.get(&key).is_some_and(|last_epoch| {
            now_epoch.saturating_sub(*last_epoch)
                < HOME_GUARDIAN_AUTO_EVALUATION_MIN_INTERVAL_SECONDS
        }) {
            return false;
        }
        guard.insert(key, now_epoch);
        true
    }

    fn record_last_event_notification_attempt(
        &self,
        response: &LocalVisionEventNotificationResponse,
    ) {
        let Ok(mut guard) = self.last_event_notification_attempt.lock() else {
            return;
        };
        *guard = Some(serde_json::to_value(response).unwrap_or_else(|_| json!({})));
    }

    fn record_last_home_assistant_service_action(
        &self,
        response: &HomeAssistantServiceActionResponse,
    ) {
        let Ok(mut guard) = self.last_home_assistant_service_action.lock() else {
            return;
        };
        *guard = Some(serde_json::to_value(response).unwrap_or_else(|_| json!({})));
    }

    fn last_event_notification_attempt(&self) -> Option<Value> {
        self.last_event_notification_attempt
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    }

    fn last_home_assistant_service_action(&self) -> Option<Value> {
        self.last_home_assistant_service_action
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
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

    fn record_last_vlm_enrichment(&self, evidence: Value) {
        let Ok(mut guard) = self.last_vlm_enrichment.lock() else {
            return;
        };
        *guard = Some(evidence);
    }

    fn last_vlm_enrichment(&self) -> Option<Value> {
        self.last_vlm_enrichment
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    }

    fn record_last_family_timeline_digest(&self, digest: &FamilyTimelineDigest) {
        let Ok(mut guard) = self.last_family_timeline_digest.lock() else {
            return;
        };
        *guard = Some(serde_json::to_value(digest).unwrap_or_else(|_| json!({})));
    }

    fn record_last_family_memory_feedback(&self, record: &FamilyMemoryFeedbackRecord) {
        let Ok(mut guard) = self.last_family_memory_feedback.lock() else {
            return;
        };
        *guard = Some(compact_family_memory_feedback_evidence(record));
    }

    fn record_last_guardian_rule_proposal(&self, proposal: &Value) {
        let Ok(mut guard) = self.last_guardian_rule_proposal.lock() else {
            return;
        };
        *guard = Some(compact_home_guardian_proposal_evidence(proposal));
    }

    fn record_last_guardian_rule_evaluation(&self, evaluation: &Value) {
        let Ok(mut guard) = self.last_guardian_rule_evaluation.lock() else {
            return;
        };
        *guard = Some(compact_home_guardian_evaluation_evidence(evaluation));
    }

    fn last_family_timeline_digest(&self) -> Option<Value> {
        self.last_family_timeline_digest
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    }

    fn last_family_memory_feedback(&self) -> Option<Value> {
        self.last_family_memory_feedback
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    }

    fn last_guardian_rule_proposal(&self) -> Option<Value> {
        self.last_guardian_rule_proposal
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    }

    fn last_guardian_rule_evaluation(&self) -> Option<Value> {
        self.last_guardian_rule_evaluation
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    }

    fn record_admin_audit(
        &self,
        principal: &AccessPrincipal,
        entity_kind: &str,
        entity_id: &str,
        action: &str,
        request_snapshot: Value,
        result_snapshot: Value,
    ) {
        let actor_kind = if principal.user_id == "system" {
            AuditActorKind::System
        } else {
            AuditActorKind::User
        };
        let record = build_metadata_audit_record(
            principal.workspace_id.clone(),
            entity_kind,
            entity_id,
            action,
            actor_kind,
            principal.user_id.clone(),
            request_snapshot,
            result_snapshot,
        );
        let _ = self.admin_store.append_audit_record(record);
    }

    fn record_local_vision_notify_audit(
        &self,
        principal: &AccessPrincipal,
        response: &LocalVisionEventNotificationResponse,
    ) {
        self.record_admin_audit(
            principal,
            "vision_event",
            &response.event_id,
            "vision_event.notify",
            json!({"event_id": response.event_id}),
            json!({
                "status": response.status,
                "notification_id": response.notification_id,
                "delivery_id": response.delivery_id,
                "target_bound": response.target_label.is_some(),
            }),
        );
    }

    fn authorize_admin_action(
        &self,
        hints: &AccessIdentityHints,
        action: AccessAction,
    ) -> Result<AccessPrincipal, String> {
        let state = self.admin_store.load_state()?;
        let workspace_id = state
            .platform
            .workspaces
            .first()
            .map(|workspace| workspace.workspace_id.clone())
            .unwrap_or_else(|| "home-1".to_string());
        authorize_access(
            &state,
            hints,
            action,
            &format!("workspace:{workspace_id}"),
            true,
        )
    }

    fn authorize_workspace_camera_action(
        &self,
        hints: &AccessIdentityHints,
    ) -> Result<AccessPrincipal, String> {
        self.authorize_admin_action(hints, AccessAction::CameraOperate)
    }

    fn authorize_camera_action(
        &self,
        hints: &AccessIdentityHints,
        device_id: &str,
        action: AccessAction,
    ) -> Result<AccessPrincipal, String> {
        let state = self.admin_store.load_state()?;
        authorize_access(&state, hints, action, &format!("camera:{device_id}"), true)
    }

    fn handle_harbor_assistant(
        &self,
        path: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }

        let dist_root = resolve_state_path(&self.harbor_assistant_dist);
        if !dist_root.exists() {
            return harbor_assistant_build_missing_response(&dist_root);
        }

        if let Some(asset_path) = resolve_harbor_assistant_asset_path(&dist_root, path) {
            if asset_path.is_file() {
                return static_file_response(&asset_path);
            }
        }

        if is_harbor_assistant_client_route(path) {
            let index_path = dist_root.join("index.html");
            if index_path.is_file() {
                return static_file_response(&index_path);
            }
            return harbor_assistant_build_missing_response(&dist_root);
        }

        error_json(StatusCode(404), "route not found")
    }

    pub fn handle(&self, mut request: Request) {
        let method = request.method().clone();
        let raw_url = normalize_unified_admin_url(&request.url().to_string());
        let path = raw_url.split('?').next().unwrap_or("/").to_string();
        let remote_addr = request.remote_addr().copied();
        let headers = request.headers().to_vec();
        let identity_hints = request_identity_hints(&raw_url, &headers);

        if is_admin_surface_path(path.as_str()) || is_harbor_assistant_surface_path(path.as_str()) {
            if let Err(error) = ensure_local_admin_access(remote_addr, &headers) {
                let _ = request.respond(error_json(StatusCode(403), &error).boxed());
                return;
            }
        }

        let response = match method {
            Method::Get if path == "/healthz" => ok_json(&json!({"status":"ok"})).boxed(),
            Method::Get if path == "/api/state" => self.handle_state(&identity_hints).boxed(),
            Method::Get if path == "/api/account-management" => {
                self.handle_account_management(&identity_hints).boxed()
            }
            Method::Get if path == "/api/gateway/status" => {
                self.handle_gateway_status(&identity_hints).boxed()
            }
            Method::Get if path == "/api/release/readiness" => {
                self.handle_release_readiness(&identity_hints).boxed()
            }
            Method::Get if path == "/api/release/readiness/history" => self
                .handle_release_readiness_history(&identity_hints)
                .boxed(),
            Method::Get if path == "/api/hardware/readiness" => {
                self.handle_hardware_readiness(&identity_hints).boxed()
            }
            Method::Get if path == "/api/rag/readiness" => {
                self.handle_rag_readiness(&identity_hints).boxed()
            }
            Method::Get if path == "/api/evt/readiness" => {
                self.handle_evt_readiness(&identity_hints).boxed()
            }
            Method::Post if path == "/api/evt/preflight" => {
                self.handle_run_evt_preflight(&identity_hints).boxed()
            }
            Method::Get if path == "/api/evt/preflight/latest" => {
                self.handle_latest_evt_preflight(&identity_hints).boxed()
            }
            Method::Get if path == "/api/evt/evidence-bundle" => {
                self.handle_evt_evidence_bundle(&identity_hints).boxed()
            }
            Method::Get if path == "/api/diagnostics/redacted-bundle" => self
                .handle_redacted_diagnostics_bundle(&identity_hints)
                .boxed(),
            Method::Get if path == "/api/routing/status" => {
                self.handle_routing_status(&identity_hints).boxed()
            }
            Method::Get if path == "/api/audit/records" => {
                self.handle_audit_records(&raw_url, &identity_hints).boxed()
            }
            Method::Get if path == "/api/audit/summary" => {
                self.handle_audit_summary(&raw_url, &identity_hints).boxed()
            }
            Method::Get if path == "/api/knowledge/settings" => {
                self.handle_knowledge_settings(&identity_hints).boxed()
            }
            Method::Put if path == "/api/knowledge/settings" => self
                .handle_save_knowledge_settings(&mut request, &identity_hints)
                .boxed(),
            Method::Post if path == "/api/knowledge/search" => self
                .handle_knowledge_search(&mut request, &identity_hints)
                .boxed(),
            Method::Get if path == "/api/knowledge/preview" => self
                .handle_knowledge_preview(&raw_url, &identity_hints)
                .boxed(),
            Method::Post if path == "/api/knowledge/index/run" => {
                self.handle_run_knowledge_index(&identity_hints).boxed()
            }
            Method::Get if path == "/api/knowledge/index/status" => {
                self.handle_knowledge_index_status(&identity_hints).boxed()
            }
            Method::Get if path == "/api/knowledge/index/jobs" => {
                self.handle_knowledge_index_jobs(&identity_hints).boxed()
            }
            Method::Post
                if path.starts_with("/api/knowledge/index/jobs/") && path.ends_with("/cancel") =>
            {
                self.handle_cancel_knowledge_index_job(&path, &identity_hints)
                    .boxed()
            }
            Method::Get if path == "/api/files/browse" => {
                self.handle_files_browse(&raw_url, &identity_hints).boxed()
            }
            Method::Get if path == "/api/cameras/recording-settings" => {
                self.handle_dvr_recording_settings(&identity_hints).boxed()
            }
            Method::Put if path == "/api/cameras/recording-settings" => self
                .handle_save_dvr_recording_settings(&mut request, &identity_hints)
                .boxed(),
            Method::Get if path == "/api/cameras/recordings/status" => {
                self.handle_dvr_recordings_status(&identity_hints).boxed()
            }
            Method::Get if path == "/api/cameras/recordings/timeline" => self
                .handle_dvr_recordings_timeline(&raw_url, &identity_hints)
                .boxed(),
            Method::Get if path == "/api/harboros/status" => {
                self.handle_harboros_status(&identity_hints).boxed()
            }
            Method::Get if path == "/api/harboros/im-capability-map" => self
                .handle_harboros_im_capability_map(&identity_hints)
                .boxed(),
            Method::Get if path == "/api/home-assistant/status" => {
                self.handle_home_assistant_status(&identity_hints).boxed()
            }
            Method::Put if path == "/api/home-assistant/config" => self
                .handle_save_home_assistant_config(&mut request, &identity_hints)
                .boxed(),
            Method::Post if path == "/api/home-assistant/test" => {
                self.handle_test_home_assistant(&identity_hints).boxed()
            }
            Method::Post if path == "/api/home-assistant/sync" => {
                self.handle_sync_home_assistant(&identity_hints).boxed()
            }
            Method::Get if path == "/api/home-assistant/entities" => {
                self.handle_home_assistant_entities(&identity_hints).boxed()
            }
            Method::Get if path == "/api/home-assistant/services" => {
                self.handle_home_assistant_services(&identity_hints).boxed()
            }
            Method::Post if path == "/api/home-assistant/service-smoke" => self
                .handle_home_assistant_service_smoke(&mut request, &identity_hints)
                .boxed(),
            Method::Post if path == "/api/home-assistant/service-action" => self
                .handle_home_assistant_service_action(&mut request, &identity_hints)
                .boxed(),
            Method::Get if path == "/api/harboros/apps/home-assistant/status" => self
                .handle_home_assistant_install_status(&identity_hints)
                .boxed(),
            Method::Post if path == "/api/harboros/apps/home-assistant/install-plan" => self
                .handle_home_assistant_install_plan(&identity_hints)
                .boxed(),
            Method::Post if path == "/api/harboros/apps/home-assistant/install" => self
                .handle_home_assistant_install(&mut request, &identity_hints)
                .boxed(),
            Method::Get if path == "/api/models/endpoints" => {
                self.handle_model_endpoints(&identity_hints).boxed()
            }
            Method::Get if path == "/api/models/capabilities" => {
                self.handle_model_capabilities(&identity_hints).boxed()
            }
            Method::Get if path == "/api/models/runtimes" => {
                self.handle_model_runtimes(&identity_hints).boxed()
            }
            Method::Get if path == "/api/inference/healthz" => {
                self.handle_inference_health_alias(&identity_hints).boxed()
            }
            Method::Get if path == "/api/models/store" => {
                self.handle_model_store(&identity_hints).boxed()
            }
            Method::Get if path == "/api/models/local-catalog" => {
                self.handle_local_model_catalog(&identity_hints).boxed()
            }
            Method::Get if path == "/api/models/local-downloads" => {
                self.handle_model_download_jobs(&identity_hints).boxed()
            }
            Method::Get if path.starts_with("/api/models/local-downloads/") => self
                .handle_model_download_job(&path, &identity_hints)
                .boxed(),
            Method::Get if path == "/api/feature-availability" => {
                self.handle_feature_availability(&identity_hints).boxed()
            }
            Method::Get if path == "/api/vision/events" => self
                .handle_list_local_vision_events(&raw_url, &identity_hints)
                .boxed(),
            Method::Get if path == "/api/vision/vlm/status" => {
                self.handle_vision_vlm_status(&identity_hints).boxed()
            }
            Method::Post if path == "/api/vision/events" => self
                .handle_ingest_local_vision_event(&mut request, &identity_hints)
                .boxed(),
            Method::Post if path == "/api/vision/events/latest/vlm-enrich" => self
                .handle_vlm_enrich_latest_local_vision_event(&identity_hints)
                .boxed(),
            Method::Post
                if path.starts_with("/api/vision/events/") && path.ends_with("/vlm-enrich") =>
            {
                self.handle_vlm_enrich_local_vision_event(&path, &identity_hints)
                    .boxed()
            }
            Method::Post
                if path.starts_with("/api/vision/events/") && path.ends_with("/notify") =>
            {
                self.handle_notify_local_vision_event(&path, &identity_hints)
                    .boxed()
            }
            Method::Get if path == "/api/family/timeline" => self
                .handle_family_timeline(&raw_url, &identity_hints)
                .boxed(),
            Method::Get if path == "/api/family/timeline/digest" => self
                .handle_family_timeline_digest(&raw_url, &identity_hints)
                .boxed(),
            Method::Get if path == "/api/family/memory/events" => self
                .handle_family_memory_events(&raw_url, &identity_hints)
                .boxed(),
            Method::Get if path == "/api/family/memory/stats" => {
                self.handle_family_memory_stats(&identity_hints).boxed()
            }
            Method::Get if path.starts_with("/api/family/memory/events/") => self
                .handle_family_memory_event(&path, &identity_hints)
                .boxed(),
            Method::Post
                if path.starts_with("/api/family/memory/events/")
                    && path.ends_with("/feedback") =>
            {
                self.handle_family_memory_feedback(&path, &mut request, &identity_hints)
                    .boxed()
            }
            Method::Get if path == "/api/home-guardian/activity" => {
                self.handle_home_guardian_activity(&identity_hints).boxed()
            }
            Method::Post
                if path.starts_with("/api/automation/reviews/")
                    && path.ends_with("/evaluate-latest") =>
            {
                self.handle_evaluate_automation_review_latest(&path, &identity_hints)
                    .boxed()
            }
            Method::Get if path == "/api/models/policies" => {
                self.handle_model_policies(&identity_hints).boxed()
            }
            Method::Get if path == "/admin/models" => {
                self.handle_models_page(&identity_hints).boxed()
            }
            Method::Get if path == "/api/access/members" => {
                self.handle_access_members(&identity_hints).boxed()
            }
            Method::Get if path == "/api/share-links" => {
                self.handle_share_links(&raw_url, &identity_hints).boxed()
            }
            Method::Get if path.starts_with("/api/devices/") && path.ends_with("/evidence") => {
                self.handle_device_evidence(&path, &identity_hints).boxed()
            }
            Method::Get
                if path.starts_with("/api/devices/") && path.ends_with("/credential-status") =>
            {
                self.handle_device_credential_status(&path, &identity_hints)
                    .boxed()
            }
            Method::Get if path == "/api/tasks/approvals" => {
                self.handle_pending_approvals(&identity_hints).boxed()
            }
            Method::Get if path == "/api/automation/reviews" => {
                self.handle_automation_reviews(&identity_hints).boxed()
            }
            Method::Get if path == "/api/admin/notification-targets" => {
                self.handle_notification_targets(&identity_hints).boxed()
            }
            Method::Get if path == "/api/binding/qr.svg" => {
                self.handle_binding_qr_svg(&identity_hints).boxed()
            }
            Method::Get if path == "/api/binding/static-qr.svg" => {
                self.handle_static_binding_qr_svg(&identity_hints).boxed()
            }
            Method::Get if path == "/setup/mobile" => self
                .handle_mobile_setup_page(&raw_url, &identity_hints)
                .boxed(),
            Method::Get
                if path.starts_with("/shared/cameras/") && path.ends_with("/live.mjpeg") =>
            {
                self.handle_shared_camera_live_mjpeg(&path)
            }
            Method::Get if path.starts_with("/shared/cameras/") => {
                self.handle_shared_live_view_page(&path).boxed()
            }
            Method::Get if path.starts_with("/live/cameras/") => self
                .handle_live_view_page(&raw_url, &path, remote_addr, &headers, &identity_hints)
                .boxed(),
            Method::Post if path.starts_with("/api/cameras/") && path.ends_with("/live/start") => {
                self.handle_camera_hls_live_start(&path, remote_addr, &headers, &identity_hints)
                    .boxed()
            }
            Method::Post if path.starts_with("/api/cameras/") && path.ends_with("/live/stop") => {
                self.handle_camera_hls_live_stop(
                    &path,
                    &mut request,
                    remote_addr,
                    &headers,
                    &identity_hints,
                )
                .boxed()
            }
            Method::Get if path.starts_with("/api/cameras/") && path.ends_with("/live/status") => {
                self.handle_camera_hls_live_status(
                    &raw_url,
                    &path,
                    remote_addr,
                    &headers,
                    &identity_hints,
                )
                .boxed()
            }
            Method::Get if parse_camera_hls_live_asset_path(&path).is_some() => {
                self.handle_camera_hls_live_asset(&path, remote_addr, &headers, &identity_hints)
            }
            Method::Get if path.starts_with("/api/cameras/") && path.ends_with("/live.mjpeg") => {
                self.handle_camera_live_mjpeg(&path, remote_addr, &headers, &identity_hints)
            }
            Method::Get if path.starts_with("/api/cameras/") && path.ends_with("/snapshot.jpg") => {
                self.handle_camera_snapshot(&path, remote_addr, &headers, &identity_hints)
                    .boxed()
            }
            Method::Post if path == "/api/binding/refresh" => {
                self.handle_refresh_binding(&identity_hints).boxed()
            }
            Method::Post if path == "/api/binding/demo-bind" => {
                self.handle_demo_bind(&identity_hints).boxed()
            }
            Method::Post if path == "/api/binding/test-bind" => {
                self.handle_test_bind(&mut request, &identity_hints).boxed()
            }
            Method::Post if path == "/api/release/readiness/run" => {
                self.handle_run_release_readiness(&identity_hints).boxed()
            }
            Method::Post if path == "/api/admin/notification-targets" => self
                .handle_upsert_notification_target(&mut request, &headers)
                .boxed(),
            Method::Post if path == "/api/admin/notification-targets/default" => self
                .handle_set_default_notification_target(&mut request, &identity_hints)
                .boxed(),
            Method::Post if path == "/api/automation/reviews" => self
                .handle_create_automation_review(&mut request, &identity_hints)
                .boxed(),
            Method::Post if path == "/api/models/endpoints" => self
                .handle_create_model_endpoint(&mut request, &identity_hints)
                .boxed(),
            Method::Post if path == "/api/models/local-downloads" => self
                .handle_create_model_download(&mut request, &identity_hints)
                .boxed(),
            Method::Post
                if path.starts_with("/api/models/runtimes/") && path.ends_with("/install") =>
            {
                self.handle_install_model_runtime(path.as_str(), &identity_hints)
                    .boxed()
            }
            Method::Put if path == "/api/models/store" => self
                .handle_update_model_store(&mut request, &identity_hints)
                .boxed(),
            Method::Post
                if path.starts_with("/api/models/capabilities/")
                    && path.ends_with("/selection") =>
            {
                self.handle_select_model_capability(&path, &mut request, &identity_hints)
                    .boxed()
            }
            Method::Post
                if path.starts_with("/api/models/local-downloads/")
                    && path.ends_with("/cancel") =>
            {
                self.handle_cancel_model_download(&path, &identity_hints)
                    .boxed()
            }
            Method::Post
                if path.starts_with("/api/models/endpoints/") && path.ends_with("/test") =>
            {
                self.handle_test_model_endpoint(path.as_str(), &identity_hints)
                    .boxed()
            }
            Method::Post if path == "/api/bridge/configure" => self
                .handle_configure_bridge(&mut request, &identity_hints)
                .boxed(),
            Method::Patch if path.starts_with("/api/models/endpoints/") => self
                .handle_patch_model_endpoint(path.as_str(), &mut request, &identity_hints)
                .boxed(),
            Method::Patch if path.starts_with("/api/devices/") => self
                .handle_patch_device_metadata(path.as_str(), &mut request, &identity_hints)
                .boxed(),
            Method::Delete if path.starts_with("/api/devices/") => self
                .handle_delete_device(path.as_str(), &identity_hints)
                .boxed(),
            Method::Post
                if path.starts_with("/api/tasks/approvals/") && path.ends_with("/approve") =>
            {
                self.handle_approve_approval(path.as_str(), &mut request, &identity_hints)
                    .boxed()
            }
            Method::Post if path.starts_with("/api/access/members/") && path.ends_with("/role") => {
                self.handle_update_member_role(path.as_str(), &mut request, &identity_hints)
                    .boxed()
            }
            Method::Post
                if path.starts_with("/api/access/members/")
                    && path.ends_with("/default-delivery-surface") =>
            {
                self.handle_update_member_default_delivery_surface(
                    path.as_str(),
                    &mut request,
                    &identity_hints,
                )
                .boxed()
            }
            Method::Post
                if path.starts_with("/api/tasks/approvals/") && path.ends_with("/reject") =>
            {
                self.handle_reject_approval(path.as_str(), &mut request, &identity_hints)
                    .boxed()
            }
            Method::Post
                if path.starts_with("/api/automation/reviews/") && path.ends_with("/enable") =>
            {
                self.handle_update_automation_review_status(
                    path.as_str(),
                    "active",
                    &identity_hints,
                )
                .boxed()
            }
            Method::Post
                if path.starts_with("/api/automation/reviews/") && path.ends_with("/pause") =>
            {
                self.handle_update_automation_review_status(
                    path.as_str(),
                    "paused",
                    &identity_hints,
                )
                .boxed()
            }
            Method::Post
                if path.starts_with("/api/automation/reviews/") && path.ends_with("/discard") =>
            {
                self.handle_update_automation_review_status(
                    path.as_str(),
                    "discarded",
                    &identity_hints,
                )
                .boxed()
            }
            Method::Post if path == "/api/discovery/scan" => {
                self.handle_scan(&mut request, &identity_hints).boxed()
            }
            Method::Post if path == "/api/devices/manual" => self
                .handle_manual_add(&mut request, &identity_hints)
                .boxed(),
            Method::Post if path == "/api/devices/default-camera" => self
                .handle_set_default_camera(&mut request, &identity_hints)
                .boxed(),
            Method::Post if path.starts_with("/api/devices/") && path.ends_with("/credentials") => {
                self.handle_save_device_credentials(&path, &mut request, &identity_hints)
                    .boxed()
            }
            Method::Post if path.starts_with("/api/devices/") && path.ends_with("/rtsp-check") => {
                self.handle_rtsp_check(&path, &mut request, &identity_hints)
                    .boxed()
            }
            Method::Post
                if path.starts_with("/api/devices/") && path.ends_with("/validation/run") =>
            {
                self.handle_device_validation_run(&path, &identity_hints)
                    .boxed()
            }
            Method::Post if path.starts_with("/api/cameras/") && path.ends_with("/share-link") => {
                self.handle_camera_share_link(&path, &identity_hints)
                    .boxed()
            }
            Method::Post
                if path.starts_with("/api/cameras/") && path.ends_with("/recordings/start") =>
            {
                self.handle_dvr_recording_start(&path, &identity_hints)
                    .boxed()
            }
            Method::Post
                if path.starts_with("/api/cameras/") && path.ends_with("/recordings/stop") =>
            {
                self.handle_dvr_recording_stop(&path, &identity_hints)
                    .boxed()
            }
            Method::Post if path.starts_with("/api/share-links/") && path.ends_with("/revoke") => {
                self.handle_revoke_share_link(&path, &identity_hints)
                    .boxed()
            }
            Method::Post if path.starts_with("/api/cameras/") && path.ends_with("/snapshot") => {
                self.handle_camera_task_snapshot(&path, &identity_hints)
                    .boxed()
            }
            Method::Post if path.starts_with("/api/cameras/") && path.ends_with("/analyze") => {
                self.handle_camera_analyze(&path, &identity_hints).boxed()
            }
            Method::Post if path == "/api/defaults" => self
                .handle_save_defaults(&mut request, &identity_hints)
                .boxed(),
            Method::Put if path == "/api/models/policies" => self
                .handle_save_model_policies(&mut request, &identity_hints)
                .boxed(),
            Method::Delete if path.starts_with("/api/admin/notification-targets/") => self
                .handle_delete_notification_target(path.as_str(), &identity_hints)
                .boxed(),
            Method::Get if is_harbor_assistant_surface_path(path.as_str()) => self
                .handle_harbor_assistant(path.as_str(), &identity_hints)
                .boxed(),
            Method::Options => no_content().boxed(),
            _ => error_json(StatusCode(404), "route not found").boxed(),
        };

        let _ = request.respond(response);
    }

    fn handle_state(&self, hints: &AccessIdentityHints) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        self.refresh_gateway_projection_best_effort();
        let live_bridge_provider = fetch_remote_gateway_status()
            .ok()
            .and_then(|payload| live_bridge_provider_from_setup_status(&payload));
        match self.admin_store.load_state() {
            Ok(state) => match self.current_state() {
                Ok(mut payload) => {
                    let mut account_management =
                        account_management_snapshot(&state, Some(&self.public_origin));
                    if let Some(provider) = live_bridge_provider.as_ref() {
                        apply_bridge_provider_projection_to_state(&mut payload, provider);
                        apply_bridge_provider_projection_to_gateway_summary(
                            &mut account_management.gateway,
                            provider,
                        );
                    }
                    let device_credential_statuses =
                        build_device_credential_statuses(&state, &payload.devices);
                    ok_json(&AdminStateResponse {
                        state: redact_state_snapshot(payload),
                        account_management: redact_account_management_snapshot(account_management),
                        device_credential_statuses,
                    })
                }
                Err(error) => error_json(StatusCode(500), &error),
            },
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_account_management(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }

        self.refresh_gateway_projection_best_effort();
        let live_bridge_provider = fetch_remote_gateway_status()
            .ok()
            .and_then(|payload| live_bridge_provider_from_setup_status(&payload));
        match self.admin_store.load_state() {
            Ok(state) => {
                let mut snapshot = account_management_snapshot(&state, Some(&self.public_origin));
                if let Some(provider) = live_bridge_provider.as_ref() {
                    apply_bridge_provider_projection_to_gateway_summary(
                        &mut snapshot.gateway,
                        provider,
                    );
                }
                ok_json(&redact_account_management_snapshot(snapshot))
            }
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_gateway_status(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }

        self.refresh_gateway_projection_best_effort();
        match self.admin_store.load_state() {
            Ok(state) => {
                if let Ok(payload) = fetch_remote_gateway_status() {
                    ok_json(&payload)
                } else {
                    ok_json(&redact_gateway_status_summary(
                        account_management_snapshot(&state, Some(&self.public_origin)).gateway,
                    ))
                }
            }
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_release_readiness(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }

        self.refresh_gateway_projection_best_effort();
        let live_gateway_status = fetch_remote_gateway_status().ok();
        let live_bridge_provider = live_gateway_status
            .as_ref()
            .and_then(live_bridge_provider_from_setup_status);

        match self.admin_store.load_state() {
            Ok(state) => {
                let state_snapshot = self.current_state().ok().map(redact_state_snapshot);
                let runtime_projection = probe_local_model_runtime(&state.models.endpoints);
                let endpoints = overlay_model_endpoints_with_runtime_truth(
                    &state.models.endpoints,
                    &runtime_projection,
                );
                let mut account_management =
                    account_management_snapshot(&state, Some(&self.public_origin));
                if let Some(provider) = live_bridge_provider.as_ref() {
                    apply_bridge_provider_projection_to_gateway_summary(
                        &mut account_management.gateway,
                        provider,
                    );
                }
                let account_management = redact_account_management_snapshot(account_management);
                let feature_availability = build_feature_availability_response(
                    &endpoints,
                    &state.models.route_policies,
                    &account_management,
                    live_gateway_status.as_ref(),
                    &runtime_projection,
                );
                let hardware = build_hardware_readiness_response();
                let harboros = build_harboros_status_response(&self.public_origin);
                let rag = build_rag_readiness_response(
                    &runtime_projection,
                    &state.knowledge,
                    &state.models.endpoints,
                    &state.knowledge_index_jobs,
                    &state.audit_records,
                );
                let response = build_release_readiness_response(
                    &self.public_origin,
                    state_snapshot.as_ref(),
                    &account_management,
                    &feature_availability,
                    &hardware,
                    &harboros,
                    &rag,
                    &runtime_projection,
                );
                ok_json(&response)
            }
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_run_release_readiness(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        self.handle_release_readiness(hints)
    }

    fn handle_release_readiness_history(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }

        self.refresh_gateway_projection_best_effort();
        let live_gateway_status = fetch_remote_gateway_status().ok();
        let live_bridge_provider = live_gateway_status
            .as_ref()
            .and_then(live_bridge_provider_from_setup_status);

        match self.admin_store.load_state() {
            Ok(state) => {
                let state_snapshot = self.current_state().ok().map(redact_state_snapshot);
                let runtime_projection = probe_local_model_runtime(&state.models.endpoints);
                let endpoints = overlay_model_endpoints_with_runtime_truth(
                    &state.models.endpoints,
                    &runtime_projection,
                );
                let mut account_management =
                    account_management_snapshot(&state, Some(&self.public_origin));
                if let Some(provider) = live_bridge_provider.as_ref() {
                    apply_bridge_provider_projection_to_gateway_summary(
                        &mut account_management.gateway,
                        provider,
                    );
                }
                let account_management = redact_account_management_snapshot(account_management);
                let feature_availability = build_feature_availability_response(
                    &endpoints,
                    &state.models.route_policies,
                    &account_management,
                    live_gateway_status.as_ref(),
                    &runtime_projection,
                );
                let hardware = build_hardware_readiness_response();
                let harboros = build_harboros_status_response(&self.public_origin);
                let rag = build_rag_readiness_response(
                    &runtime_projection,
                    &state.knowledge,
                    &state.models.endpoints,
                    &state.knowledge_index_jobs,
                    &state.audit_records,
                );
                let current = build_release_readiness_response(
                    &self.public_origin,
                    state_snapshot.as_ref(),
                    &account_management,
                    &feature_availability,
                    &hardware,
                    &harboros,
                    &rag,
                    &runtime_projection,
                );
                ok_json(&ReleaseReadinessHistoryResponse {
                    generated_at: now_unix_string(),
                    entries: vec![current],
                })
            }
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_hardware_readiness(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        ok_json(&build_hardware_readiness_response())
    }

    fn handle_rag_readiness(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        match self.admin_store.load_state() {
            Ok(state) => {
                let runtime_projection = probe_local_model_runtime(&state.models.endpoints);
                ok_json(&build_rag_readiness_response(
                    &runtime_projection,
                    &state.knowledge,
                    &state.models.endpoints,
                    &state.knowledge_index_jobs,
                    &state.audit_records,
                ))
            }
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn evt_readiness_report_with_runtime(
        &self,
        gateway_status: Option<&Value>,
    ) -> Result<Value, String> {
        let mut readiness = build_evt_readiness_report(&self.admin_store, gateway_status)?;
        if let Some(object) = readiness.as_object_mut() {
            object.insert(
                "runtime_backpressure".to_string(),
                self.runtime_backpressure_snapshot(),
            );
            if let Ok(state) = self.admin_store.load_state() {
                let models = object.entry("models").or_insert_with(|| json!({}));
                if let Some(models_object) = models.as_object_mut() {
                    models_object.insert("vlm".to_string(), vlm_endpoint_readiness(&state.models));
                }
            }
        }
        Ok(readiness)
    }

    fn handle_evt_readiness(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        let live_gateway_status = fetch_remote_gateway_status().ok();
        match self.evt_readiness_report_with_runtime(live_gateway_status.as_ref()) {
            Ok(readiness) => ok_json(&readiness),
            Err(error) => error_json(StatusCode(500), &redact_admin_string(&error)),
        }
    }

    fn handle_run_evt_preflight(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        let live_gateway_status = fetch_remote_gateway_status().ok();
        match run_evt_preflight_report(&self.admin_store, live_gateway_status.as_ref()) {
            Ok(preflight) => {
                self.record_last_evt_preflight(&preflight);
                ok_json(&preflight)
            }
            Err(error) => error_json(StatusCode(500), &redact_admin_string(&error)),
        }
    }

    fn handle_latest_evt_preflight(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        ok_json(&evt_preflight_workflow_summary(self.last_evt_preflight()))
    }

    fn handle_evt_evidence_bundle(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        let live_gateway_status = fetch_remote_gateway_status().ok();
        let readiness = match self.evt_readiness_report_with_runtime(live_gateway_status.as_ref()) {
            Ok(readiness) => readiness,
            Err(error) => return error_json(StatusCode(500), &redact_admin_string(&error)),
        };
        let admin_state_stats = self
            .admin_store
            .load_state()
            .ok()
            .map(|state| {
                serde_json::to_value(self.admin_store.state_stats(Some(&state)))
                    .unwrap_or_else(|_| json!({}))
            })
            .unwrap_or_else(|| json!({"status": "unavailable", "metadata_only": true}));
        let diagnostics_workflow = build_evt_diagnostics_workflow(
            &readiness,
            self.last_evt_preflight(),
            self.last_event_notification_attempt(),
            self.last_vlm_enrichment(),
            self.last_home_assistant_service_action(),
            self.last_family_timeline_digest(),
            self.last_family_memory_feedback(),
            self.last_guardian_rule_proposal(),
            self.last_guardian_rule_evaluation(),
            self.home_guardian_queue_snapshot(),
            self.runtime_backpressure_snapshot(),
            admin_state_stats,
            build_latest_general_message_nsp_route_workflow(&self.task_service),
            build_latest_home_assistant_task_api_workflow(&self.task_service),
            build_latest_family_memory_task_api_workflow(&self.task_service),
        );
        let bundle =
            build_evt_evidence_bundle(readiness, self.last_evt_preflight(), diagnostics_workflow);
        ok_json(&bundle)
    }

    fn handle_redacted_diagnostics_bundle(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        let admin_state = match self.admin_store.load_state() {
            Ok(state) => state,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let redacted_state = match self.current_state() {
            Ok(state) => redact_state_snapshot(state),
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let runtime_projection = probe_local_model_runtime(&admin_state.models.endpoints);
        let home_assistant =
            harborbeacon_local_agent::runtime::admin_console::redact_home_assistant_state(
                admin_state.home_assistant.clone(),
            );
        let live_gateway_status = fetch_remote_gateway_status().ok();
        let evt_readiness = self
            .evt_readiness_report_with_runtime(live_gateway_status.as_ref())
            .unwrap_or_else(|error| {
                json!({
                    "kind": "evt_readiness_v1",
                    "status": "blocked",
                    "error": redact_admin_string(&error),
                    "redacted": true,
                })
            });
        let bundle = build_redacted_diagnostics_bundle(
            &redacted_state,
            &build_home_assistant_status_response(&home_assistant),
            &runtime_projection,
            self.last_event_notification_attempt(),
            self.last_vlm_enrichment(),
            self.last_home_assistant_service_action(),
            self.last_family_timeline_digest(),
            self.last_family_memory_feedback(),
            self.last_guardian_rule_proposal(),
            self.last_guardian_rule_evaluation(),
            self.home_guardian_queue_snapshot(),
            self.runtime_backpressure_snapshot(),
            serde_json::to_value(self.admin_store.state_stats(Some(&admin_state)))
                .unwrap_or_else(|_| json!({})),
            build_latest_general_message_nsp_route_workflow(&self.task_service),
            build_latest_home_assistant_task_api_workflow(&self.task_service),
            build_latest_family_memory_task_api_workflow(&self.task_service),
            evt_readiness,
            self.last_evt_preflight(),
            &admin_state.notification_targets,
            live_gateway_status.as_ref(),
        );
        self.record_admin_audit(
            &AccessPrincipal {
                workspace_id: "home-1".to_string(),
                user_id: "system".to_string(),
                display_name: "system".to_string(),
                role_kind: RoleKind::Owner,
            },
            "diagnostics",
            "redacted-bundle",
            "diagnostics.redacted_bundle.generate",
            json!({"surface": "admin_api"}),
            json!({"status": "generated", "metadata_only": true}),
        );
        ok_json(&bundle)
    }

    fn handle_routing_status(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        match self.admin_store.load_state() {
            Ok(state) => {
                let runtimes = state
                    .models
                    .runtimes
                    .iter()
                    .map(|runtime| RoutingRuntimeProjection {
                        runtime_id: runtime.runtime_id.clone(),
                        status: runtime.status.clone(),
                        enabled: runtime.enabled,
                        capabilities: runtime.capabilities.clone(),
                    })
                    .collect::<Vec<_>>();
                ok_json(&build_routing_status(
                    &state.models.endpoints,
                    &state.models.route_policies,
                    &runtimes,
                    now_unix_string(),
                ))
            }
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_audit_records(
        &self,
        raw_url: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        match self.admin_store.audit_records() {
            Ok(records) => ok_json(&query_audit_records(
                &records,
                AuditRecordQuery {
                    limit: parse_query_param(raw_url, "limit")
                        .and_then(|value| value.parse::<usize>().ok()),
                    cursor: parse_query_param(raw_url, "cursor")
                        .and_then(|value| value.parse::<usize>().ok()),
                    entity_kind: parse_query_param(raw_url, "entity_kind")
                        .and_then(|value| non_empty_string(&value)),
                    entity_id: parse_query_param(raw_url, "entity_id")
                        .and_then(|value| non_empty_string(&value)),
                    action: parse_query_param(raw_url, "action")
                        .and_then(|value| non_empty_string(&value)),
                },
            )),
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_audit_summary(
        &self,
        raw_url: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        let window = parse_query_param(raw_url, "window").unwrap_or_else(|| "24h".to_string());
        match self.admin_store.audit_records() {
            Ok(records) => ok_json(&build_audit_summary(&records, window)),
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_knowledge_settings(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        match self.admin_store.knowledge_settings() {
            Ok(settings) => ok_json(&settings),
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_save_knowledge_settings(
        &self,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        let settings: KnowledgeSettings = match read_json_body(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };
        match validate_knowledge_settings(settings)
            .and_then(|settings| self.admin_store.save_knowledge_settings(settings))
        {
            Ok(state) => ok_json(&state.knowledge),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_knowledge_search(
        &self,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        let payload: KnowledgeSearchApiRequest = match read_json_body(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };
        let settings = match self.admin_store.knowledge_settings() {
            Ok(settings) => settings,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let focus_paths = match self.resolve_dvr_search_focus_paths(&payload) {
            Ok(paths) => paths,
            Err(error) => return error_json(StatusCode(422), &error),
        };
        let dvr_settings = self.admin_store.dvr_recording_settings().ok();
        let scoped_roots =
            match resolve_admin_search_source_scope(&payload, &settings, dvr_settings.as_ref()) {
                Ok(roots) => roots,
                Err(error) => return error_json(StatusCode(422), &error),
            };
        let search_request = match build_admin_knowledge_search_request(
            payload,
            &settings,
            focus_paths,
            scoped_roots,
        ) {
            Ok(request) => request,
            Err(error) => return error_json(StatusCode(422), &error),
        };
        match KnowledgeSearchService::search(search_request) {
            Ok(response) => ok_json(&response),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn resolve_dvr_search_focus_paths(
        &self,
        payload: &KnowledgeSearchApiRequest,
    ) -> Result<Vec<String>, String> {
        if !payload.has_dvr_focus() {
            return Ok(Vec::new());
        }
        if payload.include_videos == Some(false) {
            return Err("DVR camera/time filters require video search to be enabled.".to_string());
        }
        let from_secs = parse_optional_unix_seconds(payload.from.as_deref(), "from")?;
        let to_secs = parse_optional_unix_seconds(payload.to.as_deref(), "to")?;
        if let (Some(from), Some(to)) = (from_secs, to_secs) {
            if from > to {
                return Err("DVR search time filter has from greater than to.".to_string());
            }
        }
        let settings = self.admin_store.dvr_recording_settings()?;
        if let Err(error) = apply_retention_policy(&settings) {
            return Err(error);
        }
        let devices = self.hub().load_registered_cameras()?;
        let camera_id = payload.camera_id.as_deref().and_then(non_empty_string);
        let timeline = scan_timeline(
            &settings,
            &devices,
            camera_id.as_deref(),
            from_secs,
            to_secs,
            None,
        )?;
        let focus_paths = timeline
            .segments
            .into_iter()
            .map(|segment| segment.file_path)
            .collect::<Vec<_>>();
        if focus_paths.is_empty() {
            return Err(
                "No DVR recording segments matched the requested camera/time scope.".to_string(),
            );
        }
        Ok(focus_paths)
    }

    fn handle_knowledge_preview(
        &self,
        raw_url: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        let requested_path = match parse_query_param(raw_url, "path")
            .and_then(percent_decode_optional_query_value)
            .and_then(|value| non_empty_string(&value))
        {
            Some(path) => path,
            None => return error_json(StatusCode(400), "knowledge preview requires path"),
        };
        let settings = match self.admin_store.knowledge_settings() {
            Ok(settings) => settings,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let preview_path = match resolve_knowledge_preview_path(&requested_path, &settings) {
            Ok(path) => path,
            Err(error) => match self
                .admin_store
                .dvr_recording_settings()
                .ok()
                .and_then(|settings| dvr_media_preview_path(&settings, &requested_path).ok())
            {
                Some(path) => path,
                None => return error_json(error.status, &error.message),
            },
        };
        static_file_response(&preview_path)
    }

    fn handle_dvr_recording_settings(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        match self.admin_store.dvr_recording_settings() {
            Ok(settings) => ok_json(&settings),
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_save_dvr_recording_settings(
        &self,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        let settings: DvrRecordingSettings = match read_json_body(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };
        match self.admin_store.save_dvr_recording_settings(settings) {
            Ok(state) => ok_json(&state.dvr),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_dvr_recordings_status(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        let settings = match self.admin_store.dvr_recording_settings() {
            Ok(settings) => settings,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let devices = match self.hub().load_registered_cameras() {
            Ok(devices) => devices,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let statuses =
            match self
                .dvr_runtime
                .statuses(&devices, &settings, Some(&self.public_origin))
            {
                Ok(statuses) => statuses,
                Err(error) => return error_json(StatusCode(500), &error),
            };
        ok_json(&build_status_response(settings, statuses, devices.len()))
    }

    fn handle_dvr_recordings_timeline(
        &self,
        raw_url: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        let settings = match self.admin_store.dvr_recording_settings() {
            Ok(settings) => settings,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        if let Err(error) = apply_retention_policy(&settings) {
            return error_json(StatusCode(422), &error);
        }
        let devices = match self.hub().load_registered_cameras() {
            Ok(devices) => devices,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let device_id = parse_query_param(raw_url, "device_id")
            .and_then(percent_decode_optional_query_value)
            .and_then(|value| non_empty_string(&value));
        let from_secs =
            parse_query_param(raw_url, "from").and_then(|value| value.parse::<u64>().ok());
        let to_secs = parse_query_param(raw_url, "to").and_then(|value| value.parse::<u64>().ok());
        match scan_timeline(
            &settings,
            &devices,
            device_id.as_deref(),
            from_secs,
            to_secs,
            Some(&self.public_origin),
        ) {
            Ok(response) => ok_json(&response),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_run_knowledge_index(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        let settings = match self.admin_store.knowledge_settings() {
            Ok(settings) => settings,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        if settings.enabled_source_root_paths().is_empty() {
            return error_json(
                StatusCode(422),
                "请先在 Harbor Assistant 配置并启用至少一个知识源目录。",
            );
        }
        if let Ok(jobs) = self.admin_store.list_knowledge_index_jobs() {
            if jobs
                .iter()
                .any(|job| matches!(job.status.as_str(), "queued" | "running"))
            {
                return error_json(
                    StatusCode(409),
                    "已有 knowledge index job 正在 queued/running；请等待完成或取消后再启动新的刷新。",
                );
            }
        }
        if let Err(error) = KnowledgeIndexConfig::new(PathBuf::from(settings.index_root.clone()))
            .and_then(KnowledgeIndexService::from_config)
        {
            return error_json(StatusCode(422), &error);
        }
        let generated_at = now_unix_string();
        let enabled_roots = settings
            .source_roots
            .iter()
            .filter(|root| root.enabled)
            .cloned()
            .collect::<Vec<_>>();
        let mut job_ids = Vec::new();
        let mut indexed_roots = Vec::new();
        let mut jobs = Vec::new();
        for root in &enabled_roots {
            let job =
                build_knowledge_index_job(root, &generated_at, settings.default_resource_profile);
            job_ids.push(job.job_id.clone());
            if let Err(error) = self.admin_store.save_knowledge_index_job(job.clone()) {
                return error_json(StatusCode(500), &error);
            }
            indexed_roots.push(queued_knowledge_root_status(root));
            jobs.push(job);
        }
        let worker_store = AdminConsoleStore::new(
            self.admin_store.path().to_path_buf(),
            self.admin_store.registry_store().clone(),
        );
        if let Err(error) =
            spawn_knowledge_index_worker(worker_store, settings.clone(), jobs.clone())
        {
            for mut job in jobs {
                job.status = "failed".to_string();
                job.progress_percent = Some(100);
                job.completed_at = Some(now_unix_string());
                job.error_message = Some(error.clone());
                job.checkpoint = json!({"phase": "worker_spawn_failed"});
                let _ = self.admin_store.save_knowledge_index_job(job);
            }
            return error_json(StatusCode(500), &error);
        }
        ok_json(&KnowledgeIndexRunResponse {
            generated_at,
            job_ids,
            status: "queued".to_string(),
            index_root: settings.index_root,
            root_count: indexed_roots.len(),
            indexed_roots,
            errors: Vec::new(),
        })
    }

    fn handle_knowledge_index_status(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        match self.admin_store.knowledge_settings() {
            Ok(settings) => ok_json(&build_knowledge_index_status_response(settings)),
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_knowledge_index_jobs(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        match self.admin_store.list_knowledge_index_jobs() {
            Ok(jobs) => ok_json(&json!({
                "generated_at": now_unix_string(),
                "jobs": jobs,
            })),
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_cancel_knowledge_index_job(
        &self,
        path: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        let Some(job_id) = parse_knowledge_index_job_cancel_path(path) else {
            return error_json(StatusCode(404), "knowledge index job route not found");
        };
        match self
            .admin_store
            .cancel_knowledge_index_job(&job_id, now_unix_string())
        {
            Ok(Some(job)) => ok_json(&job),
            Ok(None) => error_json(StatusCode(404), "knowledge index job not found"),
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_files_browse(
        &self,
        raw_url: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        let settings = match self.admin_store.knowledge_settings() {
            Ok(settings) => settings,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let requested_path = parse_query_param(raw_url, "path")
            .and_then(percent_decode_optional_query_value)
            .and_then(|value| non_empty_string(&value));
        match build_files_browse_response(requested_path.as_deref(), &settings) {
            Ok(response) => ok_json(&response),
            Err(error) if error.contains("not inside an allowed") => {
                error_json(StatusCode(403), &error)
            }
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_harboros_status(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        ok_json(&build_harboros_status_response(&self.public_origin))
    }

    fn handle_harboros_im_capability_map(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        ok_json(&build_harboros_im_capability_map())
    }

    fn handle_home_assistant_status(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        match self.admin_store.home_assistant_state() {
            Ok(state) => ok_json(&build_home_assistant_status_response(&state)),
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_save_home_assistant_config(
        &self,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        let body: HomeAssistantConfigRequest = match read_json_body(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };
        let update = HomeAssistantConfigUpdate {
            enabled: body.enabled,
            base_url: body.base_url,
            access_token: body.access_token,
            exposed_domains: body.exposed_domains,
        };
        match self.admin_store.save_home_assistant_config(update) {
            Ok(state) => ok_json(&HomeAssistantConfigResponse {
                status: build_home_assistant_status_response(
                    &harborbeacon_local_agent::runtime::admin_console::redact_home_assistant_state(
                        state.home_assistant,
                    ),
                ),
            }),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_test_home_assistant(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        let state = match self.admin_store.home_assistant_secret_state() {
            Ok(state) => state,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let client = match home_assistant_client_from_state(&state) {
            Ok(client) => client,
            Err(error) => return error_json(StatusCode(422), &error),
        };
        let test = client.test_connection();
        let persisted = self.admin_store.record_home_assistant_test(
            test.ok,
            test.version.clone(),
            test.location_name.clone(),
            test.error.clone(),
        );
        match persisted {
            Ok(state) => ok_json(&json!({
                "test": test,
                "status": build_home_assistant_status_response(
                    &harborbeacon_local_agent::runtime::admin_console::redact_home_assistant_state(
                        state.home_assistant,
                    ),
                ),
            })),
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_sync_home_assistant(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        let state = match self.admin_store.home_assistant_secret_state() {
            Ok(state) => state,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let client = match home_assistant_client_from_state(&state) {
            Ok(client) => client,
            Err(error) => return error_json(StatusCode(422), &error),
        };
        let entities = match client.fetch_entities() {
            Ok(entities) => filter_home_assistant_entities(entities, &state.exposed_domains),
            Err(error) => return error_json(StatusCode(422), &error),
        };
        let services = match client.fetch_services() {
            Ok(services) => services,
            Err(error) => return error_json(StatusCode(422), &error),
        };
        let sync_at = now_unix_string();
        let mut snapshot = match self.admin_store.registry_store().load_snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let registry_entities = entities
            .iter()
            .cloned()
            .map(home_assistant_registry_entity)
            .collect::<Vec<_>>();
        snapshot.upsert_home_assistant_entities(&registry_entities, &sync_at);
        if let Err(error) = self.admin_store.registry_store().save_snapshot(&snapshot) {
            return error_json(StatusCode(500), &error);
        }
        let service_count = services.iter().map(|domain| domain.services.len()).sum();
        match self
            .admin_store
            .record_home_assistant_sync(entities.len(), service_count)
        {
            Ok(state) => ok_json(&HomeAssistantSyncResponse {
                status: build_home_assistant_status_response(
                    &harborbeacon_local_agent::runtime::admin_console::redact_home_assistant_state(
                        state.home_assistant,
                    ),
                ),
                entities,
                service_domains: services,
            }),
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_home_assistant_entities(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        let state = match self.admin_store.home_assistant_secret_state() {
            Ok(state) => state,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let client = match home_assistant_client_from_state(&state) {
            Ok(client) => client,
            Err(error) => return error_json(StatusCode(422), &error),
        };
        match client.fetch_entities() {
            Ok(entities) => ok_json(&HomeAssistantEntitiesResponse {
                entities: filter_home_assistant_entities(entities, &state.exposed_domains),
            }),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_home_assistant_services(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        let state = match self.admin_store.home_assistant_secret_state() {
            Ok(state) => state,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let client = match home_assistant_client_from_state(&state) {
            Ok(client) => client,
            Err(error) => return error_json(StatusCode(422), &error),
        };
        match client.fetch_services() {
            Ok(services) => ok_json(&HomeAssistantServicesResponse { services }),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_home_assistant_service_smoke(
        &self,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        let body: HomeAssistantServiceSmokeRequest = match read_json_body(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };
        let state = match self.admin_store.home_assistant_secret_state() {
            Ok(state) => state,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let normalized = normalize_home_assistant_service_smoke_request(&body);
        if let Err(message) = validate_home_assistant_service_smoke(&normalized, &state) {
            return ok_json(&HomeAssistantServiceSmokeResponse {
                status: "blocked".to_string(),
                allowed: false,
                executed: false,
                domain: normalized.domain,
                service: normalized.service,
                entity_id: normalized.entity_id,
                message: message.clone(),
                result: None,
                audit_record: build_home_assistant_operator_audit(
                    "home_assistant.service_smoke_blocked",
                    "blocked",
                    false,
                    false,
                    &message,
                    &body,
                ),
            });
        }
        let client = match home_assistant_client_from_state(&state) {
            Ok(client) => client,
            Err(error) => return error_json(StatusCode(422), &error),
        };
        match client.call_service(
            &normalized.domain,
            &normalized.service,
            &normalized.entity_id,
            Some(&normalized.fields),
        ) {
            Ok(result) => ok_json(&HomeAssistantServiceSmokeResponse {
                status: "succeeded".to_string(),
                allowed: true,
                executed: true,
                domain: normalized.domain,
                service: normalized.service,
                entity_id: normalized.entity_id,
                message: "Home Assistant allowlisted service call completed.".to_string(),
                result: Some(result),
                audit_record: build_home_assistant_operator_audit(
                    "home_assistant.service_smoke_executed",
                    "succeeded",
                    true,
                    true,
                    "Home Assistant allowlisted service call completed.",
                    &body,
                ),
            }),
            Err(error) => error_json(StatusCode(422), &redact_admin_string(&error)),
        }
    }

    fn handle_home_assistant_service_action(
        &self,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        let body: HomeAssistantServiceActionRequest = match read_json_body(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };
        let state = match self.admin_store.home_assistant_secret_state() {
            Ok(state) => state,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let action_id = format!("ha_action_{}", Uuid::new_v4().simple());
        let normalized = normalize_home_assistant_service_smoke_request(&body);
        if let Err(message) = validate_home_assistant_service_smoke(&normalized, &state) {
            let response = HomeAssistantServiceActionResponse {
                action_id,
                status: "blocked".to_string(),
                allowed: false,
                executed: false,
                domain: normalized.domain,
                service: normalized.service,
                entity_id: normalized.entity_id,
                message: message.clone(),
                result: None,
                audit_record: build_home_assistant_operator_audit(
                    "home_assistant.service_action_blocked",
                    "blocked",
                    false,
                    false,
                    &message,
                    &body,
                ),
            };
            self.record_last_home_assistant_service_action(&response);
            return ok_json(&response);
        }
        let client = match home_assistant_client_from_state(&state) {
            Ok(client) => client,
            Err(error) => return error_json(StatusCode(422), &error),
        };
        match client.call_service(
            &normalized.domain,
            &normalized.service,
            &normalized.entity_id,
            Some(&normalized.fields),
        ) {
            Ok(result) => {
                let response = HomeAssistantServiceActionResponse {
                    action_id,
                    status: "succeeded".to_string(),
                    allowed: true,
                    executed: true,
                    domain: normalized.domain,
                    service: normalized.service,
                    entity_id: normalized.entity_id,
                    message: "Home Assistant low-risk service action completed.".to_string(),
                    result: Some(result),
                    audit_record: build_home_assistant_operator_audit(
                        "home_assistant.service_action_executed",
                        "succeeded",
                        true,
                        true,
                        "Home Assistant low-risk service action completed.",
                        &body,
                    ),
                };
                self.record_last_home_assistant_service_action(&response);
                ok_json(&response)
            }
            Err(error) => {
                let message = redact_admin_string(&error);
                let response = HomeAssistantServiceActionResponse {
                    action_id,
                    status: "failed".to_string(),
                    allowed: true,
                    executed: false,
                    domain: normalized.domain,
                    service: normalized.service,
                    entity_id: normalized.entity_id,
                    message: message.clone(),
                    result: None,
                    audit_record: build_home_assistant_operator_audit(
                        "home_assistant.service_action_failed",
                        "failed",
                        true,
                        false,
                        &message,
                        &body,
                    ),
                };
                self.record_last_home_assistant_service_action(&response);
                ok_json(&response)
            }
        }
    }

    fn handle_home_assistant_install_status(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        ok_json(&build_home_assistant_install_status_response())
    }

    fn handle_home_assistant_install_plan(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        ok_json(&build_home_assistant_install_plan_response())
    }

    fn handle_home_assistant_install(
        &self,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        let body: HomeAssistantInstallRequest = match read_json_body_or_default(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };
        let plan = build_home_assistant_install_plan_response();
        if body.dry_run {
            return ok_json(&HomeAssistantInstallResponse {
                status: "dry_run".to_string(),
                dry_run: true,
                plan,
                message: "Dry run only; no Docker command was executed.".to_string(),
            });
        }
        match install_home_assistant_container() {
            Ok(status) => ok_json(&HomeAssistantInstallResponse {
                status: status.status,
                dry_run: false,
                plan,
                message: status.message,
            }),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_model_endpoints(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        match self.admin_store.load_state() {
            Ok(state) => {
                let runtime_projection = probe_local_model_runtime(&state.models.endpoints);
                let endpoints = overlay_model_endpoints_with_runtime_truth(
                    &state.models.endpoints,
                    &runtime_projection,
                );
                ok_json(&ModelEndpointsResponse {
                    endpoints: endpoints.iter().map(redact_model_endpoint).collect(),
                })
            }
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_model_capabilities(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        let state = match self.admin_store.load_state() {
            Ok(state) => state,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let download_jobs = match self.admin_store.list_model_download_jobs() {
            Ok(jobs) => jobs,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let runtime_projection = probe_local_model_runtime(&state.models.endpoints);
        let endpoints = overlay_model_endpoints_with_runtime_truth(
            &state.models.endpoints,
            &runtime_projection,
        );
        ok_json(&build_model_capabilities_response(
            &state.models,
            &endpoints,
            &state.models.route_policies,
            download_jobs,
            &runtime_projection,
        ))
    }

    fn handle_model_runtimes(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        let state = match self.admin_store.load_state() {
            Ok(state) => state,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let runtime_projection = probe_local_model_runtime(&state.models.endpoints);
        ok_json(&build_model_runtime_manager_response(
            &state.models,
            &runtime_projection,
        ))
    }

    fn handle_inference_health_alias(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        let state = match self.admin_store.load_state() {
            Ok(state) => state,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let runtime_projection = probe_local_model_runtime(&state.models.endpoints);
        ok_json(&build_inference_health_alias_response(&runtime_projection))
    }

    fn handle_model_store(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        match self.admin_store.load_state() {
            Ok(state) => ok_json(&build_model_store_status(&state.models)),
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_local_model_catalog(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        let state = match self.admin_store.load_state() {
            Ok(state) => state,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        match self.admin_store.list_model_download_jobs() {
            Ok(download_jobs) => ok_json(&build_local_model_catalog_for_model_state(
                &state.models,
                download_jobs,
            )),
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_model_download_jobs(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        match self.admin_store.list_model_download_jobs() {
            Ok(jobs) => {
                let latest_jobs = latest_model_download_jobs(&jobs);
                ok_json(&ModelDownloadJobsResponse {
                    generated_at: now_unix_string(),
                    checked_at: now_unix_string(),
                    status: model_download_jobs_status(&jobs).to_string(),
                    jobs: latest_jobs.clone(),
                    downloads: latest_jobs,
                    blockers: Vec::new(),
                    warnings: Vec::new(),
                })
            }
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_model_download_job(
        &self,
        path: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        let job_id = match parse_model_download_job_path(path) {
            Some(job_id) => job_id,
            None => return error_json(StatusCode(400), "invalid model download job path"),
        };
        match self.admin_store.model_download_job(&job_id) {
            Ok(Some(job)) => ok_json(&ModelDownloadJobResponse::new(job)),
            Ok(None) => error_json(StatusCode(404), "model download job not found"),
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_feature_availability(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }

        self.refresh_gateway_projection_best_effort();
        let live_gateway_status = fetch_remote_gateway_status().ok();
        let live_bridge_provider = live_gateway_status
            .as_ref()
            .and_then(live_bridge_provider_from_setup_status);

        match self.admin_store.load_state() {
            Ok(state) => {
                let runtime_projection = probe_local_model_runtime(&state.models.endpoints);
                let endpoints = overlay_model_endpoints_with_runtime_truth(
                    &state.models.endpoints,
                    &runtime_projection,
                );
                let mut account_management =
                    account_management_snapshot(&state, Some(&self.public_origin));
                if let Some(provider) = live_bridge_provider.as_ref() {
                    apply_bridge_provider_projection_to_gateway_summary(
                        &mut account_management.gateway,
                        provider,
                    );
                }
                let response = build_feature_availability_response(
                    &endpoints,
                    &state.models.route_policies,
                    &account_management,
                    live_gateway_status.as_ref(),
                    &runtime_projection,
                );
                ok_json(&response)
            }
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_ingest_local_vision_event(
        &self,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        let Some(_ingest_guard) = self.vision_ingest_limiter.try_acquire() else {
            return json_response(
                StatusCode(429),
                &json!({
                    "error": "vision ingest busy",
                    "status": "blocked",
                    "reason": "Too many local vision event ingests are in progress; retry after the current batch drains.",
                    "metadata_only": true,
                    "secret_scan": "clean",
                    "limits": self.vision_ingest_limiter.snapshot(),
                }),
            );
        };
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        let event = match read_json_body_limited::<LocalVisionEvent>(
            request,
            MAX_LOCAL_VISION_EVENT_JSON_BYTES,
        ) {
            Ok(event) => event,
            Err(error) if error.contains("exceeds") => return error_json(StatusCode(413), &error),
            Err(error) => return error_json(StatusCode(400), &error),
        };
        match ingest_local_vision_event_default(event) {
            Ok(stored) => {
                let queue_status = self.enqueue_home_guardian_evaluation(&stored);
                if queue_status != "queued" {
                    self.record_last_guardian_rule_evaluation(&json!({
                        "status": queue_status,
                        "event_id": stored.event.event_id,
                        "reason": "Home Guardian evaluation queue did not accept the event.",
                        "queue": self.home_guardian_queue_snapshot(),
                        "metadata_only": true,
                        "secret_scan": "clean",
                    }));
                }
                ok_json(&stored)
            }
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_list_local_vision_events(
        &self,
        raw_url: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        let limit = parse_query_param(raw_url, "limit")
            .and_then(|value| value.parse::<usize>().ok())
            .map(|value| value.clamp(1, 50))
            .unwrap_or(10);
        match list_recent_local_vision_events_default(limit) {
            Ok(events) => ok_json(&VisionEventsResponse {
                generated_at: now_unix_string(),
                limit,
                events,
            }),
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_vision_vlm_status(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        match self.admin_store.load_state() {
            Ok(state) => ok_json(&json!({
                "generated_at": now_unix_string(),
                "kind": "vision_vlm_status_v1",
                "readiness": vlm_endpoint_readiness(&state.models),
                "latest_enrichment": self.last_vlm_enrichment().unwrap_or_else(|| json!({
                    "status": "not_run",
                    "message": "No VLM enrichment has been attempted in this API process.",
                    "metadata_only": true,
                    "secret_scan": "clean",
                })),
                "metadata_only": true,
                "secret_scan": "clean",
            })),
            Err(error) => error_json(StatusCode(500), &redact_admin_string(&error)),
        }
    }

    fn handle_vlm_enrich_latest_local_vision_event(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        let principal = match self.authorize_admin_action(hints, AccessAction::AdminManage) {
            Ok(principal) => principal,
            Err(error) => return error_json(StatusCode(403), &error),
        };
        let event = match list_recent_local_vision_events_default(1) {
            Ok(mut events) => match events.pop() {
                Some(event) => event,
                None => return error_json(StatusCode(404), "no local vision event available"),
            },
            Err(error) => return error_json(StatusCode(500), &redact_admin_string(&error)),
        };
        self.enrich_local_vision_event_with_vlm(&principal, event, "latest")
    }

    fn handle_vlm_enrich_local_vision_event(
        &self,
        path: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        let principal = match self.authorize_admin_action(hints, AccessAction::AdminManage) {
            Ok(principal) => principal,
            Err(error) => return error_json(StatusCode(403), &error),
        };
        let event_id = match parse_local_vision_event_vlm_enrich_path(path) {
            Some(event_id) => event_id,
            None => return error_json(StatusCode(400), "invalid local vision event VLM path"),
        };
        let event = match find_recent_local_vision_event(&event_id) {
            Ok(Some(event)) => event,
            Ok(None) => return error_json(StatusCode(404), "local vision event not found"),
            Err(error) => return error_json(StatusCode(500), &redact_admin_string(&error)),
        };
        self.enrich_local_vision_event_with_vlm(&principal, event, "event_id")
    }

    fn enrich_local_vision_event_with_vlm(
        &self,
        principal: &AccessPrincipal,
        stored: StoredLocalVisionEvent,
        trigger: &str,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        let state = match self.admin_store.load_state() {
            Ok(state) => state,
            Err(error) => return error_json(StatusCode(500), &redact_admin_string(&error)),
        };
        let readiness = vlm_endpoint_readiness(&state.models);
        if readiness
            .get("endpoint_ready")
            .and_then(Value::as_bool)
            .is_some_and(|ready| !ready)
        {
            let evidence = build_vlm_enrichment_evidence(
                &stored,
                "blocked",
                0,
                None,
                "endpoint_not_ready",
                trigger,
                readiness.clone(),
            );
            self.record_last_vlm_enrichment(evidence.clone());
            self.record_admin_audit(
                principal,
                "local_vision_event",
                &stored.event.event_id,
                "local_vision_event.vlm_enrichment_blocked",
                json!({"source": "vision_vlm_api", "trigger": trigger}),
                evidence.clone(),
            );
            return json_response(
                StatusCode(409),
                &json!({
                    "status": "blocked",
                    "reason": "endpoint_not_ready",
                    "message": "Local-only VLM endpoint is not ready.",
                    "event_id": stored.event.event_id,
                    "camera_id": stored.event.camera_id,
                    "readiness": readiness,
                    "evidence": evidence,
                    "metadata_only": true,
                    "secret_scan": "clean",
                }),
            );
        }

        let temp_dir =
            std::env::temp_dir().join(format!("harbornavi-vlm-enrich-{}", Uuid::new_v4().simple()));
        let frame_path = temp_dir.join("frame.jpg");
        let started = Instant::now();
        let result = (|| -> Result<StoredLocalVisionEvent, String> {
            fs::create_dir_all(&temp_dir)
                .map_err(|error| format!("failed to create VLM temp dir: {error}"))?;
            let bytes = self.capture_camera_snapshot(&stored.event.camera_id)?;
            fs::write(&frame_path, bytes)
                .map_err(|error| format!("failed to write VLM temp frame: {error}"))?;
            let execution = run_vlm_summary_with_state(&frame_path, &state.models);
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
                    "trigger": trigger,
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
        let elapsed_ms = started.elapsed().as_millis() as u64;

        match result {
            Ok(enriched) => {
                let status = enriched
                    .event
                    .vlm
                    .as_ref()
                    .map(|vlm| vlm.status.as_str())
                    .unwrap_or("degraded");
                let reason = if status == "active" {
                    "enriched"
                } else if status == "busy" {
                    "queue_busy"
                } else if status == "blocked" {
                    "local_only_blocked"
                } else {
                    "vlm_degraded"
                };
                let evidence = build_vlm_enrichment_evidence(
                    &enriched,
                    status,
                    elapsed_ms,
                    enriched
                        .event
                        .vlm
                        .as_ref()
                        .and_then(|vlm| vlm.vlm_metrics.get("model_endpoint_id").cloned()),
                    reason,
                    trigger,
                    readiness,
                );
                self.record_last_vlm_enrichment(evidence.clone());
                self.record_admin_audit(
                    principal,
                    "local_vision_event",
                    &enriched.event.event_id,
                    "local_vision_event.vlm_enrichment_attempted",
                    json!({"source": "vision_vlm_api", "trigger": trigger}),
                    evidence.clone(),
                );
                ok_json(&json!({
                    "status": status,
                    "reason": reason,
                    "event": enriched,
                    "evidence": evidence,
                    "metadata_only": true,
                    "secret_scan": "clean",
                }))
            }
            Err(error) => {
                let redacted_error = redact_admin_string(&error);
                let (http_status, status, reason, message, audit_action) =
                    if redacted_error == "vlm_queue_busy" {
                        (
                            StatusCode(409),
                            "busy",
                            "queue_busy",
                            "VLM queue is busy; no event version was updated.",
                            "local_vision_event.vlm_enrichment_busy",
                        )
                    } else if redacted_error == "vlm_local_only_blocked" {
                        (
                            StatusCode(409),
                            "blocked",
                            "local_only_blocked",
                            "Local-only VLM guard blocked this enrichment.",
                            "local_vision_event.vlm_enrichment_blocked",
                        )
                    } else {
                        (
                            StatusCode(422),
                            "failed",
                            redacted_error.as_str(),
                            "VLM enrichment failed.",
                            "local_vision_event.vlm_enrichment_failed",
                        )
                    };
                let evidence = build_vlm_enrichment_evidence(
                    &stored, status, elapsed_ms, None, reason, trigger, readiness,
                );
                self.record_last_vlm_enrichment(evidence.clone());
                self.record_admin_audit(
                    principal,
                    "local_vision_event",
                    &stored.event.event_id,
                    audit_action,
                    json!({"source": "vision_vlm_api", "trigger": trigger}),
                    evidence.clone(),
                );
                json_response(
                    http_status,
                    &json!({
                        "status": status,
                        "reason": reason,
                        "message": message,
                        "event_id": stored.event.event_id,
                        "camera_id": stored.event.camera_id,
                        "evidence": evidence,
                        "metadata_only": true,
                        "secret_scan": "clean",
                    }),
                )
            }
        }
    }

    fn handle_family_timeline(
        &self,
        raw_url: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        let limit = parse_query_param(raw_url, "limit")
            .and_then(|value| value.parse::<usize>().ok())
            .map(|value| value.clamp(1, 100))
            .unwrap_or(50);
        let window_seconds = parse_query_param(raw_url, "window_seconds")
            .and_then(|value| value.parse::<u64>().ok())
            .map(|value| value.clamp(60, 7 * 24 * 60 * 60))
            .unwrap_or(24 * 60 * 60);
        let camera_filter = parse_query_param(raw_url, "camera_id");
        let label_filter = parse_query_param(raw_url, "label");
        let include_hidden = parse_query_bool(raw_url, "include_hidden").unwrap_or(false);
        let query = FamilyTimelineQueryOptions::new(
            window_seconds,
            camera_filter.clone(),
            label_filter.clone(),
            limit,
        );
        let events = match list_recent_local_vision_events_default(limit) {
            Ok(events) => events,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let overlays = match family_memory_overlays_default() {
            Ok(overlays) => overlays,
            Err(error) => return error_json(StatusCode(500), &redact_admin_string(&error)),
        };
        let memory_overlay = match family_memory_stats_default() {
            Ok(stats) => stats,
            Err(error) => return error_json(StatusCode(500), &redact_admin_string(&error)),
        };
        let events = apply_family_memory_overlays_to_events(&events, &overlays, include_hidden);
        match build_family_timeline_from_query(&events, &query) {
            Ok(timeline) => ok_json(&FamilyTimelineApiResponse {
                timeline,
                memory_overlay,
            }),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_family_timeline_digest(
        &self,
        raw_url: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        let window_seconds = parse_query_param(raw_url, "window_seconds")
            .and_then(|value| value.parse::<u64>().ok())
            .map(|value| value.clamp(60, 7 * 24 * 60 * 60))
            .unwrap_or(24 * 60 * 60);
        let query = FamilyTimelineQueryOptions::new(window_seconds, None, None, 50);
        let events = match list_recent_local_vision_events_default(50) {
            Ok(events) => events,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let overlays = match family_memory_overlays_default() {
            Ok(overlays) => overlays,
            Err(error) => return error_json(StatusCode(500), &redact_admin_string(&error)),
        };
        let records =
            match list_family_memory_feedback_default(DEFAULT_FAMILY_MEMORY_FEEDBACK_LIMIT) {
                Ok(records) => records,
                Err(error) => return error_json(StatusCode(500), &redact_admin_string(&error)),
            };
        let memory_overlay = build_family_memory_stats(&records);
        let events = apply_family_memory_overlays_to_events(&events, &overlays, false);
        match build_family_timeline_digest_from_query(&events, &query) {
            Ok(digest) => {
                self.record_last_family_timeline_digest(&digest);
                ok_json(&FamilyTimelineDigestApiResponse {
                    digest,
                    memory_overlay,
                })
            }
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_family_memory_events(
        &self,
        raw_url: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        let limit = parse_query_param(raw_url, "limit")
            .and_then(|value| value.parse::<usize>().ok())
            .map(|value| value.clamp(1, 100))
            .unwrap_or(50);
        let window_seconds = parse_query_param(raw_url, "window_seconds")
            .and_then(|value| value.parse::<u64>().ok())
            .map(|value| value.clamp(60, 7 * 24 * 60 * 60))
            .unwrap_or(24 * 60 * 60);
        let query = FamilyTimelineQueryOptions::new(
            window_seconds,
            parse_query_param(raw_url, "camera_id"),
            parse_query_param(raw_url, "label"),
            limit,
        );
        let filter = FamilyMemoryEventFilter {
            include_hidden: parse_query_bool(raw_url, "include_hidden").unwrap_or(false),
            favorites_only: parse_query_bool(raw_url, "favorites_only").unwrap_or(false),
            hidden_only: parse_query_bool(raw_url, "hidden_only").unwrap_or(false),
            corrected_only: parse_query_bool(raw_url, "corrected_only").unwrap_or(false),
            limit,
        };
        let events = match list_recent_local_vision_events_default(limit) {
            Ok(events) => events,
            Err(error) => return error_json(StatusCode(500), &redact_admin_string(&error)),
        };
        let records =
            match list_family_memory_feedback_default(DEFAULT_FAMILY_MEMORY_FEEDBACK_LIMIT) {
                Ok(records) => records,
                Err(error) => return error_json(StatusCode(500), &redact_admin_string(&error)),
            };
        let overlays =
            harborbeacon_local_agent::runtime::family_memory::build_family_memory_overlays(
                &records,
            );
        let memory_overlay = build_family_memory_stats(&records);
        match build_family_memory_event_views(&events, &overlays, &filter, &query) {
            Ok(events) => ok_json(&FamilyMemoryEventsResponse {
                generated_at: now_unix_string(),
                limit,
                metadata_only: true,
                secret_scan: "clean".to_string(),
                memory_overlay,
                events,
            }),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_family_memory_event(
        &self,
        path: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        let event_id = match parse_family_memory_event_path(path) {
            Some(event_id) => event_id,
            None => return error_json(StatusCode(400), "invalid family memory event path"),
        };
        let event = match find_recent_local_vision_event(&event_id) {
            Ok(Some(event)) => event,
            Ok(None) => return error_json(StatusCode(404), "family memory event not found"),
            Err(error) => return error_json(StatusCode(500), &redact_admin_string(&error)),
        };
        let records =
            match list_family_memory_feedback_default(DEFAULT_FAMILY_MEMORY_FEEDBACK_LIMIT) {
                Ok(records) => records,
                Err(error) => return error_json(StatusCode(500), &redact_admin_string(&error)),
            };
        let overlays =
            harborbeacon_local_agent::runtime::family_memory::build_family_memory_overlays(
                &records,
            );
        let filter = FamilyMemoryEventFilter {
            include_hidden: true,
            limit: 1,
            ..FamilyMemoryEventFilter::default()
        };
        let query = FamilyTimelineQueryOptions::new(7 * 24 * 60 * 60, None, None, 1);
        match build_family_memory_event_views(&[event], &overlays, &filter, &query) {
            Ok(mut events) => match events.pop() {
                Some(event) => ok_json(&FamilyMemoryEventResponse {
                    generated_at: now_unix_string(),
                    metadata_only: true,
                    secret_scan: "clean".to_string(),
                    event,
                }),
                None => error_json(StatusCode(404), "family memory event not available"),
            },
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_family_memory_feedback(
        &self,
        path: &str,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        let principal = match self.authorize_admin_action(hints, AccessAction::AdminManage) {
            Ok(principal) => principal,
            Err(error) => return error_json(StatusCode(403), &error),
        };
        let event_id = match parse_family_memory_feedback_path(path) {
            Some(event_id) => event_id,
            None => return error_json(StatusCode(400), "invalid family memory feedback path"),
        };
        match find_recent_local_vision_event(&event_id) {
            Ok(Some(_)) => {}
            Ok(None) => return error_json(StatusCode(404), "family memory event not found"),
            Err(error) => return error_json(StatusCode(500), &redact_admin_string(&error)),
        }
        let body = match read_json_body_limited::<FamilyMemoryFeedbackRequest>(
            request,
            MAX_FAMILY_MEMORY_FEEDBACK_JSON_BYTES,
        ) {
            Ok(body) => body,
            Err(error) if error.contains("exceeds") => return error_json(StatusCode(413), &error),
            Err(error) => return error_json(StatusCode(400), &error),
        };
        let record = match append_family_memory_feedback_default(&event_id, body, "webui") {
            Ok(record) => record,
            Err(error) => return error_json(StatusCode(422), &redact_admin_string(&error)),
        };
        self.record_last_family_memory_feedback(&record);
        let evidence = compact_family_memory_feedback_evidence(&record);
        self.record_admin_audit(
            &principal,
            "family_memory_event",
            &event_id,
            "family_memory.feedback",
            json!({
                "action": record.action.as_str(),
                "has_corrected_summary": record.corrected_summary.is_some(),
                "corrected_label_count": record.corrected_labels.as_ref().map(Vec::len).unwrap_or(0),
                "metadata_only": true,
            }),
            evidence.clone(),
        );
        match family_memory_stats_default() {
            Ok(memory_overlay) => ok_json(&FamilyMemoryFeedbackResponse {
                status: "stored".to_string(),
                event_id,
                feedback: record,
                memory_overlay,
                evidence,
                metadata_only: true,
                secret_scan: "clean".to_string(),
            }),
            Err(error) => error_json(StatusCode(500), &redact_admin_string(&error)),
        }
    }

    fn handle_family_memory_stats(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        match family_memory_stats_default() {
            Ok(stats) => ok_json(&stats),
            Err(error) => error_json(StatusCode(500), &redact_admin_string(&error)),
        }
    }

    fn handle_home_guardian_activity(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        match self.admin_store.load_state() {
            Ok(state) => ok_json(&build_home_guardian_activity_response_with_activity(
                state.automation_reviews,
                self.load_home_guardian_activity_summaries(),
            )),
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_evaluate_automation_review_latest(
        &self,
        path: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        let principal = match self.authorize_admin_action(hints, AccessAction::ApprovalReview) {
            Ok(principal) => principal,
            Err(error) => return error_json(StatusCode(403), &error),
        };
        let review_id = match parse_automation_review_evaluate_latest_path(path) {
            Some(review_id) => review_id,
            None => return error_json(StatusCode(400), "invalid automation evaluate path"),
        };
        let event = match list_recent_local_vision_events_default(1) {
            Ok(mut events) => match events.pop() {
                Some(event) => event,
                None => return error_json(StatusCode(404), "no local vision event available"),
            },
            Err(error) => return error_json(StatusCode(500), &error),
        };
        match self.evaluate_home_guardian_rules_for_event(&event, Some(&review_id), true) {
            Ok(evaluation) => {
                let value = serde_json::to_value(&evaluation).unwrap_or_else(|_| json!({}));
                self.record_last_guardian_rule_evaluation(&value);
                self.record_admin_audit(
                    &principal,
                    "automation_review",
                    &review_id,
                    "home_guardian.evaluate_latest",
                    json!({"review_id": review_id, "latest_event_id": event.event.event_id}),
                    json!({
                        "status": evaluation.status,
                        "result_count": evaluation.results.len(),
                        "metadata_only": true,
                    }),
                );
                ok_json(&evaluation)
            }
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn evaluate_home_guardian_rules_for_event(
        &self,
        stored: &StoredLocalVisionEvent,
        only_review_id: Option<&str>,
        execute_active: bool,
    ) -> Result<HomeGuardianEvaluationResponse, String> {
        let automatic_evaluation = only_review_id.is_none();
        let reviews = if automatic_evaluation {
            self.cached_home_guardian_reviews_for_auto()?
        } else {
            let state = self.admin_store.load_state()?;
            select_home_guardian_reviews(state.automation_reviews, only_review_id)?
        };
        let mut results = Vec::new();
        for review in reviews {
            let Some(plan) = build_home_guardian_rule_evaluation_plan(
                review,
                stored,
                only_review_id.is_some(),
                execute_active,
            ) else {
                continue;
            };
            match plan {
                HomeGuardianRuleEvaluationPlan::Completed {
                    review,
                    summary,
                    persist_summary,
                } => {
                    if persist_summary && !automatic_evaluation {
                        let review = append_home_guardian_run_summary(review, summary.clone());
                        self.admin_store.upsert_automation_review(review)?;
                        self.append_home_guardian_activity_summary(&summary);
                    } else if persist_summary {
                        self.append_home_guardian_activity_summary(&summary);
                    }
                    results.push(summary);
                }
                HomeGuardianRuleEvaluationPlan::Execute { review, actions } => {
                    let mut action_results = Vec::new();
                    let mut cooldown_skipped = 0usize;
                    let now_epoch = current_epoch_secs();
                    for action_plan in actions {
                        if only_review_id.is_none()
                            && self.guardian_action_in_cooldown(
                                &review,
                                stored,
                                &action_plan.action_key,
                                now_epoch,
                            )
                        {
                            cooldown_skipped = cooldown_skipped.saturating_add(1);
                            continue;
                        }
                        if self.guardian_action_run_seen(&action_plan.idempotency_key, now_epoch)
                            || home_guardian_run_already_recorded(
                                &review,
                                &action_plan.idempotency_key,
                            )
                        {
                            action_results.push(home_guardian_idempotent_action_result(
                                &action_plan.action_key,
                                &action_plan.idempotency_key,
                            ));
                            continue;
                        }
                        let result = match action_plan.kind {
                            HomeGuardianActionExecutionKind::NotifyDefaultTarget => self
                                .execute_home_guardian_notify_action(
                                    stored,
                                    &action_plan.idempotency_key,
                                ),
                            HomeGuardianActionExecutionKind::HomeAssistantService => self
                                .execute_home_guardian_ha_action(
                                    &action_plan.action,
                                    &action_plan.idempotency_key,
                                ),
                            HomeGuardianActionExecutionKind::Unsupported => {
                                home_guardian_unsupported_action_result(
                                    &action_plan.action_key,
                                    &action_plan.idempotency_key,
                                )
                            }
                        };
                        let should_cooldown = !matches!(
                            result.get("status").and_then(Value::as_str),
                            Some("skipped")
                        );
                        if should_cooldown {
                            self.mark_guardian_action_cooldown(
                                &review,
                                stored,
                                &action_plan.action_key,
                                now_epoch,
                            );
                            self.mark_guardian_action_run_seen(
                                &action_plan.idempotency_key,
                                now_epoch,
                            );
                        }
                        action_results.push(result);
                    }

                    if action_results.is_empty() && cooldown_skipped > 0 {
                        let summary = home_guardian_rule_run_summary(
                            &review,
                            stored,
                            "skipped",
                            true,
                            false,
                            "Home Guardian action cooldown is active; automatic evaluation was not persisted.",
                            Vec::new(),
                        );
                        results.push(summary);
                        continue;
                    }

                    let summary =
                        complete_home_guardian_rule_execution(&review, stored, action_results);
                    if !automatic_evaluation {
                        let review = append_home_guardian_run_summary(review, summary.clone());
                        self.admin_store.upsert_automation_review(review)?;
                    }
                    self.append_home_guardian_activity_summary(&summary);
                    results.push(summary);
                }
            }
        }

        Ok(build_home_guardian_evaluation_response(stored, results))
    }

    fn execute_home_guardian_notify_action(
        &self,
        stored: &StoredLocalVisionEvent,
        idempotency_key: &str,
    ) -> Value {
        let state = match self.admin_store.load_state() {
            Ok(state) => state,
            Err(error) => {
                return json!({
                    "action": "notify_default_target",
                    "status": "failed",
                    "executed": false,
                    "idempotency_key": idempotency_key,
                    "reason": redact_admin_string(&error),
                    "redacted": true,
                })
            }
        };
        let Some(target) = default_notification_target_record(&state.notification_targets).cloned()
        else {
            let response = build_local_vision_event_notification_blocked_response(
                stored,
                None,
                "No default notification target is configured.",
            );
            self.record_last_event_notification_attempt(&response);
            return json!({
                "action": "notify_default_target",
                "status": "blocked",
                "executed": false,
                "idempotency_key": idempotency_key,
                "notification_status": response.status,
                "reason": response.message,
                "redacted": true,
            });
        };
        let intent = match build_local_vision_notification_intent(stored, &target.route_key) {
            Ok(intent) => intent,
            Err(error) => {
                let response = build_local_vision_event_notification_failed_response(
                    stored,
                    Some(&target),
                    None,
                    None,
                    &error,
                );
                self.record_last_event_notification_attempt(&response);
                return json!({
                    "action": "notify_default_target",
                    "status": "failed",
                    "executed": false,
                    "idempotency_key": idempotency_key,
                    "notification_status": response.status,
                    "reason": response.message,
                    "redacted": true,
                });
            }
        };
        let service = match NotificationDeliveryService::new() {
            Ok(service) => service,
            Err(error) => {
                let response = build_local_vision_event_notification_blocked_response(
                    stored,
                    Some(&target),
                    &redact_admin_string(&error),
                );
                self.record_last_event_notification_attempt(&response);
                return json!({
                    "action": "notify_default_target",
                    "status": "blocked",
                    "executed": false,
                    "idempotency_key": idempotency_key,
                    "notification_status": response.status,
                    "reason": response.message,
                    "redacted": true,
                });
            }
        };
        match service.deliver(&intent.notification_request) {
            Ok(record) if record.ok => {
                let response = build_local_vision_event_notification_delivered_response(
                    stored,
                    &target,
                    record,
                    intent.audit_record,
                );
                self.record_last_event_notification_attempt(&response);
                json!({
                    "action": "notify_default_target",
                    "status": "delivered",
                    "executed": true,
                    "idempotency_key": idempotency_key,
                    "notification_id": response.notification_id,
                    "delivery_id": response.delivery_id,
                    "target_label": response.target_label,
                    "redacted": true,
                })
            }
            Ok(record) => {
                let response = build_local_vision_event_notification_failed_response(
                    stored,
                    Some(&target),
                    Some(record),
                    None,
                    "HarborGate returned a failed delivery record.",
                );
                self.record_last_event_notification_attempt(&response);
                json!({
                    "action": "notify_default_target",
                    "status": "failed",
                    "executed": false,
                    "idempotency_key": idempotency_key,
                    "notification_status": response.status,
                    "reason": response.message,
                    "redacted": true,
                })
            }
            Err(error) => {
                let response = build_local_vision_event_notification_delivery_error_response(
                    stored, &target, error,
                );
                self.record_last_event_notification_attempt(&response);
                json!({
                    "action": "notify_default_target",
                    "status": response.status,
                    "executed": false,
                    "idempotency_key": idempotency_key,
                    "reason": response.message,
                    "redacted": true,
                })
            }
        }
    }

    fn execute_home_guardian_ha_action(&self, action: &Value, idempotency_key: &str) -> Value {
        let guardian_request = home_guardian_ha_service_request(action);
        let request = HomeAssistantServiceActionRequest {
            entity_id: guardian_request.entity_id,
            domain: guardian_request.domain,
            service: guardian_request.service,
            fields: json!({}),
        };
        let state = match self.admin_store.home_assistant_secret_state() {
            Ok(state) => state,
            Err(error) => {
                return json!({
                    "action": "ha_service_action",
                    "status": "failed",
                    "executed": false,
                    "idempotency_key": idempotency_key,
                    "reason": redact_admin_string(&error),
                    "redacted": true,
                })
            }
        };
        let action_id = format!("ha_action_{}", Uuid::new_v4().simple());
        let normalized = normalize_home_assistant_service_smoke_request(&request);
        if let Err(message) = validate_home_assistant_service_smoke(&normalized, &state) {
            let response = HomeAssistantServiceActionResponse {
                action_id,
                status: "blocked".to_string(),
                allowed: false,
                executed: false,
                domain: normalized.domain,
                service: normalized.service,
                entity_id: normalized.entity_id,
                message: message.clone(),
                result: None,
                audit_record: build_home_assistant_operator_audit(
                    "home_assistant.guardian_service_action_blocked",
                    "blocked",
                    false,
                    false,
                    &message,
                    &request,
                ),
            };
            self.record_last_home_assistant_service_action(&response);
            return json!({
                "action": "ha_service_action",
                "status": "blocked",
                "executed": false,
                "idempotency_key": idempotency_key,
                "domain": response.domain,
                "service": response.service,
                "entity_id": response.entity_id,
                "reason": response.message,
                "redacted": true,
            });
        }
        let client = match home_assistant_client_from_state(&state) {
            Ok(client) => client,
            Err(error) => {
                return json!({
                    "action": "ha_service_action",
                    "status": "blocked",
                    "executed": false,
                    "idempotency_key": idempotency_key,
                    "domain": normalized.domain,
                    "service": normalized.service,
                    "entity_id": normalized.entity_id,
                    "reason": redact_admin_string(&error),
                    "redacted": true,
                })
            }
        };
        match client.call_service(
            &normalized.domain,
            &normalized.service,
            &normalized.entity_id,
            Some(&json!({})),
        ) {
            Ok(result) => {
                let response = HomeAssistantServiceActionResponse {
                    action_id,
                    status: "succeeded".to_string(),
                    allowed: true,
                    executed: true,
                    domain: normalized.domain,
                    service: normalized.service,
                    entity_id: normalized.entity_id,
                    message: "Home Guardian low-risk service action completed.".to_string(),
                    result: Some(result),
                    audit_record: build_home_assistant_operator_audit(
                        "home_assistant.guardian_service_action_executed",
                        "succeeded",
                        true,
                        true,
                        "Home Guardian low-risk service action completed.",
                        &request,
                    ),
                };
                self.record_last_home_assistant_service_action(&response);
                json!({
                    "action": "ha_service_action",
                    "status": "acted",
                    "executed": true,
                    "idempotency_key": idempotency_key,
                    "domain": response.domain,
                    "service": response.service,
                    "entity_id": response.entity_id,
                    "redacted": true,
                })
            }
            Err(error) => {
                let message = redact_admin_string(&error);
                let response = HomeAssistantServiceActionResponse {
                    action_id,
                    status: "failed".to_string(),
                    allowed: true,
                    executed: false,
                    domain: normalized.domain,
                    service: normalized.service,
                    entity_id: normalized.entity_id,
                    message: message.clone(),
                    result: None,
                    audit_record: build_home_assistant_operator_audit(
                        "home_assistant.guardian_service_action_failed",
                        "failed",
                        true,
                        false,
                        &message,
                        &request,
                    ),
                };
                self.record_last_home_assistant_service_action(&response);
                json!({
                    "action": "ha_service_action",
                    "status": "failed",
                    "executed": false,
                    "idempotency_key": idempotency_key,
                    "domain": response.domain,
                    "service": response.service,
                    "entity_id": response.entity_id,
                    "reason": response.message,
                    "redacted": true,
                })
            }
        }
    }

    fn handle_notify_local_vision_event(
        &self,
        path: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        let principal = match self.authorize_admin_action(hints, AccessAction::AdminManage) {
            Ok(principal) => principal,
            Err(error) => return error_json(StatusCode(403), &error),
        };
        let event_id = match parse_local_vision_event_notify_path(path) {
            Some(event_id) => event_id,
            None => return error_json(StatusCode(400), "invalid local vision event notify path"),
        };
        let event = match find_recent_local_vision_event(&event_id) {
            Ok(Some(event)) => event,
            Ok(None) => return error_json(StatusCode(404), "local vision event not found"),
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let state = match self.admin_store.load_state() {
            Ok(state) => state,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let target = match default_notification_target_record(&state.notification_targets).cloned()
        {
            Some(target) => target,
            None => {
                let response = build_local_vision_event_notification_blocked_response(
                    &event,
                    None,
                    "No default notification target is configured.",
                );
                self.record_last_event_notification_attempt(&response);
                self.record_local_vision_notify_audit(&principal, &response);
                return ok_json(&response);
            }
        };
        let intent = match build_local_vision_notification_intent(&event, &target.route_key) {
            Ok(intent) => intent,
            Err(error) => {
                let response = build_local_vision_event_notification_failed_response(
                    &event,
                    Some(&target),
                    None,
                    None,
                    &error,
                );
                self.record_last_event_notification_attempt(&response);
                self.record_local_vision_notify_audit(&principal, &response);
                return ok_json(&response);
            }
        };
        let service = match NotificationDeliveryService::new() {
            Ok(service) => service,
            Err(error) => {
                let response = build_local_vision_event_notification_blocked_response(
                    &event,
                    Some(&target),
                    &redact_admin_string(&error),
                );
                self.record_last_event_notification_attempt(&response);
                self.record_local_vision_notify_audit(&principal, &response);
                return ok_json(&response);
            }
        };
        match service.deliver(&intent.notification_request) {
            Ok(record) if record.ok => {
                let response = build_local_vision_event_notification_delivered_response(
                    &event,
                    &target,
                    record,
                    intent.audit_record,
                );
                self.record_last_event_notification_attempt(&response);
                self.record_local_vision_notify_audit(&principal, &response);
                ok_json(&response)
            }
            Ok(record) => {
                let response = build_local_vision_event_notification_failed_response(
                    &event,
                    Some(&target),
                    Some(record),
                    None,
                    "HarborGate returned a failed delivery record.",
                );
                self.record_last_event_notification_attempt(&response);
                self.record_local_vision_notify_audit(&principal, &response);
                ok_json(&response)
            }
            Err(error) => {
                let response = build_local_vision_event_notification_delivery_error_response(
                    &event, &target, error,
                );
                self.record_last_event_notification_attempt(&response);
                self.record_local_vision_notify_audit(&principal, &response);
                ok_json(&response)
            }
        }
    }

    fn handle_model_policies(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        match self.admin_store.load_state() {
            Ok(state) => ok_json(&ModelPoliciesResponse {
                route_policies: state.models.route_policies,
            }),
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_models_page(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        let mut response =
            Response::from_string(render_models_admin_page()).with_status_code(StatusCode(200));
        add_common_headers(&mut response);
        response.add_header(
            Header::from_bytes(
                b"Content-Type".as_slice(),
                b"text/html; charset=utf-8".as_slice(),
            )
            .expect("header"),
        );
        response
    }

    fn handle_create_model_endpoint(
        &self,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        let endpoint: ModelEndpoint = match read_json_body(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };
        match self.admin_store.save_model_endpoint(endpoint) {
            Ok(state) => ok_json(&redact_model_endpoint_response(&state.models.endpoints)),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_patch_model_endpoint(
        &self,
        path: &str,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        let endpoint_id = match parse_model_endpoint_path(path) {
            Some(endpoint_id) => endpoint_id,
            None => return error_json(StatusCode(400), "invalid model endpoint path"),
        };
        let patch: Value = match read_json_body(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };
        match self.admin_store.patch_model_endpoint(&endpoint_id, patch) {
            Ok(state) => ok_json(&redact_model_endpoint_response(&state.models.endpoints)),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_test_model_endpoint(
        &self,
        path: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        let principal = match self.authorize_admin_action(hints, AccessAction::AdminManage) {
            Ok(principal) => principal,
            Err(error) => return error_json(StatusCode(403), &error),
        };
        let endpoint_id = match parse_model_endpoint_test_path(path) {
            Some(endpoint_id) => endpoint_id,
            None => return error_json(StatusCode(400), "invalid model endpoint test path"),
        };
        let state = match self.admin_store.load_state() {
            Ok(state) => state,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let Some(endpoint) = state
            .models
            .endpoints
            .iter()
            .find(|endpoint| endpoint.model_endpoint_id == endpoint_id)
        else {
            return error_json(StatusCode(404), &format!("未找到模型端点 {endpoint_id}"));
        };
        let result: ModelEndpointTestResult = test_model_endpoint(endpoint);
        if let Err(error) = self.admin_store.record_model_endpoint_test_result(
            &endpoint_id,
            result.ok,
            &result.status,
            &result.summary,
            result.details.clone(),
        ) {
            return error_json(StatusCode(500), &error);
        }
        self.record_admin_audit(
            &principal,
            "model_endpoint",
            &endpoint_id,
            "model.endpoint.test",
            json!({"endpoint_id": endpoint_id}),
            json!({
                "ok": result.ok,
                "status": result.status,
                "summary": result.summary,
                "endpoint_kind": result.endpoint.endpoint_kind.as_str(),
            }),
        );
        ok_json(&result)
    }

    fn handle_create_model_download(
        &self,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        let body: ModelDownloadRequest = match read_json_body(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };
        let state = match self.admin_store.load_state() {
            Ok(state) => state,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let catalog = build_local_model_catalog_for_model_state(&state.models, Vec::new());
        let catalog_item = catalog
            .models
            .iter()
            .find(|item| item.model_id == body.model_id);
        let display_name = body
            .display_name
            .as_deref()
            .and_then(non_empty_string)
            .or_else(|| catalog_item.map(|item| item.display_name.clone()))
            .unwrap_or_else(|| body.model_id.clone());
        let provider_key = body
            .provider_key
            .as_deref()
            .and_then(non_empty_string)
            .or_else(|| catalog_item.map(|item| item.provider_key.clone()))
            .unwrap_or_else(|| "local".to_string());
        let target_path = body
            .target_path
            .as_deref()
            .and_then(non_empty_string)
            .or_else(|| catalog_item.and_then(|item| item.local_path.clone()))
            .or_else(|| {
                Some(default_model_download_target_path_for_model_state(
                    &state.models,
                    &body.model_id,
                ))
            });
        let mut metadata = if body.metadata.is_null() {
            json!({})
        } else {
            body.metadata
        };
        if let Some(catalog_item) = catalog_item {
            enrich_model_download_metadata(&mut metadata, catalog_item);
        }
        if let Some(hf_endpoint) = body.hf_endpoint.as_deref().and_then(non_empty_string) {
            if !metadata.is_object() {
                metadata = json!({});
            }
            if let Some(object) = metadata.as_object_mut() {
                object.insert("hf_endpoint".to_string(), json!(hf_endpoint));
            }
        }
        if let Some(capability_id) = body.capability_id.as_deref().and_then(non_empty_string) {
            if !metadata.is_object() {
                metadata = json!({});
            }
            if let Some(object) = metadata.as_object_mut() {
                object.insert("capability_id".to_string(), json!(capability_id));
            }
        }
        match self.admin_store.create_or_update_model_download_job(
            &body.model_id,
            &display_name,
            &provider_key,
            target_path,
            redact_secret_json_value(metadata),
        ) {
            Ok(result) => {
                let job = result.job;
                if !result.should_spawn_worker {
                    return ok_json(&ModelDownloadJobResponse::new(job));
                }
                let worker_store = AdminConsoleStore::new(
                    self.admin_store.path().to_path_buf(),
                    self.admin_store.registry_store().clone(),
                );
                match spawn_model_download_worker(worker_store, job.clone()) {
                    Ok(()) => ok_json(&ModelDownloadJobResponse::new(job)),
                    Err(error) => {
                        let _ =
                            mark_model_download_spawn_failed(&self.admin_store, job, error.clone());
                        error_json(StatusCode(500), &error)
                    }
                }
            }
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_update_model_store(
        &self,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        let body: ModelStoreUpdateRequest = match read_json_body(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };
        match self.admin_store.save_model_store_root(&body.path) {
            Ok(state) => ok_json(&build_model_store_status(&state.models)),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_install_model_runtime(
        &self,
        path: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        let runtime_id = match parse_model_runtime_install_path(path) {
            Some(runtime_id) => runtime_id,
            None => return error_json(StatusCode(400), "invalid model runtime install path"),
        };
        let runtime = match self.admin_store.install_model_runtime(&runtime_id) {
            Ok(runtime) => runtime,
            Err(error) => return error_json(StatusCode(422), &error),
        };
        let state = match self.admin_store.load_or_create_state() {
            Ok(state) => state,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let runtime_projection = probe_local_model_runtime(&state.models.endpoints);
        let manager = build_model_runtime_manager_response(&state.models, &runtime_projection);
        let runtime_status = manager
            .runtimes
            .iter()
            .find(|candidate| candidate.record.runtime_id == runtime.runtime_id)
            .cloned()
            .unwrap_or_else(|| model_runtime_status(runtime, &runtime_projection));
        ok_json(&ModelRuntimeInstallResponse {
            message: format!(
                "{} is enabled for Harbor-managed models.",
                runtime_status.record.display_name
            ),
            runtime: runtime_status,
            runtime_manager: manager,
        })
    }

    fn handle_select_model_capability(
        &self,
        path: &str,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        let principal = match self.authorize_admin_action(hints, AccessAction::AdminManage) {
            Ok(principal) => principal,
            Err(error) => return error_json(StatusCode(403), &error),
        };
        let capability_id = match parse_model_capability_selection_path(path) {
            Some(capability_id) => capability_id,
            None => return error_json(StatusCode(400), "invalid model capability selection path"),
        };
        let body: ModelCapabilitySelectionRequest = match read_json_body(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };
        if let Err(error) = self.activate_selected_model_capability(&capability_id, &body.model_id)
        {
            return error_json(StatusCode(422), &error);
        }
        if let Err(error) = self
            .admin_store
            .save_model_capability_binding(&capability_id, &body.model_id)
        {
            return error_json(StatusCode(422), &error);
        }
        self.record_admin_audit(
            &principal,
            "model_capability",
            &capability_id,
            "model.capability.select",
            json!({"capability_id": capability_id}),
            json!({"status": "selected", "model_id": body.model_id}),
        );
        self.handle_model_capabilities(hints)
    }

    fn activate_selected_model_capability(
        &self,
        capability_id: &str,
        model_id: &str,
    ) -> Result<(), String> {
        let Some(capability_model_kind) = model_kind_for_capability(capability_id) else {
            return Ok(());
        };
        let state = self.admin_store.load_or_create_state()?;
        let download_jobs = self.admin_store.list_model_download_jobs()?;
        let catalog = build_local_model_catalog_for_model_state(&state.models, download_jobs);
        let model = catalog
            .models
            .iter()
            .find(|model| model.model_id == model_id)
            .ok_or_else(|| format!("未找到模型 {model_id}"))?;
        if !model.installed {
            return Err(format!(
                "模型 {} 尚未下载完成，不能启动",
                model.display_name
            ));
        }
        if !catalog_model_matches_kind_or_capability(model, capability_model_kind, capability_id) {
            return Err(format!(
                "模型 {} 不适用于 {} 能力",
                model.display_name, capability_id
            ));
        }
        if let Some(profile) = managed_runtime_profile_for_model(model) {
            let runtime_projection = LocalModelRuntimeProjection::default();
            let runtime_status =
                model_runtime_status_for_profile(&state.models, &runtime_projection, profile)
                    .ok_or_else(|| format!("未找到 Harbor-managed runtime profile {profile}"))?;
            if !runtime_status.installed {
                return Err(format!(
                    "请先安装 {}，再选择 {}",
                    runtime_status.record.display_name, model.display_name
                ));
            }
        } else if external_runtime_profile_for_model(model) {
            return Err(format!(
                "模型 {} 需要在高级设置配置 OpenAI-compatible runtime；Harbor 不会自动启动或接管外部 runtime",
                model.display_name
            ));
        }

        let Some(model_kind) = runtime_model_kind_for_capability(capability_id) else {
            return Ok(());
        };

        let request = ModelRuntimeActivationRequest {
            capability_id: capability_id.to_string(),
            model_id: model.model_id.clone(),
            model_kind,
            local_path: model.local_path.clone(),
            runtime_profiles: model.runtime_profiles.clone(),
        };
        let result = if let Some(handler) = self.model_runtime_activation.as_ref() {
            handler(request)?
        } else {
            ModelRuntimeActivationResult {
                activated: false,
                status: "activation_unavailable".to_string(),
                message: "当前服务未提供自动启动入口".to_string(),
                runtime_model_id: None,
            }
        };
        self.record_model_runtime_activation(model_kind, model, result)
    }

    fn record_model_runtime_activation(
        &self,
        model_kind: ModelKind,
        model: &LocalModelCatalogItem,
        result: ModelRuntimeActivationResult,
    ) -> Result<(), String> {
        let endpoint_id = preferred_endpoint_id_for_model_kind(model_kind);
        let mut metadata = json!({
            "catalog_model_id": model.model_id,
            "local_path": model.local_path,
            "activation_status": result.status,
            "activation_message": result.message,
            "activation_requested_at": now_unix_string(),
            "runtime_auto_activation": true,
            "runtime_profiles": model.runtime_profiles.clone(),
        });
        if let Some(runtime_model_id) = result.runtime_model_id.as_ref() {
            set_metadata_string(&mut metadata, "runtime_model_id", runtime_model_id.clone());
        }
        let model_name = result
            .runtime_model_id
            .clone()
            .unwrap_or_else(|| model.model_id.clone());
        let status = if result.activated {
            ModelEndpointStatus::Active
        } else {
            ModelEndpointStatus::Degraded
        };
        self.admin_store.patch_model_endpoint(
            endpoint_id,
            json!({
                "model_name": model_name,
                "status": status,
                "metadata": metadata,
            }),
        )?;
        Ok(())
    }

    fn handle_cancel_model_download(
        &self,
        path: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        let job_id = match parse_model_download_cancel_path(path) {
            Some(job_id) => job_id,
            None => return error_json(StatusCode(400), "invalid model download cancel path"),
        };
        match self.admin_store.cancel_model_download_job(&job_id) {
            Ok(Some(job)) => ok_json(&ModelDownloadJobResponse::new(job)),
            Ok(None) => error_json(StatusCode(404), "model download job not found"),
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_save_model_policies(
        &self,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        let principal = match self.authorize_admin_action(hints, AccessAction::AdminManage) {
            Ok(principal) => principal,
            Err(error) => return error_json(StatusCode(403), &error),
        };
        let payload: ModelPoliciesRequest = match read_json_body(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };
        let requested_count = payload.route_policies.len();
        match self
            .admin_store
            .save_model_route_policies(payload.route_policies)
        {
            Ok(state) => {
                self.record_admin_audit(
                    &principal,
                    "model_route_policy",
                    "bulk",
                    "model.route_policy.save",
                    json!({"requested_policy_count": requested_count}),
                    json!({
                        "status": "saved",
                        "route_policy_count": state.models.route_policies.len(),
                    }),
                );
                ok_json(&ModelPoliciesResponse {
                    route_policies: state.models.route_policies,
                })
            }
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_access_members(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }

        let state = match self.admin_store.load_state() {
            Ok(state) => state,
            Err(error) => return error_json(StatusCode(500), &error),
        };

        ok_json(&build_access_member_summaries(&state))
    }

    fn handle_share_links(
        &self,
        url: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }

        let device_filter = parse_query_param(url, "device_id")
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

        match self.list_share_links(device_filter.as_deref()) {
            Ok(payload) => ok_json(&payload),
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_pending_approvals(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::ApprovalReview) {
            return error_json(StatusCode(403), &error);
        }
        match self.task_service.pending_approvals() {
            Ok(payload) => ok_json(&payload),
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_approve_approval(
        &self,
        path: &str,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        let principal = match self.authorize_admin_action(hints, AccessAction::ApprovalReview) {
            Ok(principal) => principal,
            Err(error) => return error_json(StatusCode(403), &error),
        };
        let approval_id = match parse_approval_decision_path(path, "approve") {
            Some(approval_id) => approval_id,
            None => return error_json(StatusCode(400), "invalid approval approve path"),
        };
        let body: ApprovalDecisionRequest = match read_json_body_or_default(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };
        let approver_user_id = body
            .approver_user_id
            .filter(|user_id| user_id == &principal.user_id)
            .or_else(|| Some(principal.user_id.clone()));

        match self
            .task_service
            .approve_pending_approval(&approval_id, approver_user_id)
        {
            Ok((approval, task_response)) => ok_json(&ApprovalDecisionResponse {
                approval,
                task_response: Some(task_response),
            }),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_reject_approval(
        &self,
        path: &str,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        let principal = match self.authorize_admin_action(hints, AccessAction::ApprovalReview) {
            Ok(principal) => principal,
            Err(error) => return error_json(StatusCode(403), &error),
        };
        let approval_id = match parse_approval_decision_path(path, "reject") {
            Some(approval_id) => approval_id,
            None => return error_json(StatusCode(400), "invalid approval reject path"),
        };
        let body: ApprovalDecisionRequest = match read_json_body_or_default(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };
        let approver_user_id = body
            .approver_user_id
            .filter(|user_id| user_id == &principal.user_id)
            .or_else(|| Some(principal.user_id.clone()));

        match self
            .task_service
            .reject_pending_approval(&approval_id, approver_user_id)
        {
            Ok(approval) => ok_json(&ApprovalDecisionResponse {
                approval,
                task_response: None,
            }),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_automation_reviews(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::ApprovalReview) {
            return error_json(StatusCode(403), &error);
        }
        match self.admin_store.load_state() {
            Ok(state) => ok_json(&build_automation_reviews_response(state.automation_reviews)),
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_create_automation_review(
        &self,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::ApprovalReview) {
            return error_json(StatusCode(403), &error);
        }
        let body: AutomationReviewRequest = match read_json_body(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };
        let review = AutomationRuleReview {
            review_id: body.review_id.unwrap_or_default(),
            workspace_id: body.workspace_id.unwrap_or_default(),
            source: body
                .source
                .unwrap_or_else(|| "harbor_assistant".to_string()),
            source_channel: body.source_channel,
            source_conversation_id: body.source_conversation_id,
            original_prompt: body.original_prompt,
            status: body.status.unwrap_or_else(|| "draft".to_string()),
            trigger_definition: body.trigger_definition,
            condition_definition: body.condition_definition,
            action_plan: body.action_plan,
            device_refs: body.device_refs,
            risk_level: body.risk_level,
            requires_approval: body.requires_approval,
            created_at: None,
            updated_at: None,
            expires_at: body.expires_at,
            rule_id: body.rule_id,
            run_summaries: body.run_summaries,
            metadata: body.metadata,
        };
        let is_home_guardian_review = automation_review_is_home_guardian(&review);
        let proposal_evidence = if is_home_guardian_review {
            Some(json!({
                "status": "proposed",
                "review_id": review.review_id,
                "rule_id": review.rule_id,
                "trigger": review.trigger_definition,
                "action_plan": home_guardian_redacted_action_plan(review.action_plan.as_ref()),
                "requires_enable": true,
                "metadata_only": true,
                "secret_scan": "clean",
                "created_at": now_unix_string(),
            }))
        } else {
            None
        };
        match self
            .admin_store
            .upsert_automation_review(review)
            .map(|state| build_automation_reviews_response(state.automation_reviews))
        {
            Ok(payload) => {
                if let Some(proposal_evidence) = proposal_evidence.as_ref() {
                    self.record_last_guardian_rule_proposal(proposal_evidence);
                }
                if is_home_guardian_review {
                    self.refresh_home_guardian_rule_cache_best_effort();
                }
                ok_json(&payload)
            }
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_update_automation_review_status(
        &self,
        path: &str,
        status: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        let principal = match self.authorize_admin_action(hints, AccessAction::ApprovalReview) {
            Ok(principal) => principal,
            Err(error) => return error_json(StatusCode(403), &error),
        };
        let review_id = match parse_automation_review_action_path(path) {
            Some(review_id) => review_id,
            None => return error_json(StatusCode(400), "invalid automation review path"),
        };
        match self
            .admin_store
            .set_automation_review_status(&review_id, status)
            .map(|state| build_automation_reviews_response(state.automation_reviews))
        {
            Ok(payload) => {
                self.refresh_home_guardian_rule_cache_best_effort();
                self.record_admin_audit(
                    &principal,
                    "automation_review",
                    &review_id,
                    "home_guardian.status.update",
                    json!({"review_id": review_id}),
                    json!({"status": status, "metadata_only": true}),
                );
                ok_json(&payload)
            }
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_update_member_role(
        &self,
        path: &str,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }

        let user_id = match parse_member_role_update_path(path) {
            Some(user_id) => user_id,
            None => return error_json(StatusCode(400), "invalid member role path"),
        };
        let body: MembershipRoleUpdateRequest = match read_json_body(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };
        let role_kind = match parse_role_kind(&body.role_kind) {
            Ok(role_kind) => role_kind,
            Err(error) => return error_json(StatusCode(400), &error),
        };

        match self
            .admin_store
            .set_member_role(&user_id, role_kind)
            .map(|state| build_access_member_summaries(&state))
        {
            Ok(payload) => ok_json(&payload),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_update_member_default_delivery_surface(
        &self,
        path: &str,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }

        let user_id = match parse_member_default_delivery_surface_update_path(path) {
            Some(user_id) => user_id,
            None => {
                return error_json(
                    StatusCode(400),
                    "invalid member default delivery surface path",
                )
            }
        };
        let body: DefaultDeliverySurfaceUpdateRequest = match read_json_body(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };
        let surface = match normalize_delivery_surface(&body.surface) {
            Some(surface) => surface,
            None => return error_json(StatusCode(400), "surface 只能是 feishu 或 weixin"),
        };

        match self
            .admin_store
            .set_member_default_delivery_surface(&user_id, &surface)
            .map(|state| build_access_member_summaries(&state))
        {
            Ok(payload) => ok_json(&payload),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_notification_targets(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminReadState) {
            return error_json(StatusCode(403), &error);
        }
        match self.admin_store.load_state() {
            Ok(state) => ok_json(&NotificationTargetsResponse {
                targets: state.notification_targets,
            }),
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_upsert_notification_target(
        &self,
        request: &mut Request,
        headers: &[Header],
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = authorize_gateway_service_request(headers) {
            return error_json(StatusCode(401), &error);
        }

        let body: NotificationTargetUpsertRequest = match read_json_body(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };
        match self
            .admin_store
            .upsert_notification_target(
                body.target_id.as_deref(),
                &body.label,
                &body.route_key,
                &body.platform_hint,
                body.is_default,
            )
            .map(|state| NotificationTargetsResponse {
                targets: state.notification_targets,
            }) {
            Ok(payload) => ok_json(&payload),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_set_default_notification_target(
        &self,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        let body: NotificationTargetDefaultRequest = match read_json_body(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };
        match self
            .admin_store
            .set_default_notification_target(&body.target_id)
            .map(|state| NotificationTargetsResponse {
                targets: state.notification_targets,
            }) {
            Ok(payload) => ok_json(&payload),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_delete_notification_target(
        &self,
        path: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        let target_id = match parse_notification_target_delete_path(path) {
            Some(target_id) => target_id,
            None => return error_json(StatusCode(400), "invalid notification target path"),
        };
        match self.admin_store.delete_notification_target(&target_id) {
            Ok(_) => no_content(),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_binding_qr_svg(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        deprecated_im_binding_response_json(&self.current_gateway_manage_url())
    }

    fn handle_static_binding_qr_svg(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        deprecated_im_binding_response_json(&self.current_gateway_manage_url())
    }

    fn handle_mobile_setup_page(
        &self,
        _url: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        deprecated_im_binding_response_html(&self.current_gateway_manage_url())
    }

    fn handle_live_view_page(
        &self,
        url: &str,
        path: &str,
        remote_addr: Option<SocketAddr>,
        headers: &[Header],
        hints: &AccessIdentityHints,
    ) -> Response<Cursor<Vec<u8>>> {
        if let Err(error) = ensure_local_camera_access(remote_addr, headers) {
            return error_json(StatusCode(403), &error);
        }

        let device_id = match parse_camera_live_page_path(path) {
            Some(device_id) => device_id,
            None => return error_json(StatusCode(400), "invalid live view page path"),
        };

        let device = match self.load_camera_device(&device_id) {
            Ok(device) => device,
            Err(error) if error.contains("device not found") => {
                return error_json(StatusCode(404), &error)
            }
            Err(error) => return error_json(StatusCode(422), &error),
        };

        if let Err(error) =
            self.authorize_camera_action(hints, &device_id, AccessAction::CameraView)
        {
            return error_json(StatusCode(403), &error);
        }

        let body = render_live_view_page(&self.public_origin, &device, &identity_query_suffix(url));
        let mut response = Response::from_string(body).with_status_code(StatusCode(200));
        add_common_headers(&mut response);
        response.add_header(
            Header::from_bytes(
                b"Content-Type".as_slice(),
                b"text/html; charset=utf-8".as_slice(),
            )
            .expect("header"),
        );
        response
    }

    fn handle_shared_live_view_page(&self, path: &str) -> Response<Cursor<Vec<u8>>> {
        let token = match parse_shared_camera_live_page_path(path) {
            Some(token) => token,
            None => return error_json(StatusCode(400), "invalid shared live view path"),
        };
        let claims = match self.verify_shared_camera_token(&token) {
            Ok(claims) => claims,
            Err(error) => return error_json(StatusCode(403), &error),
        };
        let device = match self.load_camera_device(&claims.device_id) {
            Ok(device) => device,
            Err(error) if error.contains("device not found") => {
                return error_json(StatusCode(404), &error)
            }
            Err(error) => return error_json(StatusCode(422), &error),
        };

        let body = render_shared_live_view_page(&token, &device);
        let mut response = Response::from_string(body).with_status_code(StatusCode(200));
        add_common_headers(&mut response);
        response.add_header(
            Header::from_bytes(
                b"Content-Type".as_slice(),
                b"text/html; charset=utf-8".as_slice(),
            )
            .expect("header"),
        );
        response
    }

    fn handle_refresh_binding(
        &self,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        deprecated_im_binding_response_json(&self.current_gateway_manage_url())
    }

    fn handle_demo_bind(&self, hints: &AccessIdentityHints) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        deprecated_im_binding_response_json(&self.current_gateway_manage_url())
    }

    fn handle_test_bind(
        &self,
        _request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        deprecated_im_binding_response_json(&self.current_gateway_manage_url())
    }

    fn handle_configure_bridge(
        &self,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        let body: BridgeConfigRequest = match read_json_body(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };

        let _ = body;

        match self
            .hub()
            .refresh_bridge_provider_status(Some(&self.public_origin))
        {
            Ok(payload) => ok_json(&payload),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_scan(
        &self,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        let principal = match self.authorize_workspace_camera_action(hints) {
            Ok(principal) => principal,
            Err(error) => return error_json(StatusCode(403), &error),
        };
        let body: ScanRequest = match read_json_body(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };

        match self.scan(&principal, body) {
            Ok(payload) => ok_json(&payload),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_manual_add(
        &self,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        let principal = match self.authorize_workspace_camera_action(hints) {
            Ok(principal) => principal,
            Err(error) => return error_json(StatusCode(403), &error),
        };
        let body: ManualAddRequest = match read_json_body(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };

        match self.manual_add(&principal, body) {
            Ok(payload) => ok_json(&payload),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_set_default_camera(
        &self,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        let body: DefaultCameraRequest = match read_json_body(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };

        let requested_device_id = body.device_id.and_then(|value| non_empty_string(&value));
        if let Some(device_id) = requested_device_id.as_deref() {
            if let Err(error) = self.load_camera_device(device_id) {
                return if error.contains("device not found") {
                    error_json(StatusCode(404), &error)
                } else {
                    error_json(StatusCode(422), &error)
                };
            }
        }

        let mut state = match self.admin_store.load_or_create_state() {
            Ok(state) => state,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        state.defaults.selected_camera_device_id = requested_device_id;
        match self
            .hub()
            .save_defaults(state.defaults, Some(&self.public_origin))
        {
            Ok(payload) => ok_json(&redact_state_snapshot(payload)),
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_patch_device_metadata(
        &self,
        path: &str,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        let device_id = match parse_device_metadata_patch_path(path) {
            Some(device_id) => device_id,
            None => return error_json(StatusCode(400), "invalid device metadata patch path"),
        };
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        let body: DeviceMetadataPatchRequest = match read_json_body(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };
        match self.patch_device_metadata(&device_id, body) {
            Ok(payload) => ok_json(&redact_state_snapshot(payload)),
            Err(error) if error.contains("device not found") => error_json(StatusCode(404), &error),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_delete_device(
        &self,
        path: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        let device_id = match parse_device_metadata_path(path) {
            Some(device_id) => device_id,
            None => return error_json(StatusCode(400), "invalid device delete path"),
        };
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        match self.delete_device(&device_id) {
            Ok(payload) => ok_json(&redact_state_snapshot(payload)),
            Err(error) if error.contains("device not found") => error_json(StatusCode(404), &error),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_device_credential_status(
        &self,
        path: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        let device_id = match parse_device_credential_status_path(path) {
            Some(device_id) => device_id,
            None => return error_json(StatusCode(400), "invalid device credential-status path"),
        };
        if let Err(error) =
            self.authorize_camera_action(hints, &device_id, AccessAction::CameraView)
        {
            return error_json(StatusCode(403), &error);
        }
        let device = match self.load_camera_device(&device_id) {
            Ok(device) => device,
            Err(error) if error.contains("device not found") => {
                return error_json(StatusCode(404), &error)
            }
            Err(error) => return error_json(StatusCode(422), &error),
        };
        match self.admin_store.load_state() {
            Ok(state) => ok_json(&build_device_credential_status(&state, &device)),
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_device_evidence(
        &self,
        path: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        let device_id = match parse_device_evidence_path(path) {
            Some(device_id) => device_id,
            None => return error_json(StatusCode(400), "invalid device evidence path"),
        };
        if let Err(error) =
            self.authorize_camera_action(hints, &device_id, AccessAction::CameraView)
        {
            return error_json(StatusCode(403), &error);
        }
        let device = match self.load_camera_device(&device_id) {
            Ok(device) => device,
            Err(error) if error.contains("device not found") => {
                return error_json(StatusCode(404), &error)
            }
            Err(error) => return error_json(StatusCode(422), &error),
        };
        match self.build_device_evidence_response(&device) {
            Ok(payload) => ok_json(&payload),
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_save_device_credentials(
        &self,
        path: &str,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        let device_id = match parse_device_credentials_path(path) {
            Some(device_id) => device_id,
            None => return error_json(StatusCode(400), "invalid device credentials path"),
        };
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        let device = match self.load_camera_device(&device_id) {
            Ok(device) => device,
            Err(error) if error.contains("device not found") => {
                return error_json(StatusCode(404), &error)
            }
            Err(error) => return error_json(StatusCode(422), &error),
        };
        let body: DeviceCredentialsRequest = match read_json_body(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };

        let state = match self.admin_store.load_state() {
            Ok(state) => state,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let existing = state
            .device_credentials
            .iter()
            .find(|credential| credential.device_id == device_id);
        let username = body
            .username
            .as_deref()
            .and_then(non_empty_string)
            .or_else(|| existing.map(|credential| credential.username.clone()))
            .or_else(|| non_empty_string(&state.defaults.rtsp_username))
            .unwrap_or_default();
        let password = body
            .password
            .as_deref()
            .and_then(non_empty_string)
            .or_else(|| existing.map(|credential| credential.password.clone()))
            .or_else(|| non_empty_string(&state.defaults.rtsp_password))
            .unwrap_or_default();
        let rtsp_port = body
            .rtsp_port
            .filter(|port| *port > 0)
            .or_else(|| existing.and_then(|credential| credential.rtsp_port))
            .or_else(|| rtsp_port_from_url(&device.primary_stream.url))
            .or(Some(state.defaults.rtsp_port));
        let rtsp_paths = if body.rtsp_paths.is_empty() {
            existing
                .map(|credential| credential.rtsp_paths.clone())
                .filter(|paths| !paths.is_empty())
                .or_else(|| rtsp_path_from_url(&device.primary_stream.url).map(|path| vec![path]))
                .unwrap_or_else(|| state.defaults.rtsp_paths.clone())
        } else {
            body.rtsp_paths
        };

        let credential = DeviceCredentialSecret {
            device_id: device_id.clone(),
            username,
            password,
            rtsp_port,
            rtsp_paths: dedupe_rtsp_paths(rtsp_paths),
            updated_at: Some(now_unix_string()),
            last_verified_at: existing.and_then(|credential| credential.last_verified_at.clone()),
        };
        match self.admin_store.save_device_credential(credential) {
            Ok(state) => ok_json(&build_device_credential_status(&state, &device)),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_device_validation_run(
        &self,
        path: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        let device_id = match parse_device_validation_run_path(path) {
            Some(device_id) => device_id,
            None => return error_json(StatusCode(400), "invalid device validation run path"),
        };
        let principal =
            match self.authorize_camera_action(hints, &device_id, AccessAction::CameraOperate) {
                Ok(principal) => principal,
                Err(error) => return error_json(StatusCode(403), &error),
            };
        let device = match self.load_camera_device(&device_id) {
            Ok(device) => device,
            Err(error) if error.contains("device not found") => {
                return error_json(StatusCode(404), &error)
            }
            Err(error) => return error_json(StatusCode(422), &error),
        };

        match self.run_device_validation(&principal, &device) {
            Ok(payload) => ok_json(&payload),
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_rtsp_check(
        &self,
        path: &str,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        let device_id = match parse_device_rtsp_check_path(path) {
            Some(device_id) => device_id,
            None => return error_json(StatusCode(400), "invalid device rtsp-check path"),
        };
        if let Err(error) =
            self.authorize_camera_action(hints, &device_id, AccessAction::CameraOperate)
        {
            return error_json(StatusCode(403), &error);
        }
        let device = match self.load_camera_device(&device_id) {
            Ok(device) => device,
            Err(error) if error.contains("device not found") => {
                return error_json(StatusCode(404), &error)
            }
            Err(error) => return error_json(StatusCode(422), &error),
        };
        let body: RtspCheckRequest = match read_json_body(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };
        match self.check_device_rtsp(&device, body) {
            Ok(payload) => {
                let evidence = build_rtsp_check_evidence(&device, &payload, None);
                if let Err(error) = self.admin_store.record_device_evidence(evidence) {
                    return error_json(StatusCode(500), &error);
                }
                ok_json(&payload)
            }
            Err(error) => {
                let evidence =
                    build_rtsp_check_error_evidence(&device, &error, &now_unix_string(), None);
                let _ = self.admin_store.record_device_evidence(evidence);
                error_json(StatusCode(422), &redact_stream_url_credentials(&error))
            }
        }
    }

    fn handle_save_defaults(
        &self,
        request: &mut Request,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }
        let body: DefaultsRequest = match read_json_body(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };

        let defaults = AdminDefaults {
            cidr: body.cidr,
            discovery: body.discovery,
            recording: body.recording,
            capture: body.capture,
            ai: body.ai,
            notification_channel: body.notification_channel,
            rtsp_username: body.rtsp_username,
            rtsp_password: body.rtsp_password,
            rtsp_port: body.rtsp_port.unwrap_or(554),
            rtsp_paths: body.rtsp_paths,
            selected_camera_device_id: body.selected_camera_device_id,
            capture_subdirectory: body
                .capture_subdirectory
                .unwrap_or_else(default_capture_subdirectory),
            clip_length_seconds: body
                .clip_length_seconds
                .unwrap_or_else(default_clip_length_seconds),
            keyframe_count: body.keyframe_count.unwrap_or_else(default_keyframe_count),
            keyframe_interval_seconds: body
                .keyframe_interval_seconds
                .unwrap_or_else(default_keyframe_interval_seconds),
        };

        match self
            .hub()
            .save_defaults(defaults, Some(&self.public_origin))
        {
            Ok(payload) => ok_json(&payload),
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_camera_analyze(
        &self,
        path: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        let device_id = match parse_camera_analyze_path(path) {
            Some(device_id) => device_id,
            None => return error_json(StatusCode(400), "invalid camera analyze path"),
        };
        let principal =
            match self.authorize_camera_action(hints, &device_id, AccessAction::CameraOperate) {
                Ok(principal) => principal,
                Err(error) => return error_json(StatusCode(403), &error),
            };

        ok_json(&CameraTaskResponse {
            task_response: redact_camera_task_response(self.analyze_camera(&principal, &device_id)),
            media_item: None,
        })
    }

    fn handle_camera_task_snapshot(
        &self,
        path: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        let device_id = match parse_camera_task_snapshot_path(path) {
            Some(device_id) => device_id,
            None => return error_json(StatusCode(400), "invalid camera snapshot task path"),
        };
        let principal =
            match self.authorize_camera_action(hints, &device_id, AccessAction::CameraOperate) {
                Ok(principal) => principal,
                Err(error) => return error_json(StatusCode(403), &error),
            };

        let task_response =
            redact_camera_task_response(self.snapshot_camera(&principal, &device_id));
        let media_item = self.store_dvr_snapshot_media_item(&device_id).ok();
        ok_json(&CameraTaskResponse {
            task_response,
            media_item,
        })
    }

    fn handle_camera_share_link(
        &self,
        path: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        let device_id = match parse_camera_share_link_path(path) {
            Some(device_id) => device_id,
            None => return error_json(StatusCode(400), "invalid camera share-link path"),
        };
        let principal =
            match self.authorize_camera_action(hints, &device_id, AccessAction::CameraOperate) {
                Ok(principal) => principal,
                Err(error) => return error_json(StatusCode(403), &error),
            };

        let task_response = self.share_camera_link(&principal, &device_id);
        self.record_share_link_response_evidence(&device_id, &task_response);
        ok_json(&CameraTaskResponse {
            task_response: redact_camera_task_response(task_response),
            media_item: None,
        })
    }

    fn handle_dvr_recording_start(
        &self,
        path: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        let device_id = match parse_camera_recording_start_path(path) {
            Some(device_id) => device_id,
            None => return error_json(StatusCode(400), "invalid camera recording start path"),
        };
        if let Err(error) =
            self.authorize_camera_action(hints, &device_id, AccessAction::CameraOperate)
        {
            return error_json(StatusCode(403), &error);
        }
        let device = match self.load_camera_device(&device_id) {
            Ok(device) => device,
            Err(error) if error.contains("device not found") => {
                return error_json(StatusCode(404), &error)
            }
            Err(error) => return error_json(StatusCode(422), &error),
        };
        let device = self.camera_device_with_runtime_credentials(&device);
        let mut settings = match self.admin_store.dvr_recording_settings() {
            Ok(settings) => settings,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        if !settings
            .enabled_device_ids
            .iter()
            .any(|enabled| enabled == &device_id)
        {
            settings.enabled_device_ids.push(device_id.clone());
        }
        let settings = match self.admin_store.save_dvr_recording_settings(settings) {
            Ok(state) => state.dvr,
            Err(error) => return error_json(StatusCode(422), &error),
        };
        match self
            .dvr_runtime
            .start_recording(&device, &settings, Some(&self.public_origin))
        {
            Ok(status) => ok_json(&status),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_dvr_recording_stop(
        &self,
        path: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        let device_id = match parse_camera_recording_stop_path(path) {
            Some(device_id) => device_id,
            None => return error_json(StatusCode(400), "invalid camera recording stop path"),
        };
        if let Err(error) =
            self.authorize_camera_action(hints, &device_id, AccessAction::CameraOperate)
        {
            return error_json(StatusCode(403), &error);
        }
        if let Err(error) = self.load_camera_device(&device_id) {
            return if error.contains("device not found") {
                error_json(StatusCode(404), &error)
            } else {
                error_json(StatusCode(422), &error)
            };
        }
        let mut settings = match self.admin_store.dvr_recording_settings() {
            Ok(settings) => settings,
            Err(error) => return error_json(StatusCode(500), &error),
        };
        settings
            .enabled_device_ids
            .retain(|enabled| enabled != &device_id);
        if let Err(error) = self.admin_store.save_dvr_recording_settings(settings) {
            return error_json(StatusCode(422), &error);
        }
        match self
            .dvr_runtime
            .stop_recording(&device_id, Some(&self.public_origin))
        {
            Ok(status) => ok_json(&status),
            Err(error) => error_json(StatusCode(500), &error),
        }
    }

    fn handle_revoke_share_link(
        &self,
        path: &str,
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = self.authorize_admin_action(hints, AccessAction::AdminManage) {
            return error_json(StatusCode(403), &error);
        }

        let share_link_id = match parse_share_link_revoke_path(path) {
            Some(share_link_id) => share_link_id,
            None => return error_json(StatusCode(400), "invalid share-link revoke path"),
        };

        let store = self.task_service.conversation_store();
        let share_link = match store.load_share_link(&share_link_id) {
            Ok(Some(share_link)) => share_link,
            Ok(None) => return error_json(StatusCode(404), "share link not found"),
            Err(error) => return error_json(StatusCode(500), &error),
        };

        let revoked_at = remote_view::now_unix_secs().to_string();
        let revoked = match store.revoke_share_link(&share_link_id, Some(revoked_at.clone())) {
            Ok(Some(share_link)) => share_link,
            Ok(None) => return error_json(StatusCode(404), "share link not found"),
            Err(error) => return error_json(StatusCode(500), &error),
        };
        let media_session =
            match store.close_media_session(&share_link.media_session_id, Some(revoked_at)) {
                Ok(Some(media_session)) => media_session,
                Ok(None) => return error_json(StatusCode(404), "media session not found"),
                Err(error) => return error_json(StatusCode(500), &error),
            };
        self.record_share_link_revoke_evidence(
            &media_session.device_id,
            &revoked.share_link_id,
            &media_session.media_session_id,
            media_session
                .ended_at
                .as_deref()
                .unwrap_or_else(|| revoked.revoked_at.as_deref().unwrap_or("")),
        );

        ok_json(&json!({
            "share_link": revoked,
            "media_session": media_session,
        }))
    }

    fn handle_camera_snapshot(
        &self,
        path: &str,
        remote_addr: Option<SocketAddr>,
        headers: &[Header],
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = ensure_local_camera_access(remote_addr, headers) {
            return error_json(StatusCode(403), &error);
        }

        let device_id = match parse_camera_snapshot_path(path) {
            Some(device_id) => device_id,
            None => return error_json(StatusCode(400), "invalid camera snapshot path"),
        };
        if let Err(error) =
            self.authorize_camera_action(hints, &device_id, AccessAction::CameraView)
        {
            return error_json(StatusCode(403), &error);
        }

        match self.capture_camera_snapshot(&device_id) {
            Ok(bytes) => image_response(StatusCode(200), bytes, "image/jpeg"),
            Err(error) if error.contains("device not found") => error_json(StatusCode(404), &error),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_camera_hls_live_start(
        &self,
        path: &str,
        remote_addr: Option<SocketAddr>,
        headers: &[Header],
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = ensure_local_camera_access(remote_addr, headers) {
            return error_json(StatusCode(403), &error);
        }

        let device_id = match parse_camera_hls_live_start_path(path) {
            Some(device_id) => device_id,
            None => return error_json(StatusCode(400), "invalid live start path"),
        };
        if let Err(error) =
            self.authorize_camera_action(hints, &device_id, AccessAction::CameraView)
        {
            return error_json(StatusCode(403), &error);
        }

        let device = match self.load_camera_device(&device_id) {
            Ok(device) => self.camera_device_with_runtime_credentials(&device),
            Err(error) if error.contains("device not found") => {
                return error_json(StatusCode(404), &error)
            }
            Err(error) => return error_json(StatusCode(422), &error),
        };

        match self
            .hls_live_runtime
            .start_session(&device.device_id, &device.primary_stream.url)
        {
            Ok(session) => ok_json(&session.to_response(&self.public_origin)),
            Err(error) => error_json(StatusCode(422), &error),
        }
    }

    fn handle_camera_hls_live_status(
        &self,
        raw_url: &str,
        path: &str,
        remote_addr: Option<SocketAddr>,
        headers: &[Header],
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = ensure_local_camera_access(remote_addr, headers) {
            return error_json(StatusCode(403), &error);
        }

        let device_id = match parse_camera_hls_live_status_path(path) {
            Some(device_id) => device_id,
            None => return error_json(StatusCode(400), "invalid live status path"),
        };
        if let Err(error) =
            self.authorize_camera_action(hints, &device_id, AccessAction::CameraView)
        {
            return error_json(StatusCode(403), &error);
        }
        let session_id = parse_query_param(raw_url, "session_id");
        ok_json(
            &self
                .hls_live_runtime
                .status(&device_id, session_id.as_deref())
                .to_response(&self.public_origin),
        )
    }

    fn handle_camera_hls_live_stop(
        &self,
        path: &str,
        request: &mut Request,
        remote_addr: Option<SocketAddr>,
        headers: &[Header],
        hints: &AccessIdentityHints,
    ) -> Response<std::io::Cursor<Vec<u8>>> {
        if let Err(error) = ensure_local_camera_access(remote_addr, headers) {
            return error_json(StatusCode(403), &error);
        }

        let device_id = match parse_camera_hls_live_stop_path(path) {
            Some(device_id) => device_id,
            None => return error_json(StatusCode(400), "invalid live stop path"),
        };
        if let Err(error) =
            self.authorize_camera_action(hints, &device_id, AccessAction::CameraView)
        {
            return error_json(StatusCode(403), &error);
        }
        let payload = match read_json_body_or_default::<LiveStopRequest>(request) {
            Ok(payload) => payload,
            Err(error) => return error_json(StatusCode(400), &error),
        };
        ok_json(
            &self
                .hls_live_runtime
                .stop_session(&device_id, payload.session_id.as_deref())
                .to_response(&self.public_origin),
        )
    }

    fn handle_camera_hls_live_asset(
        &self,
        path: &str,
        remote_addr: Option<SocketAddr>,
        headers: &[Header],
        hints: &AccessIdentityHints,
    ) -> ResponseBox {
        if let Err(error) = ensure_local_camera_access(remote_addr, headers) {
            return error_json(StatusCode(403), &error).boxed();
        }

        let (device_id, session_id, asset_name) = match parse_camera_hls_live_asset_path(path) {
            Some(parts) => parts,
            None => return error_json(StatusCode(400), "invalid live asset path").boxed(),
        };
        if let Err(error) =
            self.authorize_camera_action(hints, &device_id, AccessAction::CameraView)
        {
            return error_json(StatusCode(403), &error).boxed();
        }

        let asset_path =
            match self
                .hls_live_runtime
                .asset_path(&device_id, &session_id, &asset_name)
            {
                Ok(path) => path,
                Err(error) if error.contains("not found") => {
                    return error_json(StatusCode(404), &error).boxed()
                }
                Err(error) => return error_json(StatusCode(422), &error).boxed(),
            };

        let file = match fs::File::open(&asset_path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return error_json(StatusCode(404), "live asset not found").boxed()
            }
            Err(error) => {
                return error_json(
                    StatusCode(422),
                    &format!("failed to read live asset: {error}"),
                )
                .boxed()
            }
        };

        let metadata = file.metadata().ok();
        let headers = vec![
            Header::from_bytes(
                b"Content-Type".as_slice(),
                live_asset_mime_type(&asset_name).as_bytes(),
            )
            .expect("header"),
            Header::from_bytes(b"X-Accel-Buffering".as_slice(), b"no".as_slice()).expect("header"),
        ];
        let mut response = Response::new(
            StatusCode(200),
            headers,
            file,
            metadata.map(|value| value.len() as usize),
            None,
        )
        .boxed();
        add_common_headers(&mut response);
        response
    }

    fn handle_camera_live_mjpeg(
        &self,
        path: &str,
        remote_addr: Option<SocketAddr>,
        headers: &[Header],
        hints: &AccessIdentityHints,
    ) -> ResponseBox {
        if let Err(error) = ensure_local_camera_access(remote_addr, headers) {
            return error_json(StatusCode(403), &error).boxed();
        }

        let device_id = match parse_camera_live_stream_path(path) {
            Some(device_id) => device_id,
            None => return error_json(StatusCode(400), "invalid live stream path").boxed(),
        };
        if let Err(error) =
            self.authorize_camera_action(hints, &device_id, AccessAction::CameraView)
        {
            return error_json(StatusCode(403), &error).boxed();
        }

        let device = match self.load_camera_device(&device_id) {
            Ok(device) => device,
            Err(error) if error.contains("device not found") => {
                return error_json(StatusCode(404), &error).boxed()
            }
            Err(error) => return error_json(StatusCode(422), &error).boxed(),
        };

        let device = self.camera_device_with_runtime_credentials(&device);
        let stream = match FfmpegMjpegStream::spawn(&device.primary_stream.url) {
            Ok(stream) => stream,
            Err(error) => {
                return error_json(StatusCode(422), &format!("打开实时画面失败: {error}")).boxed()
            }
        };

        let headers = vec![
            Header::from_bytes(
                b"Content-Type".as_slice(),
                b"multipart/x-mixed-replace;boundary=ffmpeg".as_slice(),
            )
            .expect("header"),
            Header::from_bytes(b"X-Accel-Buffering".as_slice(), b"no".as_slice()).expect("header"),
        ];
        let mut response = Response::new(StatusCode(200), headers, stream, None, None).boxed();
        add_common_headers(&mut response);
        response
    }

    fn handle_shared_camera_live_mjpeg(&self, path: &str) -> ResponseBox {
        let token = match parse_shared_camera_live_stream_path(path) {
            Some(token) => token,
            None => return error_json(StatusCode(400), "invalid shared live stream path").boxed(),
        };
        let claims = match self.verify_shared_camera_token(&token) {
            Ok(claims) => claims,
            Err(error) => return error_json(StatusCode(403), &error).boxed(),
        };
        let device = match self.load_camera_device(&claims.device_id) {
            Ok(device) => device,
            Err(error) if error.contains("device not found") => {
                return error_json(StatusCode(404), &error).boxed()
            }
            Err(error) => return error_json(StatusCode(422), &error).boxed(),
        };

        let device = self.camera_device_with_runtime_credentials(&device);
        let stream = match FfmpegMjpegStream::spawn(&device.primary_stream.url) {
            Ok(stream) => stream,
            Err(error) => {
                return error_json(StatusCode(422), &format!("打开共享实时画面失败: {error}"))
                    .boxed()
            }
        };

        let headers = vec![
            Header::from_bytes(
                b"Content-Type".as_slice(),
                b"multipart/x-mixed-replace;boundary=ffmpeg".as_slice(),
            )
            .expect("header"),
            Header::from_bytes(b"X-Accel-Buffering".as_slice(), b"no".as_slice()).expect("header"),
        ];
        let mut response = Response::new(StatusCode(200), headers, stream, None, None).boxed();
        add_common_headers(&mut response);
        response
    }

    fn current_state(&self) -> Result<StateResponse, String> {
        self.hub().state_snapshot(Some(&self.public_origin))
    }

    fn current_gateway_manage_url(&self) -> String {
        self.admin_store
            .load_state()
            .map(|state| {
                harborbeacon_local_agent::runtime::admin_console::gateway_manage_url(
                    &state.bridge_provider.gateway_base_url,
                )
            })
            .unwrap_or_default()
    }

    fn refresh_gateway_projection_best_effort(&self) {
        if self
            .hub()
            .refresh_bridge_provider_status(Some(&self.public_origin))
            .is_ok()
        {
            return;
        }
        if let Ok(payload) = fetch_remote_gateway_status() {
            if let Some(provider) = live_bridge_provider_from_setup_status(&payload) {
                let _ = self.admin_store.save_bridge_provider_status(provider);
            }
        }
    }

    fn scan(
        &self,
        principal: &AccessPrincipal,
        request: ScanRequest,
    ) -> Result<ScanResponse, String> {
        let response = self
            .task_service
            .handle_task(self.build_camera_task_request(
                principal,
                "scan",
                "扫描摄像头",
                scan_request_task_args(&request),
            ));
        if response.status != TaskStatus::Completed {
            return Err(task_error_message(&response));
        }

        let state = self.current_state()?;
        let results = parse_scan_results(&response.result.data)?;
        let scanned_hosts = response
            .result
            .data
            .pointer("/summary/scanned_hosts")
            .and_then(Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or_default();

        Ok(HubScanSummary {
            binding: state.binding,
            defaults: state.defaults,
            devices: state.devices,
            results,
            scanned_hosts,
        })
    }

    fn manual_add(
        &self,
        principal: &AccessPrincipal,
        request: ManualAddRequest,
    ) -> Result<ManualAddResponse, String> {
        let ManualAddRequest {
            name,
            room,
            ip,
            path,
            snapshot_url,
            username,
            password,
            port,
        } = request;
        let path_candidates = path
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| {
                if value.starts_with('/') {
                    value.to_string()
                } else {
                    format!("/{value}")
                }
            })
            .map(|value| vec![value])
            .unwrap_or_default();

        if principal_skips_manual_camera_connect_approval(principal) {
            return self.hub().manual_add(
                CameraConnectRequest {
                    name,
                    room,
                    ip,
                    path_candidates,
                    username,
                    password,
                    port,
                    snapshot_url,
                    discovery_source: "admin_console_manual_add".to_string(),
                    vendor: None,
                    model: None,
                },
                Some(&self.public_origin),
            );
        }

        let response = self
            .task_service
            .handle_task(self.build_camera_task_request(
                principal,
                "connect",
                "手动接入摄像头",
                json!({
                    "name": name,
                    "room": room,
                    "ip": ip,
                    "path_candidates": path_candidates,
                    "snapshot_url": snapshot_url,
                    "username": username,
                    "password": password,
                    "port": port,
                    "discovery_source": "admin_console_manual_add",
                }),
            ));
        if response.status != TaskStatus::Completed {
            return Err(task_error_message(&response));
        }

        let state = self.current_state()?;
        let device = parse_connected_device(&response.result.data)?;
        Ok(HubManualAddSummary {
            binding: state.binding,
            defaults: state.defaults,
            device,
            devices: state.devices,
            note: response.result.message,
        })
    }

    fn patch_device_metadata(
        &self,
        device_id: &str,
        request: DeviceMetadataPatchRequest,
    ) -> Result<StateResponse, String> {
        let registry_store = self.admin_store.registry_store();
        let mut devices = registry_store.load_devices()?;
        let Some(device) = devices
            .iter_mut()
            .find(|device| device.device_id == device_id)
        else {
            return Err(format!("device not found: {device_id}"));
        };

        if let Some(name) = request.name.as_deref().and_then(non_empty_string) {
            device.name = name;
        }
        if let Some(room) = request.room {
            device.room = non_empty_string(&room);
        }
        if let Some(vendor) = request.vendor {
            device.vendor = non_empty_string(&vendor);
        }
        if let Some(model) = request.model {
            device.model = non_empty_string(&model);
        }
        if let Some(ip_address) = request.ip_address {
            device.ip_address = non_empty_string(&ip_address);
        }
        if let Some(snapshot_url) = request.snapshot_url {
            device.snapshot_url = non_empty_string(&snapshot_url);
            if device.snapshot_url.is_some() {
                device.capabilities.snapshot = true;
            }
        }
        if let Some(requires_auth) = request.requires_auth {
            device.primary_stream.requires_auth = requires_auth;
        }
        if let Some(primary_stream_url) = request
            .primary_stream_url
            .as_deref()
            .and_then(non_empty_string)
        {
            device.primary_stream.url = primary_stream_url;
            device.capabilities.stream = true;
        } else if request.rtsp_path.is_some() || request.rtsp_port.is_some() {
            device.primary_stream.url =
                build_rtsp_url_from_patch(device, request.rtsp_path.as_deref(), request.rtsp_port)?;
            device.capabilities.stream = true;
        }

        registry_store.save_devices(&devices)?;
        self.current_state()
    }

    fn delete_device(&self, device_id: &str) -> Result<StateResponse, String> {
        let registry_store = self.admin_store.registry_store();
        let mut devices = registry_store.load_devices()?;
        let original_len = devices.len();
        devices.retain(|device| device.device_id != device_id);
        if devices.len() == original_len {
            return Err(format!("device not found: {device_id}"));
        }

        let _ = self
            .dvr_runtime
            .stop_recording(device_id, Some(&self.public_origin));
        registry_store.save_devices(&devices)?;
        self.admin_store.forget_device(device_id)?;
        self.current_state()
    }

    fn analyze_camera(&self, principal: &AccessPrincipal, device_id: &str) -> TaskResponse {
        self.task_service
            .handle_task(self.build_camera_task_request(
                principal,
                "analyze",
                "分析摄像头画面",
                json!({
                    "device_id": device_id,
                }),
            ))
    }

    fn snapshot_camera(&self, principal: &AccessPrincipal, device_id: &str) -> TaskResponse {
        self.task_service
            .handle_task(self.build_camera_task_request(
                principal,
                "snapshot",
                "抓拍摄像头画面",
                json!({
                    "device_id": device_id,
                }),
            ))
    }

    fn share_camera_link(&self, principal: &AccessPrincipal, device_id: &str) -> TaskResponse {
        self.task_service
            .handle_task(self.build_camera_task_request(
                principal,
                "share_link",
                "生成共享观看链接",
                json!({
                    "device_id": device_id,
                }),
            ))
    }

    fn capture_camera_snapshot(&self, device_id: &str) -> Result<Vec<u8>, String> {
        let device = self.load_camera_device(device_id)?;
        let device = self.camera_device_with_runtime_credentials(&device);
        let adapter = CommandRtspAdapter::default();
        let result = adapter.capture_snapshot(
            &SnapshotCaptureRequest::new(
                device.device_id,
                device.primary_stream.url,
                SnapshotFormat::Jpeg,
                StorageTarget::LocalDisk,
            )
            .with_snapshot_url(device.snapshot_url),
        )?;

        base64::engine::general_purpose::STANDARD
            .decode(result.bytes_base64.as_bytes())
            .map_err(|error| format!("snapshot bytes decode failed: {error}"))
    }

    fn camera_device_with_runtime_credentials(&self, device: &CameraDevice) -> CameraDevice {
        let Ok(state) = self.admin_store.load_state() else {
            return device.clone();
        };
        let Some(stream_url) = camera_stream_url_with_credentials(device, &state) else {
            return device.clone();
        };
        let mut resolved = device.clone();
        resolved.primary_stream.url = stream_url;
        resolved.primary_stream.requires_auth = true;
        resolved
    }

    fn store_dvr_snapshot_media_item(&self, device_id: &str) -> Result<DvrTimelineSegment, String> {
        let settings = self.admin_store.dvr_recording_settings()?;
        let device = self.load_camera_device(device_id)?;
        let bytes = self.capture_camera_snapshot(device_id)?;
        store_snapshot_bytes(&settings, &device, &bytes, Some(&self.public_origin))
    }

    fn load_camera_device(
        &self,
        device_id: &str,
    ) -> Result<harborbeacon_local_agent::runtime::registry::CameraDevice, String> {
        self.hub()
            .load_registered_cameras()?
            .into_iter()
            .find(|device| device.device_id == device_id)
            .ok_or_else(|| format!("device not found: {device_id}"))
    }

    fn check_device_rtsp(
        &self,
        device: &CameraDevice,
        request: RtspCheckRequest,
    ) -> Result<RtspCheckResponse, String> {
        let state = self.admin_store.load_state()?;
        let credential = state
            .device_credentials
            .iter()
            .find(|credential| credential.device_id == device.device_id);
        let ip_address = device
            .ip_address
            .clone()
            .or_else(|| rtsp_host_from_url(&device.primary_stream.url))
            .ok_or_else(|| format!("device {} does not expose an RTSP host", device.device_id))?;
        let rtsp_port = request
            .rtsp_port
            .filter(|port| *port > 0)
            .or_else(|| credential.and_then(|credential| credential.rtsp_port))
            .or_else(|| rtsp_port_from_url(&device.primary_stream.url))
            .unwrap_or(state.defaults.rtsp_port);
        let username = request
            .username
            .as_deref()
            .and_then(non_empty_string)
            .or_else(|| credential.and_then(|credential| non_empty_string(&credential.username)))
            .or_else(|| non_empty_string(&state.defaults.rtsp_username));
        let password = request
            .password
            .as_deref()
            .and_then(non_empty_string)
            .or_else(|| credential.and_then(|credential| non_empty_string(&credential.password)))
            .or_else(|| non_empty_string(&state.defaults.rtsp_password));
        let path_candidates = if request.rtsp_paths.is_empty() {
            credential
                .map(|credential| credential.rtsp_paths.clone())
                .filter(|paths| !paths.is_empty())
                .or_else(|| rtsp_path_from_url(&device.primary_stream.url).map(|path| vec![path]))
                .unwrap_or_else(|| state.defaults.rtsp_paths.clone())
        } else {
            request.rtsp_paths
        };

        let adapter = CommandRtspAdapter::default();
        let checked_at = now_unix_string();
        let result = adapter.probe(&RtspProbeRequest {
            candidate_id: format!("rtsp-check-{}", device.device_id),
            ip_address,
            port: rtsp_port,
            username,
            password,
            path_candidates: dedupe_rtsp_paths(path_candidates),
        })?;
        if result.reachable {
            let _ = self
                .admin_store
                .mark_device_credential_verified(&device.device_id, checked_at.clone());
        }

        Ok(RtspCheckResponse {
            device_id: device.device_id.clone(),
            reachable: result.reachable,
            stream_url: result
                .stream_url
                .as_deref()
                .map(redact_stream_url_credentials),
            transport: format!("{:?}", result.transport).to_lowercase(),
            requires_auth: result.requires_auth,
            capabilities: result.capabilities,
            error_message: result.error_message,
            checked_at,
        })
    }

    fn run_device_validation(
        &self,
        principal: &AccessPrincipal,
        device: &CameraDevice,
    ) -> Result<DeviceValidationRunResponse, String> {
        let validation_id = format!(
            "device-validation-{}-{}",
            sanitize_id_fragment(&device.device_id),
            Uuid::new_v4().simple()
        );
        let rtsp_check = match self.check_device_rtsp(device, RtspCheckRequest::default()) {
            Ok(payload) => build_rtsp_check_evidence(device, &payload, Some(&validation_id)),
            Err(error) => build_rtsp_check_error_evidence(
                device,
                &error,
                &now_unix_string(),
                Some(&validation_id),
            ),
        };
        self.admin_store
            .record_device_evidence(rtsp_check.clone())?;

        let snapshot_check = if device_has_snapshot_path(device) {
            let response = self.snapshot_camera(principal, &device.device_id);
            build_snapshot_check_evidence(device, &response, Some(&validation_id))
        } else {
            build_snapshot_skipped_evidence(
                device,
                "device has no stream or snapshot endpoint to validate",
                Some(&validation_id),
            )
        };
        self.admin_store
            .record_device_evidence(snapshot_check.clone())?;

        let evidence = self.build_device_evidence_response(device)?;
        let status = validation_status(&rtsp_check, &snapshot_check);

        Ok(DeviceValidationRunResponse {
            validation_id,
            device_id: device.device_id.clone(),
            status,
            rtsp_check: evidence
                .recent_rtsp_check
                .clone()
                .unwrap_or_else(|| rtsp_check.clone()),
            snapshot_check: evidence
                .recent_snapshot_check
                .clone()
                .unwrap_or_else(|| snapshot_check.clone()),
            evidence,
        })
    }

    fn build_device_evidence_response(
        &self,
        device: &CameraDevice,
    ) -> Result<DeviceEvidenceResponse, String> {
        let state = self.admin_store.load_state()?;
        let credential_status = build_device_credential_status(&state, device);
        let share_links = self.list_share_links(Some(&device.device_id))?;
        let mut evidence = self.admin_store.list_device_evidence(&device.device_id)?;
        if let Some(snapshot_evidence) = self.latest_snapshot_asset_evidence(device)? {
            evidence.push(snapshot_evidence);
        }
        evidence.extend(
            share_links
                .iter()
                .map(build_share_link_evidence)
                .collect::<Vec<_>>(),
        );
        evidence = redact_device_evidence_records(evidence);
        evidence.sort_by(|left, right| {
            right
                .observed_at
                .cmp(&left.observed_at)
                .then(right.evidence_id.cmp(&left.evidence_id))
        });
        let recent_rtsp_check = evidence
            .iter()
            .find(|record| record.evidence_kind == "rtsp_check")
            .cloned();
        let recent_snapshot_check = evidence
            .iter()
            .find(|record| record.evidence_kind == "snapshot_check")
            .cloned();
        evidence.truncate(50);

        Ok(DeviceEvidenceResponse {
            device_id: device.device_id.clone(),
            generated_at: now_unix_string(),
            credential_status,
            recent_rtsp_check,
            recent_snapshot_check,
            share_links,
            evidence,
        })
    }

    fn latest_snapshot_asset_evidence(
        &self,
        device: &CameraDevice,
    ) -> Result<Option<DeviceEvidenceRecord>, String> {
        let media_assets = self.task_service.conversation_store().list_media_assets()?;
        Ok(media_assets
            .into_iter()
            .filter(|asset| {
                asset.device_id.as_deref() == Some(device.device_id.as_str())
                    && matches!(asset.asset_kind, MediaAssetKind::Snapshot)
            })
            .max_by(|left, right| {
                left.captured_at
                    .cmp(&right.captured_at)
                    .then(left.asset_id.cmp(&right.asset_id))
            })
            .map(|asset| build_snapshot_asset_evidence(device, &asset)))
    }

    fn record_share_link_response_evidence(&self, device_id: &str, response: &TaskResponse) {
        let observed_at = now_unix_string();
        let status = if matches!(response.status, TaskStatus::Completed) {
            "ready"
        } else {
            "blocked"
        };
        let share_link_id = response
            .result
            .data
            .pointer("/share_link/share_link_id")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let evidence = DeviceEvidenceRecord {
            evidence_id: format!(
                "share-link-create-{}-{}",
                sanitize_id_fragment(device_id),
                observed_at
            ),
            device_id: device_id.to_string(),
            evidence_kind: "share_link_create".to_string(),
            status: status.to_string(),
            observed_at,
            summary: format!("Share link create status={status} share_link_id={share_link_id}"),
            details: json!({
                "task_id": response.task_id,
                "status": format!("{:?}", response.status).to_lowercase(),
                "share_link_id": share_link_id,
                "artifact_count": response.result.artifacts.len(),
            }),
        };
        let _ = self.admin_store.record_device_evidence(evidence);
    }

    fn record_share_link_revoke_evidence(
        &self,
        device_id: &str,
        share_link_id: &str,
        media_session_id: &str,
        observed_at: &str,
    ) {
        let evidence = DeviceEvidenceRecord {
            evidence_id: format!(
                "share-link-revoke-{}-{}",
                sanitize_id_fragment(share_link_id),
                observed_at
            ),
            device_id: device_id.to_string(),
            evidence_kind: "share_link_revoke".to_string(),
            status: "ready".to_string(),
            observed_at: observed_at.to_string(),
            summary: format!("Share link revoked: {share_link_id}"),
            details: json!({
                "share_link_id": share_link_id,
                "media_session_id": media_session_id,
                "revoked": true,
            }),
        };
        let _ = self.admin_store.record_device_evidence(evidence);
    }

    fn verify_shared_camera_token(
        &self,
        token: &str,
    ) -> Result<remote_view::CameraShareClaims, String> {
        let remote_view_config = self.admin_store.load_remote_view_config()?;
        let claims =
            remote_view::verify_camera_share_token(&remote_view_config.share_secret, token)?;
        let token_hash = remote_view::camera_share_token_hash(token);
        let share_link = self
            .task_service
            .conversation_store()
            .find_share_link_by_token_hash(&token_hash)?
            .ok_or_else(|| "share token is not registered".to_string())?;
        if share_link
            .revoked_at
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some()
        {
            return Err("share token revoked".to_string());
        }
        if let Some(expires_at) = share_link.expires_at.as_deref() {
            let expires_at = expires_at
                .trim()
                .parse::<u64>()
                .map_err(|_| "share token expiry is invalid".to_string())?;
            if remote_view::now_unix_secs() > expires_at {
                return Err("share token expired".to_string());
            }
        }

        let media_session = self
            .task_service
            .conversation_store()
            .load_media_session(&share_link.media_session_id)?
            .ok_or_else(|| {
                format!(
                    "share media session not found: {}",
                    share_link.media_session_id
                )
            })?;
        if media_session.device_id != claims.device_id {
            return Err("share token device mismatch".to_string());
        }
        if media_session.share_link_id.as_deref() != Some(share_link.share_link_id.as_str()) {
            return Err("share token session mismatch".to_string());
        }
        if !matches!(
            media_session.status,
            MediaSessionStatus::Opening | MediaSessionStatus::Active
        ) {
            return Err("share session is no longer active".to_string());
        }

        Ok(claims)
    }

    fn list_share_links(
        &self,
        device_filter: Option<&str>,
    ) -> Result<Vec<ShareLinkSummary>, String> {
        let share_links = self.task_service.conversation_store().list_share_links()?;
        let media_sessions = self
            .task_service
            .conversation_store()
            .list_media_sessions()?;
        let media_session_map: HashMap<String, MediaSession> = media_sessions
            .into_iter()
            .map(|media_session| (media_session.media_session_id.clone(), media_session))
            .collect();
        let device_name_map: HashMap<String, String> = self
            .hub()
            .load_registered_cameras()?
            .into_iter()
            .map(|device| (device.device_id.clone(), device.name))
            .collect();
        let now = remote_view::now_unix_secs();

        let mut summaries = share_links
            .into_iter()
            .filter_map(|share_link| {
                let media_session = media_session_map.get(&share_link.media_session_id)?;
                if let Some(device_filter) = device_filter {
                    if media_session.device_id != device_filter {
                        return None;
                    }
                }
                Some(build_share_link_summary(
                    share_link,
                    media_session,
                    device_name_map.get(&media_session.device_id),
                    now,
                ))
            })
            .collect::<Vec<_>>();
        summaries.sort_by(|left, right| {
            right
                .started_at
                .cmp(&left.started_at)
                .then(right.share_link_id.cmp(&left.share_link_id))
        });
        Ok(summaries)
    }

    fn build_camera_task_request(
        &self,
        principal: &AccessPrincipal,
        action: &str,
        raw_text: &str,
        args: Value,
    ) -> TaskRequest {
        TaskRequest {
            task_id: String::new(),
            trace_id: String::new(),
            step_id: String::new(),
            source: TaskSource {
                channel: "admin_api".to_string(),
                surface: "agent_hub_admin_api".to_string(),
                conversation_id: format!("admin-console:{}", principal.user_id),
                user_id: principal.user_id.clone(),
                session_id: format!("admin-console:{}", principal.user_id),
                route_key: String::new(),
            },
            intent: TaskIntent {
                domain: "camera".to_string(),
                action: action.to_string(),
                raw_text: raw_text.to_string(),
            },
            entity_refs: Value::Null,
            args,
            autonomy: Default::default(),
            message: None,
        }
    }
}

fn scan_request_task_args(request: &ScanRequest) -> Value {
    json!({
        "cidr": request.cidr,
        "protocol": request.protocol,
        "rtsp_port": request.rtsp_port,
        "rtsp_username": request.rtsp_username,
        "rtsp_password": request.rtsp_password,
    })
}

fn principal_skips_manual_camera_connect_approval(principal: &AccessPrincipal) -> bool {
    matches!(principal.role_kind, RoleKind::Owner | RoleKind::Admin)
}

fn main() {
    let cli = Cli::parse();
    let device_registry_path = resolve_state_path(&cli.device_registry);
    let admin_state_path = resolve_state_path(&cli.admin_state);
    std::env::set_var(ADMIN_STATE_PATH_ENV, &admin_state_path);
    let conversation_path = resolve_state_path(&cli.conversations);
    let registry_store = DeviceRegistryStore::new(device_registry_path);
    let admin_store = AdminConsoleStore::new(admin_state_path, registry_store);
    let conversation_store = TaskConversationStore::new(conversation_path);
    let task_service = TaskApiService::new(admin_store.clone(), conversation_store);
    let api = AdminApi::new(
        admin_store,
        task_service,
        cli.harbor_assistant_dist,
        cli.public_origin,
    );

    let server = Server::http(&cli.bind).unwrap_or_else(|error| {
        eprintln!("failed to start admin api on {}: {}", cli.bind, error);
        std::process::exit(1);
    });

    println!("HarborBeacon admin API listening on http://{}", cli.bind);
    let (request_sender, request_receiver) =
        sync_channel::<Request>(ADMIN_HTTP_REQUEST_QUEUE_CAPACITY);
    let request_receiver = Arc::new(Mutex::new(request_receiver));
    for worker_index in 0..ADMIN_HTTP_WORKER_COUNT {
        let worker_api = api.clone();
        let worker_receiver = request_receiver.clone();
        let _ = thread::Builder::new()
            .name(format!("admin-api-worker-{worker_index}"))
            .spawn(move || loop {
                let request = {
                    let Ok(receiver) = worker_receiver.lock() else {
                        return;
                    };
                    receiver.recv()
                };
                match request {
                    Ok(request) => {
                        let _guard = worker_api.begin_http_request();
                        worker_api.handle(request);
                    }
                    Err(_) => return,
                }
            });
    }

    for request in server.incoming_requests() {
        if is_healthz_request(&request) {
            let _ = request.respond(ok_json(&json!({"status":"ok"})).boxed());
            continue;
        }
        match request_sender.try_send(request) {
            Ok(()) => api.record_http_request_accepted(),
            Err(TrySendError::Full(request)) => {
                api.record_http_request_rejected();
                let _ = request.respond(service_overloaded_response().boxed());
            }
            Err(TrySendError::Disconnected(request)) => {
                api.record_http_request_rejected();
                let _ = request.respond(service_overloaded_response().boxed());
            }
        }
    }
}

fn is_healthz_request(request: &Request) -> bool {
    request.method() == &Method::Get
        && normalize_unified_admin_url(request.url())
            .split('?')
            .next()
            .is_some_and(|path| path == "/healthz")
}

fn service_overloaded_response() -> Response<std::io::Cursor<Vec<u8>>> {
    json_response(
        StatusCode(503),
        &json!({
            "error": "service overloaded",
            "status": "blocked",
            "reason": "HarborBeacon request queue is full; retry after the current load drops.",
            "metadata_only": true,
            "secret_scan": "clean",
        }),
    )
}

fn resolve_state_path(preferred: &Path) -> PathBuf {
    preferred.to_path_buf()
}

fn normalize_unified_admin_url(raw_url: &str) -> String {
    let (path, query) = raw_url
        .split_once('?')
        .map(|(path, query)| (path, Some(query)))
        .unwrap_or((raw_url, None));
    let normalized_path = normalize_unified_admin_path(path);
    match query {
        Some(query) => format!("{normalized_path}?{query}"),
        None => normalized_path,
    }
}

fn normalize_unified_admin_path(path: &str) -> String {
    if path == "/api/beacon" {
        return "/api/state".to_string();
    }
    if let Some(tail) = path.strip_prefix("/api/beacon/") {
        return format!("/api/{tail}");
    }
    if path == "/api/harbor-beacon" {
        return "/api/state".to_string();
    }
    if let Some(tail) = path.strip_prefix("/api/harbor-beacon/") {
        return format!("/api/{tail}");
    }
    if path == "/api/harbor-assistant" {
        return "/api/state".to_string();
    }
    if let Some(tail) = path.strip_prefix("/api/harbor-assistant/") {
        return format!("/api/{tail}");
    }
    let Some(tail) = path.strip_prefix("/api/admin") else {
        return path.to_string();
    };
    let tail = tail.trim_start_matches('/');
    if tail.is_empty() {
        return "/api/state".to_string();
    }
    if tail == "notification-targets" || tail.starts_with("notification-targets/") {
        return path.to_string();
    }
    format!("/api/{tail}")
}

fn parse_query_param(url: &str, key: &str) -> Option<String> {
    let query = url.split('?').nth(1)?;
    for pair in query.split('&') {
        let (name, value) = pair.split_once('=')?;
        if name == key {
            return Some(value.to_string());
        }
    }
    None
}

fn parse_query_bool(url: &str, key: &str) -> Option<bool> {
    parse_query_param(url, key).and_then(|value| match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    })
}

fn parse_automation_review_action_path(path: &str) -> Option<String> {
    let trimmed = path.strip_prefix("/api/automation/reviews/")?;
    let (review_id, action) = trimmed.rsplit_once('/')?;
    if !matches!(action, "enable" | "pause" | "discard") {
        return None;
    }
    let review_id = review_id.trim();
    if review_id.is_empty() || review_id.contains('/') {
        return None;
    }
    Some(review_id.to_string())
}

fn parse_automation_review_evaluate_latest_path(path: &str) -> Option<String> {
    let trimmed = path.strip_prefix("/api/automation/reviews/")?;
    let (review_id, action) = trimmed.rsplit_once('/')?;
    if action != "evaluate-latest" {
        return None;
    }
    let review_id = review_id.trim();
    if review_id.is_empty() || review_id.contains('/') {
        return None;
    }
    Some(review_id.to_string())
}

fn build_automation_reviews_response(
    reviews: Vec<AutomationRuleReview>,
) -> AutomationReviewsResponse {
    let pending_count = reviews
        .iter()
        .filter(|review| matches!(review.status.as_str(), "draft" | "pending"))
        .count();
    AutomationReviewsResponse {
        generated_at: now_unix_string(),
        pending_count,
        reviews,
    }
}

fn request_identity_hints(url: &str, headers: &[Header]) -> AccessIdentityHints {
    AccessIdentityHints {
        user_id: header_value(headers, "X-Harbor-User-Id")
            .or_else(|| parse_query_param(url, "user_id"))
            .and_then(percent_decode_optional_query_value),
        open_id: header_value(headers, "X-Harbor-Open-Id")
            .or_else(|| parse_query_param(url, "open_id"))
            .and_then(percent_decode_optional_query_value),
        harboros_user_id: header_value(headers, "X-HarborOS-User")
            .or_else(|| header_value(headers, "X-Harbor-OS-User"))
            .or_else(|| parse_query_param(url, "harboros_user"))
            .and_then(percent_decode_optional_query_value)
            .or_else(|| std::env::var("HARBOR_HARBOROS_USER").ok()),
    }
}

fn header_value(headers: &[Header], name: &str) -> Option<String> {
    headers
        .iter()
        .find(|header| header.field.as_str().to_string().eq_ignore_ascii_case(name))
        .map(|header| header.value.as_str().trim().to_string())
        .filter(|value| !value.is_empty())
}

fn percent_decode_optional_query_value(value: String) -> Option<String> {
    percent_decode_path_segment(&value).ok().or(Some(value))
}

fn identity_query_suffix(url: &str) -> String {
    let mut pairs = Vec::new();
    if let Some(open_id) = parse_query_param(url, "open_id").filter(|value| !value.is_empty()) {
        pairs.push(format!("open_id={open_id}"));
    }
    if let Some(user_id) = parse_query_param(url, "user_id").filter(|value| !value.is_empty()) {
        pairs.push(format!("user_id={user_id}"));
    }
    if pairs.is_empty() {
        String::new()
    } else {
        format!("?{}", pairs.join("&"))
    }
}

fn parse_approval_decision_path(path: &str, action: &str) -> Option<String> {
    let prefix = "/api/tasks/approvals/";
    let suffix = format!("/{action}");
    let approval_id = path.strip_prefix(prefix)?.strip_suffix(&suffix)?.trim();
    if approval_id.is_empty() {
        return None;
    }
    percent_decode_path_segment(approval_id).ok()
}

fn parse_member_role_update_path(path: &str) -> Option<String> {
    let trimmed = path.strip_prefix("/api/access/members/")?;
    let user_id = trimmed.strip_suffix("/role")?.trim();
    if user_id.is_empty() {
        None
    } else {
        percent_decode_path_segment(user_id).ok()
    }
}

fn parse_member_default_delivery_surface_update_path(path: &str) -> Option<String> {
    let trimmed = path.strip_prefix("/api/access/members/")?;
    let user_id = trimmed.strip_suffix("/default-delivery-surface")?.trim();
    if user_id.is_empty() {
        None
    } else {
        percent_decode_path_segment(user_id).ok()
    }
}

fn parse_notification_target_delete_path(path: &str) -> Option<String> {
    let trimmed = path
        .strip_prefix("/api/admin/notification-targets/")?
        .trim();
    if trimmed.is_empty() {
        None
    } else {
        percent_decode_path_segment(trimmed).ok()
    }
}

fn parse_local_vision_event_notify_path(path: &str) -> Option<String> {
    let trimmed = path.strip_prefix("/api/vision/events/")?;
    let event_id = trimmed.strip_suffix("/notify")?.trim();
    if event_id.is_empty() || event_id.contains('/') {
        return None;
    }
    percent_decode_path_segment(event_id).ok()
}

fn parse_local_vision_event_vlm_enrich_path(path: &str) -> Option<String> {
    let trimmed = path.strip_prefix("/api/vision/events/")?;
    let event_id = trimmed.strip_suffix("/vlm-enrich")?.trim();
    if event_id.is_empty() || event_id.contains('/') || event_id == "latest" {
        return None;
    }
    percent_decode_path_segment(event_id).ok()
}

fn parse_family_memory_event_path(path: &str) -> Option<String> {
    let event_id = path.strip_prefix("/api/family/memory/events/")?.trim();
    if event_id.is_empty() || event_id.contains('/') {
        return None;
    }
    percent_decode_path_segment(event_id).ok()
}

fn parse_family_memory_feedback_path(path: &str) -> Option<String> {
    let trimmed = path.strip_prefix("/api/family/memory/events/")?;
    let event_id = trimmed.strip_suffix("/feedback")?.trim();
    if event_id.is_empty() || event_id.contains('/') {
        return None;
    }
    percent_decode_path_segment(event_id).ok()
}

fn parse_model_endpoint_path(path: &str) -> Option<String> {
    let trimmed = path.strip_prefix("/api/models/endpoints/")?;
    if trimmed.trim().is_empty() || trimmed.ends_with("/test") {
        return None;
    }
    percent_decode_path_segment(trimmed.trim()).ok()
}

fn parse_model_endpoint_test_path(path: &str) -> Option<String> {
    let endpoint_id = path
        .strip_prefix("/api/models/endpoints/")?
        .strip_suffix("/test")?
        .trim();
    if endpoint_id.is_empty() {
        None
    } else {
        percent_decode_path_segment(endpoint_id).ok()
    }
}

fn parse_model_download_job_path(path: &str) -> Option<String> {
    let job_id = path.strip_prefix("/api/models/local-downloads/")?.trim();
    if job_id.is_empty() || job_id.contains('/') {
        None
    } else {
        percent_decode_path_segment(job_id).ok()
    }
}

fn parse_model_download_cancel_path(path: &str) -> Option<String> {
    let job_id = path
        .strip_prefix("/api/models/local-downloads/")?
        .strip_suffix("/cancel")?
        .trim();
    if job_id.is_empty() || job_id.contains('/') {
        None
    } else {
        percent_decode_path_segment(job_id).ok()
    }
}

fn parse_model_runtime_install_path(path: &str) -> Option<String> {
    let runtime_id = path
        .strip_prefix("/api/models/runtimes/")?
        .strip_suffix("/install")?
        .trim();
    if runtime_id.is_empty() || runtime_id.contains('/') {
        None
    } else {
        percent_decode_path_segment(runtime_id).ok()
    }
}

fn parse_model_capability_selection_path(path: &str) -> Option<String> {
    let capability_id = path
        .strip_prefix("/api/models/capabilities/")?
        .strip_suffix("/selection")?
        .trim_matches('/');
    if capability_id.is_empty() || capability_id.contains('/') {
        None
    } else {
        percent_decode_path_segment(capability_id).ok()
    }
}

fn parse_knowledge_index_job_cancel_path(path: &str) -> Option<String> {
    let job_id = path
        .strip_prefix("/api/knowledge/index/jobs/")?
        .strip_suffix("/cancel")?
        .trim();
    if job_id.is_empty() || job_id.contains('/') {
        None
    } else {
        percent_decode_path_segment(job_id).ok()
    }
}

fn parse_role_kind(value: &str) -> Result<RoleKind, String> {
    match value.trim().to_lowercase().replace('-', "_").as_str() {
        "admin" => Ok(RoleKind::Admin),
        "operator" => Ok(RoleKind::Operator),
        "member" => Ok(RoleKind::Member),
        "viewer" => Ok(RoleKind::Viewer),
        "guest" => Ok(RoleKind::Guest),
        "owner" => Err("当前入口不支持直接设置 owner 角色".to_string()),
        _ => Err(format!("unknown role_kind: {}", value.trim())),
    }
}

fn build_access_member_summaries(state: &AdminConsoleState) -> Vec<AccessMemberSummary> {
    let workspace = state
        .platform
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == "home-1")
        .or_else(|| state.platform.workspaces.first());
    let owner_user_id = workspace
        .map(|workspace| workspace.owner_user_id.as_str())
        .unwrap_or("local-owner");

    let mut members: Vec<AccessMemberSummary> = state
        .platform
        .memberships
        .iter()
        .filter(|membership| membership.workspace_id == "home-1")
        .map(|membership| {
            let user = state
                .platform
                .users
                .iter()
                .find(|user| user.user_id == membership.user_id);
            let identity_binding = state
                .platform
                .identity_bindings
                .iter()
                .find(|binding| binding.user_id == membership.user_id);

            AccessMemberSummary {
                user_id: membership.user_id.clone(),
                display_name: user
                    .map(|user| user.display_name.clone())
                    .or_else(|| {
                        identity_binding
                            .and_then(|binding| binding.profile_snapshot.get("display_name"))
                            .and_then(Value::as_str)
                            .map(|value| value.to_string())
                    })
                    .unwrap_or_else(|| membership.user_id.clone()),
                role_kind: role_kind_value(membership.role_kind).to_string(),
                membership_status: membership_status_value(membership.status).to_string(),
                source: identity_binding
                    .map(|binding| binding.provider_key.clone())
                    .unwrap_or_else(|| "local_console".to_string()),
                open_id: identity_binding.map(|binding| binding.external_user_id.clone()),
                chat_id: identity_binding.and_then(|binding| binding.external_chat_id.clone()),
                can_edit: membership.user_id != owner_user_id,
                is_owner: membership.user_id == owner_user_id
                    || membership.role_kind == RoleKind::Owner,
                proactive_delivery_surface: user
                    .and_then(user_default_delivery_surface)
                    .unwrap_or_else(|| "feishu".to_string()),
                proactive_delivery_default: true,
                binding_availability: if identity_binding.is_some() {
                    "available".to_string()
                } else {
                    "blocked".to_string()
                },
                binding_available: identity_binding.is_some(),
                binding_availability_note: if identity_binding.is_some() {
                    "HarborGate identity binding is available for member-default proactive delivery."
                        .to_string()
                } else {
                    "HarborGate identity binding is missing; proactive delivery will remain queued until a binding exists."
                        .to_string()
                },
                recent_interactive_surface: user.and_then(user_recent_interactive_surface),
            }
        })
        .collect();

    members.sort_by(|left, right| {
        right
            .is_owner
            .cmp(&left.is_owner)
            .then_with(|| left.display_name.cmp(&right.display_name))
    });
    members
}

fn role_kind_value(role_kind: RoleKind) -> &'static str {
    match role_kind {
        RoleKind::Owner => "owner",
        RoleKind::Admin => "admin",
        RoleKind::Operator => "operator",
        RoleKind::Member => "member",
        RoleKind::Viewer => "viewer",
        RoleKind::Guest => "guest",
    }
}

fn membership_status_value(status: MembershipStatus) -> &'static str {
    match status {
        MembershipStatus::Active => "active",
        MembershipStatus::Pending => "pending",
        MembershipStatus::Revoked => "revoked",
    }
}

fn render_models_admin_page() -> String {
    r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>HarborBeacon 模型中心</title>
  <style>
    :root {
      color-scheme: light;
      --bg: #f4efe7;
      --card: rgba(255,255,255,0.9);
      --line: #d9c6ae;
      --text: #1e1b18;
      --muted: #6b5a49;
      --accent: #1f7a6f;
      --danger: #b94739;
    }
    * { box-sizing: border-box; }
    body { margin: 0; font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text", "PingFang SC", sans-serif; background: var(--bg); color: var(--text); }
    .wrap { max-width: 980px; margin: 0 auto; padding: 24px 18px 48px; }
    .grid { display: grid; gap: 18px; grid-template-columns: 1.3fr 1fr; }
    .card { background: var(--card); border-radius: 22px; padding: 18px; box-shadow: 0 18px 48px rgba(51,36,18,0.12); }
    h1, h2 { margin: 0 0 10px; }
    .meta { color: var(--muted); font-size: 14px; line-height: 1.6; margin-bottom: 14px; }
    table { width: 100%; border-collapse: collapse; font-size: 14px; }
    th, td { text-align: left; padding: 10px 8px; border-bottom: 1px solid #eadcca; vertical-align: top; }
    th { color: var(--muted); font-weight: 600; }
    .chip { display: inline-block; padding: 4px 9px; border-radius: 999px; background: #f6f2ec; border: 1px solid #eadcca; margin-right: 6px; margin-bottom: 6px; font-size: 12px; }
    label { display: block; margin: 12px 0 6px; font-weight: 600; }
    input, select, textarea { width: 100%; padding: 11px 12px; border-radius: 12px; border: 1px solid var(--line); background: white; font: inherit; }
    textarea { min-height: 96px; resize: vertical; }
    .row { display: grid; grid-template-columns: 1fr 1fr; gap: 12px; }
    button { border: 0; border-radius: 999px; padding: 11px 16px; background: var(--accent); color: white; font-weight: 700; font-size: 14px; cursor: pointer; }
    button.secondary { background: #f6f2ec; color: var(--text); border: 1px solid var(--line); }
    .actions { display: flex; gap: 10px; flex-wrap: wrap; margin-top: 14px; }
    pre { background: #181512; color: #f5efe7; border-radius: 16px; padding: 14px; overflow: auto; font-size: 12px; }
    .ok { color: var(--accent); }
    .err { color: var(--danger); }
    @media (max-width: 860px) {
      .grid { grid-template-columns: 1fr; }
      .row { grid-template-columns: 1fr; }
    }
  </style>
</head>
<body>
  <div class="wrap">
    <div class="card" style="margin-bottom:18px;">
      <h1>模型中心</h1>
      <div class="meta">HarborBeacon 负责 OCR / embedder / LLM / VLM 的路由与红acted 管理。前端只看到脱敏状态，secret 只保存在后端状态文件里。</div>
      <div class="actions">
        <button id="refresh-btn" class="secondary">刷新状态</button>
      </div>
    </div>
    <div class="grid">
      <div class="card">
        <h2>端点列表</h2>
        <div class="meta">默认会显示本地 OCR / OpenAI-compatible 槽位。`api_key_configured=true` 表示后端已持有 secret，但不会回显明文。</div>
        <table>
          <thead>
            <tr>
              <th>ID</th>
              <th>种类</th>
              <th>路由</th>
              <th>状态</th>
              <th>配置</th>
            </tr>
          </thead>
          <tbody id="endpoint-body">
            <tr><td colspan="5">加载中...</td></tr>
          </tbody>
        </table>
        <div class="actions" id="endpoint-actions"></div>
      </div>
      <div class="card">
        <h2>新增 / 更新端点</h2>
        <div class="row">
          <div>
            <label for="endpoint-id">Endpoint ID</label>
            <input id="endpoint-id" placeholder="ocr-local-tesseract" />
          </div>
          <div>
            <label for="model-kind">Model Kind</label>
            <select id="model-kind">
              <option value="ocr">ocr</option>
              <option value="embedder">embedder</option>
              <option value="llm">llm</option>
              <option value="vlm">vlm</option>
            </select>
          </div>
        </div>
        <div class="row">
          <div>
            <label for="endpoint-kind">Endpoint Kind</label>
            <select id="endpoint-kind">
              <option value="local">local</option>
              <option value="sidecar">sidecar</option>
              <option value="cloud">cloud</option>
            </select>
          </div>
          <div>
            <label for="provider-key">Provider</label>
            <input id="provider-key" placeholder="tesseract / ollama / custom" />
          </div>
        </div>
        <div class="row">
          <div>
            <label for="model-name">Model Name</label>
            <input id="model-name" placeholder="qwen2.5:7b / tesseract-cli" />
          </div>
          <div>
            <label for="status">Status</label>
            <select id="status">
              <option value="active">active</option>
              <option value="degraded">degraded</option>
              <option value="disabled">disabled</option>
            </select>
          </div>
        </div>
        <label for="capability-tags">Capability Tags（逗号分隔）</label>
        <input id="capability-tags" placeholder="ocr,image,local_first" />
        <label for="base-url">Base URL</label>
        <input id="base-url" placeholder="http://127.0.0.1:11434/v1" />
        <label for="api-key">API Key</label>
        <input id="api-key" placeholder="可留空；只会写入后端，不会回显" />
        <label for="binary-path">Tesseract Binary</label>
        <input id="binary-path" placeholder="留空则自动查找 PATH" />
        <label for="languages">OCR Languages</label>
        <input id="languages" value="chi_sim+eng" />
        <label for="metadata-json">Metadata JSON（可选）</label>
        <textarea id="metadata-json" placeholder='{"mock_text":"front gate"}'></textarea>
        <div class="actions">
          <button id="save-endpoint-btn">保存端点</button>
        </div>
      </div>
    </div>
    <div class="grid" style="margin-top:18px;">
      <div class="card">
        <h2>路由策略</h2>
        <div class="meta">这里直接编辑 `retrieval.ocr / retrieval.embed / retrieval.answer / retrieval.vision_summary` 的 JSON 数组。</div>
        <textarea id="policies-json" style="min-height:260px;"></textarea>
        <div class="actions">
          <button id="save-policies-btn">保存策略</button>
        </div>
      </div>
      <div class="card">
        <h2>连通性测试</h2>
        <div class="meta">点击端点表中的测试按钮后，这里会显示结果。</div>
        <pre id="test-result">等待测试</pre>
      </div>
    </div>
  </div>
  <script>
    const endpointBody = document.getElementById("endpoint-body");
    const endpointActions = document.getElementById("endpoint-actions");
    const policiesJson = document.getElementById("policies-json");
    const testResult = document.getElementById("test-result");

    async function fetchJson(path, options = {}) {
      const response = await fetch(path, {
        headers: { "Content-Type": "application/json", ...(options.headers || {}) },
        ...options,
      });
      const payload = await response.json().catch(() => ({}));
      if (!response.ok) {
        throw new Error(payload.error || payload.message || `Request failed: ${response.status}`);
      }
      return payload;
    }

    function endpointConfigSummary(endpoint) {
      const metadata = endpoint.metadata || {};
      const summary = [];
      if (metadata.base_url) summary.push(`base_url=${metadata.base_url}`);
      if (metadata.binary_path) summary.push(`binary=${metadata.binary_path}`);
      if (metadata.languages) summary.push(`langs=${metadata.languages}`);
      if (metadata.api_key_configured) summary.push("api_key=configured");
      return summary.join(" | ") || "未配置";
    }

    function renderEndpoints(endpoints) {
      endpointBody.innerHTML = "";
      endpointActions.innerHTML = "";
      if (!endpoints.length) {
        endpointBody.innerHTML = '<tr><td colspan="5">还没有模型端点。</td></tr>';
        return;
      }
      for (const endpoint of endpoints) {
        const row = document.createElement("tr");
        row.innerHTML = `
          <td><strong>${endpoint.model_endpoint_id}</strong></td>
          <td>${endpoint.model_kind}</td>
          <td>${endpoint.endpoint_kind}<br /><span class="chip">${endpoint.provider_key}</span></td>
          <td>${endpoint.status}</td>
          <td>${endpointConfigSummary(endpoint)}</td>
        `;
        endpointBody.appendChild(row);

        const button = document.createElement("button");
        button.className = "secondary";
        button.textContent = `测试 ${endpoint.model_endpoint_id}`;
        button.addEventListener("click", async () => {
          testResult.textContent = "测试中...";
          try {
            const payload = await fetchJson(`/api/models/endpoints/${encodeURIComponent(endpoint.model_endpoint_id)}/test`, {
              method: "POST",
              body: JSON.stringify({}),
            });
            testResult.textContent = JSON.stringify(payload, null, 2);
          } catch (error) {
            testResult.textContent = error.message;
          }
        });
        endpointActions.appendChild(button);
      }
    }

    async function loadState() {
      const [endpointPayload, policyPayload] = await Promise.all([
        fetchJson("/api/models/endpoints"),
        fetchJson("/api/models/policies"),
      ]);
      renderEndpoints(endpointPayload.endpoints || []);
      policiesJson.value = JSON.stringify(policyPayload.route_policies || [], null, 2);
    }

    function collectEndpointPayload() {
      let metadata = {};
      const rawMetadata = document.getElementById("metadata-json").value.trim();
      if (rawMetadata) {
        metadata = JSON.parse(rawMetadata);
      }
      const baseUrl = document.getElementById("base-url").value.trim();
      const apiKey = document.getElementById("api-key").value.trim();
      const binaryPath = document.getElementById("binary-path").value.trim();
      const languages = document.getElementById("languages").value.trim();
      if (baseUrl) metadata.base_url = baseUrl;
      if (apiKey) metadata.api_key = apiKey;
      if (binaryPath) metadata.binary_path = binaryPath;
      if (languages) metadata.languages = languages;
      return {
        model_endpoint_id: document.getElementById("endpoint-id").value.trim(),
        workspace_id: "home-1",
        provider_account_id: null,
        model_kind: document.getElementById("model-kind").value,
        endpoint_kind: document.getElementById("endpoint-kind").value,
        provider_key: document.getElementById("provider-key").value.trim() || "custom",
        model_name: document.getElementById("model-name").value.trim() || "custom",
        capability_tags: document.getElementById("capability-tags").value.split(",").map((item) => item.trim()).filter(Boolean),
        cost_policy: {},
        status: document.getElementById("status").value,
        metadata,
      };
    }

    document.getElementById("refresh-btn").addEventListener("click", loadState);
    document.getElementById("save-endpoint-btn").addEventListener("click", async () => {
      try {
        const payload = collectEndpointPayload();
        await fetchJson("/api/models/endpoints", {
          method: "POST",
          body: JSON.stringify(payload),
        });
        await loadState();
      } catch (error) {
        testResult.textContent = error.message;
      }
    });
    document.getElementById("save-policies-btn").addEventListener("click", async () => {
      try {
        const route_policies = JSON.parse(policiesJson.value || "[]");
        await fetchJson("/api/models/policies", {
          method: "PUT",
          body: JSON.stringify({ route_policies }),
        });
        await loadState();
      } catch (error) {
        testResult.textContent = error.message;
      }
    });
    loadState().catch((error) => {
      endpointBody.innerHTML = `<tr><td colspan="5" class="err">${error.message}</td></tr>`;
      testResult.textContent = error.message;
    });
  </script>
</body>
</html>"#
        .to_string()
}

fn render_live_view_page(
    public_origin: &str,
    device: &harborbeacon_local_agent::runtime::registry::CameraDevice,
    identity_query: &str,
) -> String {
    let device_label = device.room.as_deref().unwrap_or(device.name.as_str());
    let device_label = html_escape(device_label);
    let device_name = html_escape(&device.name);
    let ip_address = html_escape(device.ip_address.as_deref().unwrap_or("未知 IP"));
    let device_id = url_encode_path_segment(&device.device_id);
    let origin = public_origin.trim_end_matches('/');
    let live_stream_url = format!("{origin}/api/cameras/{device_id}/live.mjpeg{identity_query}");
    let snapshot_url = format!("{origin}/api/cameras/{device_id}/snapshot.jpg{identity_query}");

    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>{device_label} 实时观看</title>
  <style>
    :root {{
      color-scheme: light;
      --bg: #0f1720;
      --card: rgba(10, 18, 28, 0.82);
      --line: rgba(255,255,255,0.12);
      --text: #f3f7fb;
      --muted: #98a8bb;
      --accent: #4fd1c5;
      --danger: #ff8f70;
    }}
    * {{ box-sizing: border-box; }}
    body {{
      margin: 0;
      font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text", "PingFang SC", sans-serif;
      background:
        radial-gradient(circle at top, rgba(79,209,197,0.22), transparent 35%),
        linear-gradient(180deg, #0c1218 0%, #101927 100%);
      color: var(--text);
      min-height: 100vh;
    }}
    .wrap {{ max-width: 880px; margin: 0 auto; padding: 20px 16px 28px; }}
    .topbar {{
      display: flex;
      align-items: center;
      justify-content: space-between;
      gap: 12px;
      margin-bottom: 14px;
    }}
    .title {{ margin: 0; font-size: 28px; line-height: 1.1; }}
    .meta {{ color: var(--muted); font-size: 14px; margin-top: 6px; }}
    .status {{
      display: inline-flex;
      align-items: center;
      gap: 8px;
      padding: 8px 12px;
      border-radius: 999px;
      background: rgba(255,255,255,0.08);
      border: 1px solid rgba(255,255,255,0.1);
      font-size: 13px;
      white-space: nowrap;
    }}
    .status-dot {{
      width: 10px;
      height: 10px;
      border-radius: 999px;
      background: var(--danger);
      box-shadow: 0 0 0 4px rgba(255,143,112,0.18);
    }}
    .status.live .status-dot {{
      background: var(--accent);
      box-shadow: 0 0 0 4px rgba(79,209,197,0.18);
    }}
    .panel {{
      background: var(--card);
      border: 1px solid var(--line);
      border-radius: 24px;
      padding: 14px;
      box-shadow: 0 24px 60px rgba(0,0,0,0.28);
      backdrop-filter: blur(12px);
    }}
    .viewer {{
      position: relative;
      overflow: hidden;
      border-radius: 18px;
      background: #060b11;
      aspect-ratio: 16 / 9;
    }}
    .viewer img {{
      width: 100%;
      height: 100%;
      display: block;
      object-fit: contain;
      background: #060b11;
    }}
    .overlay {{
      position: absolute;
      left: 12px;
      right: 12px;
      bottom: 12px;
      display: flex;
      justify-content: space-between;
      gap: 12px;
      flex-wrap: wrap;
    }}
    .chip {{
      padding: 8px 10px;
      border-radius: 12px;
      background: rgba(3, 10, 17, 0.72);
      border: 1px solid rgba(255,255,255,0.1);
      font-size: 13px;
      color: var(--muted);
    }}
    .actions {{
      display: flex;
      gap: 10px;
      flex-wrap: wrap;
      margin-top: 14px;
    }}
    .actions button, .actions a {{
      appearance: none;
      border: 0;
      text-decoration: none;
      color: var(--text);
      background: rgba(255,255,255,0.08);
      border: 1px solid rgba(255,255,255,0.1);
      border-radius: 999px;
      padding: 11px 16px;
      font-size: 14px;
      font-weight: 600;
    }}
    .actions .primary {{
      background: linear-gradient(135deg, #27c4b3, #1f8f85);
      border-color: transparent;
    }}
    .hint {{
      margin-top: 14px;
      color: var(--muted);
      font-size: 13px;
      line-height: 1.6;
    }}
    @media (max-width: 640px) {{
      .title {{ font-size: 22px; }}
      .topbar {{ align-items: flex-start; flex-direction: column; }}
      .actions button, .actions a {{ flex: 1 1 calc(50% - 10px); text-align: center; }}
    }}
  </style>
</head>
<body>
  <div class="wrap">
    <div class="topbar">
      <div>
        <h1 class="title">{device_label} 实时观看</h1>
        <div class="meta">{device_name} · {ip_address} · 浏览器内低延迟 MJPEG 预览</div>
      </div>
      <div id="status" class="status">
        <span class="status-dot"></span>
        <span id="status-text">正在连接画面…</span>
      </div>
    </div>

    <div class="panel">
      <div class="viewer">
        <img id="stream" src="{live_stream_url}" alt="{device_label} 实时画面" />
        <div class="overlay">
          <div class="chip">链路：RTSP → 本地 ffmpeg → MJPEG</div>
          <div class="chip" id="last-frame">等待首帧…</div>
        </div>
      </div>

      <div class="actions">
        <button id="reload-btn" class="primary" type="button">重连画面</button>
        <a href="{snapshot_url}" target="_blank" rel="noreferrer">打开当前截图</a>
      </div>

      <div class="hint">
        如果画面没有出来，先确认手机和 HarborBeacon 在同一个局域网，再点击“重连画面”。
        这个页面只负责看实时视频；拍照、录像、云台控制仍然建议继续在统一 IM 入口里完成。
      </div>
    </div>
  </div>

  <script>
    const streamUrl = {live_stream_url:?};
    const reloadSeparator = streamUrl.includes('?') ? '&' : '?';
    const streamEl = document.getElementById('stream');
    const statusEl = document.getElementById('status');
    const statusTextEl = document.getElementById('status-text');
    const lastFrameEl = document.getElementById('last-frame');

    function setStatus(isLive, text) {{
      statusEl.classList.toggle('live', isLive);
      statusTextEl.textContent = text;
    }}

    function reloadStream() {{
      setStatus(false, '正在重连画面…');
      streamEl.src = `${{streamUrl}}${{reloadSeparator}}ts=${{Date.now()}}`;
    }}

    streamEl.addEventListener('load', () => {{
      setStatus(true, '实时画面连接中');
      lastFrameEl.textContent = `最后更新：${{new Date().toLocaleTimeString()}}`;
    }});

    streamEl.addEventListener('error', () => {{
      setStatus(false, '画面连接失败，请重试');
    }});

    document.getElementById('reload-btn').addEventListener('click', reloadStream);
  </script>
</body>
</html>"#
    )
}

fn render_shared_live_view_page(
    share_token: &str,
    device: &harborbeacon_local_agent::runtime::registry::CameraDevice,
) -> String {
    let device_label = html_escape(device.room.as_deref().unwrap_or(device.name.as_str()));
    let device_name = html_escape(&device.name);
    let ip_address = html_escape(device.ip_address.as_deref().unwrap_or("未知 IP"));
    let live_stream_url = format!(
        "/shared/cameras/{}/live.mjpeg",
        url_encode_path_segment(share_token)
    );

    format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>{device_label} 远程观看</title>
  <style>
    :root {{
      color-scheme: light;
      --bg: #0d1119;
      --card: rgba(7, 14, 24, 0.86);
      --line: rgba(255,255,255,0.12);
      --text: #eff6ff;
      --muted: #98a7ba;
      --accent: #58d6c2;
      --danger: #ff9f79;
    }}
    * {{ box-sizing: border-box; }}
    body {{
      margin: 0;
      min-height: 100vh;
      color: var(--text);
      font-family: -apple-system, BlinkMacSystemFont, "SF Pro Text", "PingFang SC", sans-serif;
      background:
        radial-gradient(circle at top, rgba(88,214,194,0.16), transparent 34%),
        linear-gradient(180deg, #0b1118 0%, #0e1723 100%);
    }}
    .wrap {{ max-width: 880px; margin: 0 auto; padding: 18px 14px 28px; }}
    .header {{ display: flex; justify-content: space-between; gap: 12px; align-items: flex-start; margin-bottom: 14px; }}
    .title {{ margin: 0; font-size: 28px; line-height: 1.1; }}
    .meta {{ margin-top: 8px; color: var(--muted); font-size: 14px; line-height: 1.5; }}
    .status {{
      display: inline-flex;
      align-items: center;
      gap: 8px;
      padding: 8px 12px;
      border-radius: 999px;
      background: rgba(255,255,255,0.08);
      border: 1px solid rgba(255,255,255,0.1);
      font-size: 13px;
      white-space: nowrap;
    }}
    .status-dot {{
      width: 10px;
      height: 10px;
      border-radius: 999px;
      background: var(--danger);
      box-shadow: 0 0 0 4px rgba(255,159,121,0.16);
    }}
    .status.live .status-dot {{
      background: var(--accent);
      box-shadow: 0 0 0 4px rgba(88,214,194,0.18);
    }}
    .panel {{
      padding: 14px;
      border-radius: 24px;
      background: var(--card);
      border: 1px solid var(--line);
      box-shadow: 0 24px 64px rgba(0,0,0,0.28);
      backdrop-filter: blur(12px);
    }}
    .viewer {{
      position: relative;
      overflow: hidden;
      border-radius: 18px;
      background: #05080d;
      aspect-ratio: 16 / 9;
    }}
    .viewer img {{
      width: 100%;
      height: 100%;
      object-fit: contain;
      display: block;
      background: #05080d;
    }}
    .overlay {{
      position: absolute;
      left: 12px;
      right: 12px;
      bottom: 12px;
      display: flex;
      justify-content: space-between;
      gap: 12px;
      flex-wrap: wrap;
    }}
    .chip {{
      padding: 8px 10px;
      border-radius: 12px;
      background: rgba(3, 10, 17, 0.74);
      border: 1px solid rgba(255,255,255,0.08);
      color: var(--muted);
      font-size: 13px;
    }}
    .actions {{ display: flex; gap: 10px; flex-wrap: wrap; margin-top: 14px; }}
    .actions button {{
      appearance: none;
      border: 0;
      border-radius: 999px;
      padding: 11px 16px;
      font-size: 14px;
      font-weight: 600;
      color: var(--text);
      background: linear-gradient(135deg, #28c6b5, #1d8d82);
    }}
    .hint {{ margin-top: 14px; color: var(--muted); font-size: 13px; line-height: 1.6; }}
    @media (max-width: 640px) {{
      .title {{ font-size: 22px; }}
      .header {{ flex-direction: column; }}
    }}
  </style>
</head>
<body>
  <div class="wrap">
    <div class="header">
      <div>
        <h1 class="title">{device_label} 远程观看</h1>
        <div class="meta">{device_name} · {ip_address} · 这是一个带签名的临时分享链接，仅用于看实时画面。</div>
      </div>
      <div id="status" class="status">
        <span class="status-dot"></span>
        <span id="status-text">正在连接画面…</span>
      </div>
    </div>

    <div class="panel">
      <div class="viewer">
        <img id="stream" src="{live_stream_url}" alt="{device_label} 远程实时画面" />
        <div class="overlay">
          <div class="chip">链路：公网入口 → 本地 ffmpeg → MJPEG</div>
          <div class="chip" id="last-frame">等待首帧…</div>
        </div>
      </div>

      <div class="actions">
        <button id="reload-btn" type="button">重连画面</button>
      </div>

      <div class="hint">
        这个链接默认会在一段时间后自动过期。分享出去时请只发给需要查看的人，不要长期公开传播。
      </div>
    </div>
  </div>

  <script>
    const streamUrl = {live_stream_url:?};
    const streamEl = document.getElementById('stream');
    const statusEl = document.getElementById('status');
    const statusTextEl = document.getElementById('status-text');
    const lastFrameEl = document.getElementById('last-frame');

    function setStatus(isLive, text) {{
      statusEl.classList.toggle('live', isLive);
      statusTextEl.textContent = text;
    }}

    function reloadStream() {{
      setStatus(false, '正在重连画面…');
      streamEl.src = `${{streamUrl}}?ts=${{Date.now()}}`;
    }}

    streamEl.addEventListener('load', () => {{
      setStatus(true, '远程画面连接中');
      lastFrameEl.textContent = `最后更新：${{new Date().toLocaleTimeString()}}`;
    }});

    streamEl.addEventListener('error', () => {{
      setStatus(false, '画面连接失败，请重试');
    }});

    document.getElementById('reload-btn').addEventListener('click', reloadStream);
  </script>
</body>
</html>"#
    )
}

fn html_escape(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => {
                let _ = escaped.write_char(ch);
            }
        }
    }
    escaped
}

fn read_json_body<T: for<'de> Deserialize<'de>>(request: &mut Request) -> Result<T, String> {
    let mut body = String::new();
    request
        .as_reader()
        .read_to_string(&mut body)
        .map_err(|e| format!("failed to read request body: {e}"))?;
    serde_json::from_str(&body).map_err(|e| format!("invalid JSON body: {e}"))
}

fn read_json_body_limited<T: for<'de> Deserialize<'de>>(
    request: &mut Request,
    max_bytes: usize,
) -> Result<T, String> {
    let mut body = Vec::new();
    request
        .as_reader()
        .take(max_bytes.saturating_add(1) as u64)
        .read_to_end(&mut body)
        .map_err(|e| format!("failed to read request body: {e}"))?;
    if body.len() > max_bytes {
        return Err(format!("request body exceeds {max_bytes} bytes"));
    }
    parse_json_body_limited(&body, max_bytes)
}

fn parse_json_body_limited<T: for<'de> Deserialize<'de>>(
    body: &[u8],
    max_bytes: usize,
) -> Result<T, String> {
    if body.len() > max_bytes {
        return Err(format!("request body exceeds {max_bytes} bytes"));
    }
    serde_json::from_slice(body).map_err(|e| format!("invalid JSON body: {e}"))
}

fn read_json_body_or_default<T>(request: &mut Request) -> Result<T, String>
where
    T: for<'de> Deserialize<'de> + Default,
{
    let mut body = String::new();
    request
        .as_reader()
        .read_to_string(&mut body)
        .map_err(|e| format!("failed to read request body: {e}"))?;
    if body.trim().is_empty() {
        return Ok(T::default());
    }
    serde_json::from_str(&body).map_err(|e| format!("invalid JSON body: {e}"))
}

fn ok_json(payload: &impl Serialize) -> Response<std::io::Cursor<Vec<u8>>> {
    json_response(StatusCode(200), payload)
}

fn image_response(
    status: StatusCode,
    bytes: Vec<u8>,
    mime_type: &str,
) -> Response<std::io::Cursor<Vec<u8>>> {
    let mut response = Response::from_data(bytes).with_status_code(status);
    add_common_headers(&mut response);
    response.add_header(
        Header::from_bytes(b"Content-Type".as_slice(), mime_type.as_bytes()).expect("header"),
    );
    response
}

fn no_content() -> Response<std::io::Cursor<Vec<u8>>> {
    let mut response = Response::from_data(Vec::new()).with_status_code(StatusCode(204));
    add_common_headers(&mut response);
    response
}

fn error_json(status: StatusCode, message: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    json_response(status, &json!({ "error": message }))
}

fn deprecated_im_binding_message() -> &'static str {
    "IM configuration has moved to HarborGate. HarborBeacon no longer serves IM setup, binding, or QR flows."
}

fn deprecated_im_binding_response_json(manage_url: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    json_response(
        StatusCode(410),
        &json!({
            "error": deprecated_im_binding_message(),
            "manage_url": manage_url,
        }),
    )
}

fn deprecated_im_binding_response_html(manage_url: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    let manage_url = html_escape(manage_url);
    let body = format!(
        r#"<!DOCTYPE html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>IM 配置已迁移</title>
  <style>
    body {{ font-family: -apple-system, BlinkMacSystemFont, sans-serif; background: #f4efe7; color: #1e1b18; margin: 0; }}
    .wrap {{ max-width: 560px; margin: 0 auto; padding: 36px 18px 48px; }}
    .card {{ background: rgba(255,255,255,0.92); border-radius: 20px; padding: 24px; box-shadow: 0 18px 48px rgba(51,36,18,0.12); }}
    h1 {{ margin-top: 0; }}
    code, a {{ word-break: break-all; }}
  </style>
</head>
<body>
  <div class="wrap">
    <div class="card">
      <h1>IM 配置已迁移到 HarborGate</h1>
      <p>{}</p>
      <p>HarborBeacon 现在只保留业务后台与 HarborGate 状态读取，不再提供任何 IM 扫码、绑定或登录入口。</p>
      <p>HarborGate 管理页：<a href="{manage_url}">{manage_url}</a></p>
    </div>
  </div>
</body>
</html>"#,
        html_escape(deprecated_im_binding_message())
    );
    let mut response = Response::from_string(body).with_status_code(StatusCode(410));
    add_common_headers(&mut response);
    response.add_header(
        Header::from_bytes(
            b"Content-Type".as_slice(),
            b"text/html; charset=utf-8".as_slice(),
        )
        .expect("header"),
    );
    response
}

fn authorize_gateway_service_request(headers: &[Header]) -> Result<(), String> {
    let expected =
        env_var_with_legacy_alias("HARBORGATE_BEARER_TOKEN", "HARBOR_IM_GATEWAY_BEARER_TOKEN")
            .or_else(|| env::var("IM_AGENT_SERVICE_TOKEN").ok())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "gateway service token is not configured".to_string())?;
    let actual = header_value(headers, "Authorization")
        .and_then(|value| parse_bearer_token(&value))
        .ok_or_else(|| "missing or invalid bearer token".to_string())?;
    if actual != expected {
        return Err("missing or invalid bearer token".to_string());
    }
    Ok(())
}

fn parse_bearer_token(value: &str) -> Option<String> {
    let prefix = "bearer ";
    value
        .trim()
        .to_ascii_lowercase()
        .strip_prefix(prefix)
        .map(|_| value.trim()[prefix.len()..].trim().to_string())
        .filter(|token| !token.is_empty())
}

fn json_response(
    status: StatusCode,
    payload: &impl Serialize,
) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = serde_json::to_vec_pretty(payload)
        .unwrap_or_else(|_| b"{\"error\":\"serialize failed\"}".to_vec());
    let mut response = Response::from_data(body).with_status_code(status);
    add_common_headers(&mut response);
    response.add_header(
        Header::from_bytes(
            b"Content-Type".as_slice(),
            b"application/json; charset=utf-8".as_slice(),
        )
        .expect("header"),
    );
    response
}

fn add_common_headers<R: Read>(response: &mut Response<R>) {
    for header in [
        ("Access-Control-Allow-Origin", "*"),
        ("Access-Control-Allow-Headers", "Content-Type"),
        (
            "Access-Control-Allow-Methods",
            "GET, POST, PATCH, PUT, OPTIONS",
        ),
        ("Cache-Control", "no-store"),
    ] {
        response.add_header(
            Header::from_bytes(header.0.as_bytes(), header.1.as_bytes()).expect("header"),
        );
    }
}

fn is_admin_surface_path(path: &str) -> bool {
    path == "/api/state"
        || path == "/api/account-management"
        || path == "/api/gateway/status"
        || path == "/api/release/readiness"
        || path == "/api/release/readiness/history"
        || path == "/api/release/readiness/run"
        || path == "/api/hardware/readiness"
        || path == "/api/rag/readiness"
        || path == "/api/evt/readiness"
        || path == "/api/evt/preflight"
        || path == "/api/evt/preflight/latest"
        || path == "/api/evt/evidence-bundle"
        || path == "/api/diagnostics/redacted-bundle"
        || path == "/api/routing/status"
        || path == "/api/audit/records"
        || path == "/api/audit/summary"
        || path == "/api/family/timeline"
        || path == "/api/family/timeline/digest"
        || path == "/api/family/memory/events"
        || path == "/api/family/memory/stats"
        || path.starts_with("/api/family/memory/events/")
        || path == "/api/home-guardian/activity"
        || path == "/api/knowledge/settings"
        || path == "/api/knowledge/search"
        || path == "/api/knowledge/preview"
        || path == "/api/knowledge/index/run"
        || path == "/api/knowledge/index/status"
        || path == "/api/knowledge/index/jobs"
        || (path.starts_with("/api/knowledge/index/jobs/") && path.ends_with("/cancel"))
        || path == "/api/files/browse"
        || path == "/api/cameras/recording-settings"
        || path == "/api/cameras/recordings/status"
        || path == "/api/cameras/recordings/timeline"
        || path == "/api/harboros/status"
        || path == "/api/harboros/im-capability-map"
        || path == "/api/home-assistant/status"
        || path == "/api/home-assistant/config"
        || path == "/api/home-assistant/test"
        || path == "/api/home-assistant/sync"
        || path == "/api/home-assistant/entities"
        || path == "/api/home-assistant/services"
        || path == "/api/home-assistant/service-smoke"
        || path == "/api/home-assistant/service-action"
        || path == "/api/harboros/apps/home-assistant/status"
        || path == "/api/harboros/apps/home-assistant/install-plan"
        || path == "/api/harboros/apps/home-assistant/install"
        || path == "/api/models/endpoints"
        || path == "/api/models/capabilities"
        || path == "/api/models/runtimes"
        || path == "/api/inference/healthz"
        || path == "/api/models/store"
        || path == "/api/models/local-catalog"
        || path == "/api/models/policies"
        || path == "/api/vision/events"
        || path == "/api/vision/vlm/status"
        || path == "/api/vision/events/latest/vlm-enrich"
        || (path.starts_with("/api/vision/events/") && path.ends_with("/notify"))
        || (path.starts_with("/api/vision/events/") && path.ends_with("/vlm-enrich"))
        || path == "/admin/models"
        || path == "/api/access/members"
        || path == "/api/share-links"
        || path == "/api/binding/qr.svg"
        || path == "/api/binding/static-qr.svg"
        || path == "/setup/mobile"
        || path == "/api/binding/refresh"
        || path == "/api/binding/demo-bind"
        || path == "/api/binding/test-bind"
        || path == "/api/bridge/configure"
        || (path.starts_with("/api/access/members/") && path.ends_with("/role"))
        || (path.starts_with("/api/access/members/") && path.ends_with("/default-delivery-surface"))
        || path == "/api/tasks/approvals"
        || path.starts_with("/api/tasks/approvals/")
        || path == "/api/automation/reviews"
        || path.starts_with("/api/automation/reviews/")
        || path == "/api/discovery/scan"
        || path == "/api/devices/manual"
        || path == "/api/devices/default-camera"
        || (path.starts_with("/api/devices/") && !path.contains("/../"))
        || (path.starts_with("/api/devices/") && path.ends_with("/credentials"))
        || (path.starts_with("/api/devices/") && path.ends_with("/credential-status"))
        || (path.starts_with("/api/devices/") && path.ends_with("/rtsp-check"))
        || (path.starts_with("/api/cameras/") && path.ends_with("/share-link"))
        || (path.starts_with("/api/cameras/") && path.ends_with("/snapshot.jpg"))
        || (path.starts_with("/api/cameras/") && path.ends_with("/live.mjpeg"))
        || (path.starts_with("/api/cameras/") && path.ends_with("/live/start"))
        || (path.starts_with("/api/cameras/") && path.ends_with("/live/stop"))
        || (path.starts_with("/api/cameras/") && path.ends_with("/live/status"))
        || parse_camera_hls_live_asset_path(path).is_some()
        || (path.starts_with("/api/cameras/") && path.ends_with("/recordings/start"))
        || (path.starts_with("/api/cameras/") && path.ends_with("/recordings/stop"))
        || (path.starts_with("/api/share-links/") && path.ends_with("/revoke"))
        || (path.starts_with("/api/cameras/") && path.ends_with("/snapshot"))
        || (path.starts_with("/api/cameras/") && path.ends_with("/analyze"))
        || path == "/api/defaults"
        || path.starts_with("/api/models/endpoints/")
        || path == "/api/models/local-downloads"
        || path.starts_with("/api/models/local-downloads/")
        || (path.starts_with("/api/models/runtimes/") && path.ends_with("/install"))
        || (path.starts_with("/api/models/capabilities/") && path.ends_with("/selection"))
}

fn is_harbor_assistant_client_route(path: &str) -> bool {
    matches!(
        path,
        "/" | "/overview"
            | "/im-gateway"
            | "/account-management"
            | "/tasks-approvals"
            | "/devices-aiot"
            | "/home-assistant"
            | "/harboros"
            | "/models-policies"
            | "/system-settings"
    )
}

fn looks_like_harbor_assistant_asset_path(path: &str) -> bool {
    path.starts_with("/assets/")
        || [
            ".js",
            ".css",
            ".map",
            ".json",
            ".png",
            ".svg",
            ".ico",
            ".txt",
            ".webmanifest",
            ".woff",
            ".woff2",
        ]
        .iter()
        .any(|extension| path.ends_with(extension))
}

fn is_harbor_assistant_surface_path(path: &str) -> bool {
    is_harbor_assistant_client_route(path) || looks_like_harbor_assistant_asset_path(path)
}

fn resolve_harbor_assistant_asset_path(dist_root: &Path, request_path: &str) -> Option<PathBuf> {
    if !looks_like_harbor_assistant_asset_path(request_path) {
        return None;
    }

    let relative = request_path.trim_start_matches('/');
    if relative.is_empty() {
        return None;
    }

    let mut resolved = dist_root.to_path_buf();
    for component in Path::new(relative).components() {
        match component {
            std::path::Component::Normal(segment) => resolved.push(segment),
            _ => return None,
        }
    }
    Some(resolved)
}

fn mime_type_for_path(path: &Path) -> &'static str {
    match path.extension().and_then(|value| value.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "application/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("bmp") => "image/bmp",
        Some("mp4") | Some("m4v") => "video/mp4",
        Some("mov") => "video/quicktime",
        Some("mkv") => "video/x-matroska",
        Some("webm") => "video/webm",
        Some("avi") => "video/x-msvideo",
        Some("ico") => "image/x-icon",
        Some("txt") => "text/plain; charset=utf-8",
        Some("md") | Some("markdown") => "text/markdown; charset=utf-8",
        Some("map") => "application/json; charset=utf-8",
        Some("webmanifest") => "application/manifest+json; charset=utf-8",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn static_file_response(path: &Path) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = match fs::read(path) {
        Ok(payload) => payload,
        Err(error) => {
            return error_json(
                StatusCode(500),
                &format!("failed to read static file {}: {error}", path.display()),
            )
        }
    };
    let mut response = Response::from_data(body).with_status_code(StatusCode(200));
    add_common_headers(&mut response);
    response.add_header(
        Header::from_bytes(
            b"Content-Type".as_slice(),
            mime_type_for_path(path).as_bytes(),
        )
        .expect("header"),
    );
    response
}

fn harbor_assistant_build_missing_response(dist_root: &Path) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Harbor Assistant build missing</title></head><body><h1>Harbor Assistant build missing</h1><p>Angular build output was not found at <code>{}</code>.</p><p>Run <code>npm install</code> and <code>npm run build</code> under <code>frontend/harbor-assistant</code>, or pass <code>--harbor-assistant-dist</code>.</p></body></html>",
        dist_root.display()
    );
    let mut response = Response::from_string(body).with_status_code(StatusCode(503));
    add_common_headers(&mut response);
    response.add_header(
        Header::from_bytes(
            b"Content-Type".as_slice(),
            b"text/html; charset=utf-8".as_slice(),
        )
        .expect("header"),
    );
    response
}

fn build_share_link_summary(
    share_link: ShareLink,
    media_session: &MediaSession,
    device_name: Option<&String>,
    now_unix_secs: u64,
) -> ShareLinkSummary {
    let status = share_link_status(&share_link, media_session, now_unix_secs);
    ShareLinkSummary {
        share_link_id: share_link.share_link_id.clone(),
        media_session_id: media_session.media_session_id.clone(),
        device_id: media_session.device_id.clone(),
        device_name: device_name
            .cloned()
            .unwrap_or_else(|| media_session.device_id.clone()),
        opened_by_user_id: media_session.opened_by_user_id.clone(),
        access_scope: serde_json::to_value(share_link.access_scope)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_else(|| "public_link".to_string()),
        session_status: serde_json::to_value(media_session.status)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_else(|| "unknown".to_string()),
        status: status.to_string(),
        expires_at: share_link.expires_at.clone(),
        revoked_at: share_link.revoked_at.clone(),
        started_at: media_session.started_at.clone(),
        ended_at: media_session.ended_at.clone(),
        can_revoke: status == "active",
    }
}

fn share_link_status(
    share_link: &ShareLink,
    media_session: &MediaSession,
    now_unix_secs: u64,
) -> &'static str {
    if share_link
        .revoked_at
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
    {
        return "revoked";
    }

    if let Some(expires_at) = share_link.expires_at.as_deref() {
        if let Ok(expires_at) = expires_at.trim().parse::<u64>() {
            if now_unix_secs > expires_at {
                return "expired";
            }
        }
    }

    match media_session.status {
        MediaSessionStatus::Opening | MediaSessionStatus::Active => "active",
        MediaSessionStatus::Closed => "closed",
        MediaSessionStatus::Failed => "failed",
    }
}

fn ensure_local_admin_access(
    remote_addr: Option<SocketAddr>,
    headers: &[Header],
) -> Result<(), String> {
    if remote_addr.is_some_and(|addr| !is_local_socket_addr(addr)) {
        return Err("当前管理后台接口只允许本机或局域网内访问。".to_string());
    }

    if !forwarded_client_chain_is_local(headers) {
        return Err(
            "当前管理后台接口只允许在本机或局域网内直连访问，不能通过公网反向代理转发。"
                .to_string(),
        );
    }

    Ok(())
}

fn ensure_local_camera_access(
    remote_addr: Option<SocketAddr>,
    headers: &[Header],
) -> Result<(), String> {
    if remote_addr.is_some_and(|addr| !is_local_socket_addr(addr)) {
        return Err(
            "当前摄像头直连预览只允许本机或局域网访问；如果要给外网用户观看，请使用带签名的共享链接。"
                .to_string(),
        );
    }

    if !forwarded_client_chain_is_local(headers) {
        return Err("当前摄像头直连预览只允许本机或局域网直连访问；如果要给外网用户观看，请使用带签名的共享链接。".to_string());
    }

    Ok(())
}

fn is_local_socket_addr(addr: SocketAddr) -> bool {
    is_local_ip(addr.ip())
}

fn is_local_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => ip.is_loopback() || is_private_ipv4(ip),
        IpAddr::V6(ip) => ip.is_loopback() || ip.is_unique_local(),
    }
}

fn is_private_ipv4(ip: Ipv4Addr) -> bool {
    let octets = ip.octets();
    octets[0] == 10
        || (octets[0] == 172 && (16..=31).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 168)
}

fn has_forwarding_headers(headers: &[Header]) -> bool {
    headers.iter().any(|header| {
        header.field.equiv("Forwarded")
            || header.field.equiv("X-Forwarded-For")
            || header.field.equiv("X-Forwarded-Host")
            || header.field.equiv("X-Forwarded-Proto")
            || header.field.equiv("X-Real-Ip")
    })
}

fn forwarded_client_chain_is_local(headers: &[Header]) -> bool {
    for header in headers {
        if header.field.equiv("X-Forwarded-For") {
            for value in header.value.as_str().split(',') {
                let Some(ip) = parse_forwarded_client_ip(value) else {
                    return false;
                };
                if !is_local_ip(ip) {
                    return false;
                }
            }
        } else if header.field.equiv("X-Real-Ip") {
            let Some(ip) = parse_forwarded_client_ip(header.value.as_str()) else {
                return false;
            };
            if !is_local_ip(ip) {
                return false;
            }
        } else if header.field.equiv("Forwarded") {
            for element in header.value.as_str().split(',') {
                for part in element.split(';') {
                    let Some((name, value)) = part.split_once('=') else {
                        continue;
                    };
                    if name.trim().eq_ignore_ascii_case("for") {
                        let Some(ip) = parse_forwarded_client_ip(value) else {
                            return false;
                        };
                        if !is_local_ip(ip) {
                            return false;
                        }
                    }
                }
            }
        }
    }
    true
}

fn parse_forwarded_client_ip(value: &str) -> Option<IpAddr> {
    let value = value.trim().trim_matches('"').trim();
    if value.is_empty() || value.eq_ignore_ascii_case("unknown") || value.starts_with('_') {
        return None;
    }
    if let Ok(ip) = value.parse::<IpAddr>() {
        return Some(ip);
    }
    if let Some(rest) = value.strip_prefix('[') {
        let (host, _) = rest.split_once(']')?;
        return host.parse::<IpAddr>().ok();
    }
    if value.matches(':').count() == 1 {
        let (host, _) = value.rsplit_once(':')?;
        return host.parse::<IpAddr>().ok();
    }
    None
}

fn build_release_readiness_response(
    public_origin: &str,
    state: Option<&StateResponse>,
    account: &AccountManagementSnapshot,
    feature_availability: &FeatureAvailabilityResponse,
    hardware: &HardwareReadinessResponse,
    harboros: &HarborOsStatusResponse,
    rag: &RagReadinessResponse,
    runtime: &LocalModelRuntimeProjection,
) -> ReleaseReadinessResponse {
    let mut groups = Vec::new();

    let interactive = find_feature_item(feature_availability, "interactive_reply");
    let proactive = find_feature_item(feature_availability, "proactive_delivery");
    let binding = find_feature_item(feature_availability, "binding_availability");
    groups.push(release_group(
        "im",
        "IM Gateway",
        "harbor-im-gateway",
        vec![
            release_item_from_feature(
                "weixin-setup",
                "Weixin setup",
                "harbor-im-gateway",
                proactive,
                "/im-gateway",
                vec![format!(
                    "manage_url={}",
                    to_non_empty_option(&account.gateway.manage_url)
                )],
            ),
            release_item_from_feature(
                "feishu-setup",
                "Feishu API key setup",
                "harbor-im-gateway",
                interactive,
                "/im-gateway",
                vec![format!(
                    "gateway_configured={}",
                    yes_no(account.gateway.bridge_provider.configured)
                )],
            ),
            release_item_from_feature(
                "binding-availability",
                "Binding availability",
                "harbor-im-gateway",
                binding,
                "/account-management",
                Vec::new(),
            ),
        ],
    ));

    let answer = find_feature_item(feature_availability, "retrieval.answer");
    let embed = find_feature_item(feature_availability, "retrieval.embed");
    let vision = find_feature_item(feature_availability, "retrieval.vision_summary");
    groups.push(release_group(
        "models",
        "Models & Policies",
        "harbor-framework",
        vec![
            release_item_from_feature(
                "model-answer",
                "LLM answer endpoint",
                "harbor-framework",
                answer,
                "/models-policies",
                vec![format!("runtime_ready={}", yes_no(runtime.ready))],
            ),
            release_item_from_feature(
                "model-embedding",
                "Embedding endpoint",
                "harbor-framework",
                embed,
                "/models-policies",
                vec![format!("backend_ready={}", yes_no(runtime.backend_ready))],
            ),
            release_item_from_feature(
                "model-vision",
                "Vision summary endpoint",
                "harbor-framework",
                vision,
                "/models-policies",
                Vec::new(),
            ),
        ],
    ));

    groups.push(release_group(
        "rag",
        "HarborOS Multimodal RAG",
        "harbor-framework",
        vec![release_item(
            "rag-readiness",
            "Multimodal RAG readiness",
            "harbor-framework",
            release_status_from_probe_status(&rag.status),
            &format!(
                "Index: {}; embedding: {}",
                rag.index_directory.status, rag.embedding_model.status
            ),
            "RAG readiness checks index directory, embedding model, media parser, and writable storage.",
            "GET /api/rag/readiness",
            "/models-policies",
            rag.evidence.clone(),
        )],
    ));

    groups.push(release_group(
        "hardware",
        "Hardware Readiness",
        "harbor-framework",
        vec![release_item(
            "hardware-profile",
            "CPU / GPU / NPU readiness",
            "harbor-framework",
            release_status_from_probe_status(&hardware.status),
            &hardware.recommended_model_profile,
            "Hardware probe for local inference placement.",
            "GET /api/hardware/readiness",
            "/models-policies",
            hardware.evidence.clone(),
        )],
    ));

    groups.push(release_group(
        "harboros",
        "HarborOS System Domain",
        "harbor-hos-control",
        vec![release_item(
            "harboros-status",
            "System Domain status",
            "harbor-hos-control",
            release_status_from_probe_status(&harboros.status),
            &format!("HarborOS WebUI: {}", harboros.webui_url),
            "HarborOS stays System Domain only; AIoT is not managed here.",
            "GET /api/harboros/status",
            "/harboros",
            harboros.evidence.clone(),
        )],
    ));

    let devices = state.map(|state| state.devices.as_slice()).unwrap_or(&[]);
    let selected_camera = state
        .and_then(|state| state.defaults.selected_camera_device_id.clone())
        .unwrap_or_default();
    let selected_device = devices
        .iter()
        .find(|device| device.device_id == selected_camera);
    let rtsp_ready = selected_device
        .map(|device| !device.primary_stream.url.trim().is_empty())
        .unwrap_or(false);
    let snapshot_ready = selected_device
        .map(|device| {
            device
                .snapshot_url
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
        })
        .unwrap_or(false);
    let default_camera_summary = if selected_camera.is_empty() {
        "No default camera selected.".to_string()
    } else {
        format!("Default camera: {selected_camera}")
    };
    groups.push(release_group(
        "aiot",
        "Devices & AIoT",
        "harbor-aiot",
        vec![
            release_item(
                "aiot-default-camera",
                "Default camera",
                "harbor-aiot",
                if selected_device.is_some() {
                    "ready"
                } else if devices.is_empty() {
                    "blocked"
                } else {
                    "needs-config"
                },
                &default_camera_summary,
                "Default camera is configured in Harbor Assistant Devices & AIoT.",
                "GET /api/state",
                "/devices-aiot",
                vec![format!("registered_devices={}", devices.len())],
            ),
            release_item(
                "aiot-rtsp-snapshot",
                "RTSP / snapshot readiness",
                "harbor-aiot",
                if rtsp_ready && snapshot_ready {
                    "ready"
                } else if rtsp_ready || snapshot_ready {
                    "needs-config"
                } else {
                    "blocked"
                },
                "Camera media capabilities are projected from the Home Device Domain registry.",
                "RTSP/snapshot checks are explicit actions and should only run after target confirmation.",
                "POST /api/devices/{device_id}/rtsp-check",
                "/devices-aiot",
                vec![
                    format!("rtsp_ready={}", yes_no(rtsp_ready)),
                    format!("snapshot_ready={}", yes_no(snapshot_ready)),
                ],
            ),
        ],
    ));

    let overall_status = rollup_group_status(groups.iter().map(|group| group.status.as_str()));
    let generated_at = now_unix_string();
    let checklist = groups
        .iter()
        .flat_map(|group| group.items.clone())
        .collect::<Vec<_>>();
    let status_cards = groups
        .iter()
        .map(|group| ReleaseReadinessStatusCard {
            id: format!("{}-status", group.group_id),
            label: group.label.clone(),
            value: group.status.clone(),
            status: group.status.clone(),
            detail: format!("{} readiness owned by {}.", group.label, group.owner_lane),
            endpoint: group
                .items
                .first()
                .map(|item| item.endpoint.clone())
                .unwrap_or_else(|| "GET /api/release/readiness".to_string()),
            deep_link: group
                .items
                .first()
                .map(|item| item.deep_link.clone())
                .unwrap_or_else(|| "/overview".to_string()),
        })
        .collect::<Vec<_>>();
    let deep_links = release_readiness_deep_links(public_origin, harboros);
    let blockers = checklist
        .iter()
        .filter(|item| item.status == "blocked")
        .filter_map(|item| {
            non_empty_string(&item.blocking_reason).or_else(|| Some(item.label.clone()))
        })
        .collect::<Vec<_>>();
    let warnings = checklist
        .iter()
        .filter(|item| item.status == "needs-config")
        .filter_map(|item| {
            non_empty_string(&item.blocking_reason).or_else(|| Some(item.label.clone()))
        })
        .collect::<Vec<_>>();
    ReleaseReadinessResponse {
        generated_at: generated_at.clone(),
        checked_at: generated_at,
        status: overall_status.clone(),
        summary: format!(
            "Release readiness is {overall_status}; Harbor Assistant stays on :4174 and HarborOS WebUI stays on /ui/ or 80/443."
        ),
        overall_status,
        harbor_desk: ReadinessSurfaceSummary {
            admin_origin: public_origin.to_string(),
            admin_port: public_origin_port(public_origin).unwrap_or(4174),
            harboros_webui: harboros_webui_url(public_origin),
            note: "Harbor Assistant uses port 4174; HarborOS WebUI stays on /ui/ or 80/443.".to_string(),
        },
        groups,
        checklist,
        status_cards,
        deep_links,
        blockers,
        warnings,
    }
}

fn release_readiness_deep_links(
    public_origin: &str,
    harboros: &HarborOsStatusResponse,
) -> Vec<ReleaseReadinessDeepLink> {
    let origin = public_origin.trim_end_matches('/');
    vec![
        ReleaseReadinessDeepLink {
            label: "Harbor Assistant Overview (:4174)".to_string(),
            href: format!("{origin}/overview"),
            detail: "Release readiness entry in Harbor Assistant.".to_string(),
            endpoint: "GET /api/release/readiness".to_string(),
        },
        ReleaseReadinessDeepLink {
            label: "Devices & AIoT".to_string(),
            href: format!("{origin}/devices-aiot"),
            detail: "Camera, RTSP, snapshot, share-link, and credential management.".to_string(),
            endpoint: "GET /api/devices/{device_id}/evidence".to_string(),
        },
        ReleaseReadinessDeepLink {
            label: "Models & Policies".to_string(),
            href: format!("{origin}/models-policies"),
            detail: "Model endpoints, local catalog, downloads, and RAG readiness.".to_string(),
            endpoint: "GET /api/models/local-catalog + GET /api/rag/readiness".to_string(),
        },
        ReleaseReadinessDeepLink {
            label: "HarborOS System Domain".to_string(),
            href: format!("{origin}/harboros"),
            detail: "System Domain status inside Harbor Assistant, not AIoT device control.".to_string(),
            endpoint: "GET /api/harboros/status".to_string(),
        },
        ReleaseReadinessDeepLink {
            label: "HarborOS WebUI (/ui/)".to_string(),
            href: harboros.webui_url.clone(),
            detail: "HarborOS WebUI stays on /ui/ or ports 80/443, separate from Harbor Assistant :4174."
                .to_string(),
            endpoint: "HarborOS WebUI /ui/ or 80/443".to_string(),
        },
    ]
}

fn release_group(
    group_id: &str,
    label: &str,
    owner_lane: &str,
    items: Vec<ReleaseReadinessItem>,
) -> ReleaseReadinessGroup {
    let status = rollup_group_status(items.iter().map(|item| item.status.as_str()));
    ReleaseReadinessGroup {
        group_id: group_id.to_string(),
        label: label.to_string(),
        owner_lane: owner_lane.to_string(),
        status,
        items,
    }
}

fn release_item_from_feature(
    item_id: &str,
    label: &str,
    owner_lane: &str,
    feature: Option<&FeatureAvailabilityItem>,
    action_path: &str,
    mut extra_evidence: Vec<String>,
) -> ReleaseReadinessItem {
    if let Some(feature) = feature {
        extra_evidence.extend(feature.evidence.clone());
        return release_item(
            item_id,
            label,
            owner_lane,
            release_status_from_feature_status(&feature.status),
            &feature.current_option,
            &feature.blocker,
            &feature.source_of_truth,
            action_path,
            extra_evidence,
        );
    }
    release_item(
        item_id,
        label,
        owner_lane,
        "blocked",
        "Feature projection missing.",
        "Readiness aggregator could not find the expected feature row.",
        "GET /api/feature-availability",
        action_path,
        extra_evidence,
    )
}

fn release_item(
    item_id: &str,
    label: &str,
    owner_lane: &str,
    status: &str,
    summary: &str,
    detail: &str,
    source_of_truth: &str,
    action_path: &str,
    evidence: Vec<String>,
) -> ReleaseReadinessItem {
    let summary = redact_admin_string(summary);
    let detail = redact_admin_string(detail);
    let source_of_truth = redact_admin_string(source_of_truth);
    let redacted_evidence = evidence
        .into_iter()
        .map(|item| redact_admin_string(&item))
        .collect::<Vec<_>>();
    let blocking_reason = if status == "ready" {
        String::new()
    } else {
        detail.clone()
    };
    ReleaseReadinessItem {
        id: item_id.to_string(),
        item_id: item_id.to_string(),
        label: label.to_string(),
        lane: owner_lane.to_string(),
        owner_lane: owner_lane.to_string(),
        status: status.to_string(),
        summary,
        detail,
        endpoint: source_of_truth.clone(),
        source_of_truth,
        deep_link: action_path.to_string(),
        next_action: action_path.to_string(),
        action_path: action_path.to_string(),
        last_verified_at: Some(now_unix_string()),
        blocking_reason: blocking_reason.clone(),
        blockers: if blocking_reason.is_empty() {
            Vec::new()
        } else {
            vec![blocking_reason.clone()]
        },
        evidence: redacted_evidence.clone(),
        evidence_records: vec![ReadinessEvidenceRecord {
            generated_at: now_unix_string(),
            lane: owner_lane.to_string(),
            status: status.to_string(),
            action_path: action_path.to_string(),
            blocking_reason,
            evidence: redacted_evidence,
        }],
    }
}

fn rollup_group_status<'a>(statuses: impl Iterator<Item = &'a str>) -> String {
    let mut best = "ready";
    for status in statuses {
        if readiness_status_rank(status) > readiness_status_rank(best) {
            best = status;
        }
    }
    best.to_string()
}

fn readiness_status_rank(status: &str) -> u8 {
    match status {
        "blocked" => 3,
        "needs-config" => 2,
        "ready" => 1,
        _ => 2,
    }
}

fn release_status_from_feature_status(status: &str) -> &'static str {
    match status {
        "available" => "ready",
        "degraded" | "not_configured" => "needs-config",
        "blocked" => "blocked",
        _ => "needs-config",
    }
}

fn release_status_from_probe_status(status: &str) -> &'static str {
    match status {
        "ready" | "available" => "ready",
        "blocked" => "blocked",
        _ => "needs-config",
    }
}

fn find_feature_item<'a>(
    response: &'a FeatureAvailabilityResponse,
    feature_id: &str,
) -> Option<&'a FeatureAvailabilityItem> {
    response
        .groups
        .iter()
        .flat_map(|group| group.items.iter())
        .find(|item| item.feature_id == feature_id)
}

fn build_hardware_readiness_response() -> HardwareReadinessResponse {
    let generated_at = now_unix_string();
    let cpu_count = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(1);
    let memory_mb = proc_mem_total_mb();
    let gpu_evidence = gpu_probe_evidence();
    let (gpu_vram_total_mb, gpu_vram_free_mb) = nvidia_smi_memory_mb();
    let npu_evidence = npu_probe_evidence();

    let cpu = HardwareComponentReadiness {
        status: if cpu_count >= 2 {
            "ready"
        } else {
            "needs-config"
        }
        .to_string(),
        summary: format!("{cpu_count} logical CPU threads"),
        detail: format!("{} / {}", env::consts::OS, env::consts::ARCH),
        evidence: vec![format!("available_parallelism={cpu_count}")],
    };
    let memory = HardwareComponentReadiness {
        status: memory_mb
            .map(|mb| if mb >= 8192 { "ready" } else { "needs-config" })
            .unwrap_or("needs-config")
            .to_string(),
        summary: memory_mb
            .map(|mb| format!("{mb} MiB memory detected"))
            .unwrap_or_else(|| "memory total not detected".to_string()),
        detail: "Memory is read from /proc/meminfo when available.".to_string(),
        evidence: memory_mb
            .map(|mb| vec![format!("mem_total_mb={mb}")])
            .unwrap_or_else(|| vec!["mem_total_mb=unknown".to_string()]),
    };
    let gpu_ready = gpu_evidence
        .iter()
        .any(|item| item.contains("present=true"));
    let gpu = HardwareComponentReadiness {
        status: if gpu_ready { "ready" } else { "needs-config" }.to_string(),
        summary: if gpu_ready {
            "GPU runtime detected".to_string()
        } else {
            "No GPU runtime detected".to_string()
        },
        detail: "Checks nvidia-smi, /dev/nvidia0, /dev/dri, and CUDA visibility.".to_string(),
        evidence: gpu_evidence,
    };
    let npu_ready = npu_evidence
        .iter()
        .any(|item| item.contains("present=true"));
    let npu = HardwareComponentReadiness {
        status: if npu_ready { "ready" } else { "needs-config" }.to_string(),
        summary: if npu_ready {
            "NPU/accelerator device detected".to_string()
        } else {
            "No NPU runtime detected".to_string()
        },
        detail: "Checks common Linux accelerator device nodes.".to_string(),
        evidence: npu_evidence,
    };
    let hardware_class =
        hardware_class_for_probe(cpu_count, memory_mb, gpu_ready, gpu_vram_total_mb);
    let recommended_model_profile = recommended_model_profile_for_class(&hardware_class);
    let mut blockers = Vec::new();
    if cpu_count < 2 {
        blockers.push("CPU parallelism is below the release recommendation.".to_string());
    }
    if memory_mb.unwrap_or_default() > 0 && memory_mb.unwrap_or_default() < 4096 {
        blockers.push("Memory is below the minimal local model recommendation.".to_string());
    }
    let status = if blockers.is_empty() {
        "ready"
    } else {
        "needs-config"
    }
    .to_string();
    let mut evidence = Vec::new();
    evidence.extend(cpu.evidence.clone());
    evidence.extend(memory.evidence.clone());
    evidence.extend(gpu.evidence.clone());
    if let Some(total) = gpu_vram_total_mb {
        evidence.push(format!("gpu_vram_total_mb={total}"));
    } else if gpu_ready {
        evidence.push("gpu_vram_total_mb=unknown".to_string());
    }
    if let Some(free) = gpu_vram_free_mb {
        evidence.push(format!("gpu_vram_free_mb={free}"));
    }
    evidence.extend(npu.evidence.clone());
    HardwareReadinessResponse {
        generated_at,
        status,
        cpu,
        memory,
        gpu,
        npu,
        memory_mb,
        gpu_vram_total_mb,
        gpu_vram_free_mb,
        hardware_class,
        recommended_model_profile,
        blockers,
        evidence,
    }
}

fn hardware_class_for_probe(
    cpu_count: usize,
    memory_mb: Option<u64>,
    gpu_ready: bool,
    gpu_vram_total_mb: Option<u64>,
) -> String {
    let memory_mb = memory_mb.unwrap_or_default();
    if let Some(vram) = gpu_vram_total_mb {
        if vram >= 48 * 1024 {
            return "multi_gpu_or_remote".to_string();
        }
        if vram >= 24 * 1024 {
            return "gpu_24gb_plus".to_string();
        }
        if vram >= 16 * 1024 {
            return "gpu_16gb".to_string();
        }
        return "low_vram_gpu".to_string();
    }
    if gpu_ready {
        return "low_vram_gpu".to_string();
    }
    if memory_mb >= 16 * 1024 && cpu_count >= 4 {
        return "cpu_small".to_string();
    }
    if memory_mb >= 8 * 1024 && cpu_count >= 2 {
        return "tiny_cpu".to_string();
    }
    "cloud_first".to_string()
}

fn recommended_model_profile_for_class(hardware_class: &str) -> String {
    match hardware_class {
        "multi_gpu_or_remote" | "gpu_24gb_plus" => "high-capacity-local-models",
        "gpu_16gb" => "local-4b-vlm-plus-llm",
        "low_vram_gpu" | "cpu_small" | "tiny_cpu" => "lightweight-local-models",
        _ => "cloud-or-tiny-local-models",
    }
    .to_string()
}

fn build_knowledge_index_job(
    root: &KnowledgeSourceRoot,
    requested_at: &str,
    resource_profile: RagResourceProfile,
) -> KnowledgeIndexJobRecord {
    KnowledgeIndexJobRecord {
        job_id: format!("knowledge-index-{}", Uuid::new_v4().simple()),
        source_root_id: root.root_id.clone(),
        source_root_label: root.label.clone(),
        source_root_path: root.path.clone(),
        modalities: vec![
            "document".to_string(),
            "image".to_string(),
            "audio".to_string(),
            "video".to_string(),
        ],
        status: "queued".to_string(),
        progress_percent: Some(0),
        requested_at: Some(requested_at.to_string()),
        started_at: None,
        completed_at: None,
        error_message: None,
        retry_count: 0,
        checkpoint: json!({
            "phase": "queued",
            "source_root_id": root.root_id.clone(),
        }),
        resource_profile,
        cancel_requested: false,
    }
}

fn queued_knowledge_root_status(root: &KnowledgeSourceRoot) -> KnowledgeIndexRootStatus {
    let mut status = knowledge_root_status(root, None);
    status.status = "queued".to_string();
    status.detail = "Index refresh has been queued as a background job.".to_string();
    status
}

fn spawn_knowledge_index_worker(
    store: AdminConsoleStore,
    settings: KnowledgeSettings,
    jobs: Vec<KnowledgeIndexJobRecord>,
) -> Result<(), String> {
    thread::Builder::new()
        .name("harborbeacon-knowledge-index".to_string())
        .spawn(move || run_knowledge_index_jobs(store, settings, jobs))
        .map(|_| ())
        .map_err(|error| format!("failed to spawn knowledge index worker: {error}"))
}

fn run_knowledge_index_jobs(
    store: AdminConsoleStore,
    settings: KnowledgeSettings,
    jobs: Vec<KnowledgeIndexJobRecord>,
) {
    let service = match KnowledgeIndexConfig::new(PathBuf::from(settings.index_root.clone()))
        .and_then(KnowledgeIndexService::from_config)
    {
        Ok(service) => service,
        Err(error) => {
            for job in jobs {
                fail_knowledge_index_job(&store, job, "index_root_unavailable", error.clone());
            }
            return;
        }
    };

    for job in jobs {
        run_knowledge_index_job(&store, &service, job);
    }
}

fn knowledge_embedding_warmup_timeout() -> Duration {
    env::var(KNOWLEDGE_EMBEDDING_WARMUP_TIMEOUT_MS_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|value| *value > 0)
        .map(Duration::from_millis)
        .unwrap_or_else(|| Duration::from_millis(DEFAULT_KNOWLEDGE_EMBEDDING_WARMUP_TIMEOUT_MS))
}

fn embedding_warmup_timeout_stats(
    total: usize,
    timeout: Duration,
) -> KnowledgeEmbeddingWarmupStats {
    KnowledgeEmbeddingWarmupStats {
        total,
        completed: 0,
        skipped: 0,
        failed: total,
        degraded: true,
        persist_error: None,
        last_error: Some(format!(
            "Embedding warmup exceeded {} ms; index completed with degraded embedding cache.",
            timeout.as_millis()
        )),
    }
}

fn run_knowledge_embedding_warmup_with_timeout(
    service: &KnowledgeIndexService,
    snapshot: &KnowledgeIndexSnapshot,
    model_center_state: &AdminModelCenterState,
) -> KnowledgeEmbeddingWarmupStats {
    let timeout = knowledge_embedding_warmup_timeout();
    let total = service.embedding_warmup_candidate_count(snapshot);
    let (sender, receiver) = sync_channel(1);
    let worker_service = service.clone();
    let worker_snapshot = snapshot.clone();
    let worker_model_center_state = model_center_state.clone();

    if let Err(error) = thread::Builder::new()
        .name("harborbeacon-knowledge-embedding-warmup".to_string())
        .spawn(move || {
            let stats =
                worker_service.warm_embedding_cache(&worker_snapshot, &worker_model_center_state);
            let _ = sender.try_send(stats);
        })
    {
        return KnowledgeEmbeddingWarmupStats {
            total,
            completed: 0,
            skipped: 0,
            failed: total,
            degraded: true,
            persist_error: None,
            last_error: Some(format!("failed to spawn embedding warmup worker: {error}")),
        };
    }

    match receiver.recv_timeout(timeout) {
        Ok(stats) => stats,
        Err(RecvTimeoutError::Timeout) => embedding_warmup_timeout_stats(total, timeout),
        Err(RecvTimeoutError::Disconnected) => KnowledgeEmbeddingWarmupStats {
            total,
            completed: 0,
            skipped: 0,
            failed: total,
            degraded: true,
            persist_error: None,
            last_error: Some("Embedding warmup worker exited without returning stats.".to_string()),
        },
    }
}

fn run_knowledge_index_job(
    store: &AdminConsoleStore,
    service: &KnowledgeIndexService,
    mut job: KnowledgeIndexJobRecord,
) {
    if knowledge_index_job_cancel_requested(store, &job.job_id) {
        cancel_knowledge_index_job(store, job, "canceled_before_start");
        return;
    }

    job.status = "running".to_string();
    job.started_at = job.started_at.or_else(|| Some(now_unix_string()));
    job.progress_percent = Some(10);
    job.checkpoint = json!({
        "phase": "load_or_refresh",
        "source_root_id": job.source_root_id.clone(),
    });
    if let Err(error) = store.save_knowledge_index_job(job.clone()) {
        fail_knowledge_index_job(store, job, "job_state_write_failed", error);
        return;
    }

    let root_path = PathBuf::from(job.source_root_path.trim());
    if !root_path.exists() {
        fail_knowledge_index_job(
            store,
            job,
            "source_root_missing",
            format!("knowledge source root not found: {}", root_path.display()),
        );
        return;
    }
    if knowledge_index_job_cancel_requested(store, &job.job_id) {
        cancel_knowledge_index_job(store, job, "canceled_before_refresh");
        return;
    }

    match service.load_or_refresh(&root_path) {
        Ok(snapshot) => {
            if knowledge_index_job_cancel_requested(store, &job.job_id) {
                cancel_knowledge_index_job(store, job, "canceled_after_refresh");
                return;
            }
            job.progress_percent = Some(85);
            job.checkpoint = json!({
                "phase": "embedding_warmup",
                "entry_count": snapshot.manifest.entries.len(),
                "manifest_path": snapshot.manifest_path.to_string_lossy(),
            });
            if let Err(error) = store.save_knowledge_index_job(job.clone()) {
                fail_knowledge_index_job(store, job, "job_state_write_failed", error);
                return;
            }
            let model_center_state = load_model_center_state();
            let embedding_warmup = run_knowledge_embedding_warmup_with_timeout(
                service,
                &snapshot,
                &model_center_state,
            );
            if knowledge_index_job_cancel_requested(store, &job.job_id) {
                cancel_knowledge_index_job(store, job, "canceled_after_embedding_warmup");
                return;
            }
            let indexed_at = snapshot.manifest.generated_at.clone();
            let _ = mark_knowledge_source_root_indexed(
                store,
                &job.source_root_id,
                &job.source_root_path,
                indexed_at,
            );
            job.status = "completed".to_string();
            job.progress_percent = Some(100);
            job.completed_at = Some(now_unix_string());
            job.error_message = None;
            job.checkpoint = json!({
                "phase": "completed",
                "entry_count": snapshot.manifest.entries.len(),
                "manifest_path": snapshot.manifest_path.to_string_lossy(),
                "embedding_total": embedding_warmup.total,
                "embedding_completed": embedding_warmup.completed,
                "embedding_skipped": embedding_warmup.skipped,
                "embedding_failed": embedding_warmup.failed,
                "embedding_warmup_degraded": embedding_warmup.degraded,
                "embedding_warmup_timeout_ms": knowledge_embedding_warmup_timeout().as_millis()
                    as u64,
                "embedding_persist_error": embedding_warmup.persist_error,
                "embedding_last_error": embedding_warmup.last_error,
            });
            let _ = store.save_knowledge_index_job(job);
        }
        Err(error) => fail_knowledge_index_job(store, job, "load_or_refresh_failed", error),
    }
}

fn fail_knowledge_index_job(
    store: &AdminConsoleStore,
    mut job: KnowledgeIndexJobRecord,
    phase: &str,
    error: String,
) {
    job.status = "failed".to_string();
    job.progress_percent = Some(100);
    job.completed_at = Some(now_unix_string());
    job.error_message = Some(error);
    job.checkpoint = json!({"phase": phase});
    let _ = store.save_knowledge_index_job(job);
}

fn cancel_knowledge_index_job(
    store: &AdminConsoleStore,
    mut job: KnowledgeIndexJobRecord,
    phase: &str,
) {
    job.status = "canceled".to_string();
    job.cancel_requested = true;
    job.progress_percent = job.progress_percent.or(Some(0));
    job.completed_at = Some(now_unix_string());
    job.checkpoint = json!({"phase": phase});
    let _ = store.save_knowledge_index_job(job);
}

fn knowledge_index_job_cancel_requested(store: &AdminConsoleStore, job_id: &str) -> bool {
    store
        .list_knowledge_index_jobs()
        .map(|jobs| {
            jobs.into_iter().any(|job| {
                job.job_id == job_id && (job.cancel_requested || job.status.as_str() == "canceled")
            })
        })
        .unwrap_or(false)
}

fn mark_knowledge_source_root_indexed(
    store: &AdminConsoleStore,
    source_root_id: &str,
    source_root_path: &str,
    indexed_at: String,
) -> Result<(), String> {
    let mut settings = store.knowledge_settings()?;
    let Some(root) = settings
        .source_roots
        .iter_mut()
        .find(|root| root.root_id == source_root_id && root.path.trim() == source_root_path.trim())
    else {
        return Ok(());
    };
    root.last_indexed_at = Some(indexed_at);
    store.save_knowledge_settings(settings).map(|_| ())
}

fn build_knowledge_index_status_response(
    settings: KnowledgeSettings,
) -> KnowledgeIndexStatusResponse {
    let index_path = Path::new(&settings.index_root);
    let index_root_exists = index_path.exists();
    let index_root_writable = path_can_accept_write(index_path);
    let storage_summary = knowledge_index_storage_summary(index_path);
    let source_roots = settings
        .source_roots
        .iter()
        .map(|root| knowledge_root_status(root, None))
        .collect::<Vec<_>>();
    let mut blockers = Vec::new();
    if settings.enabled_source_root_paths().is_empty() {
        blockers.push("No enabled knowledge source roots are configured.".to_string());
    }
    if !index_root_writable {
        blockers.push("Knowledge index root is not writable or its parent is missing.".to_string());
    }
    for root in &source_roots {
        if root.enabled && !root.exists {
            blockers.push(format!("Knowledge source root not found: {}", root.path));
        }
    }
    if let Err(error) = validate_knowledge_settings(settings.clone()) {
        blockers.push(error);
    }
    let status = if blockers.is_empty() {
        "ready"
    } else if index_root_writable || source_roots.iter().any(|root| root.enabled && root.exists) {
        "needs-config"
    } else {
        "blocked"
    }
    .to_string();
    KnowledgeIndexStatusResponse {
        generated_at: now_unix_string(),
        status,
        settings,
        index_root_exists,
        index_root_writable,
        manifest_count: storage_summary.manifest_count,
        manifest_entry_count: storage_summary.manifest_entry_count,
        document_count: storage_summary.document_count,
        image_count: storage_summary.image_count,
        audio_count: storage_summary.audio_count,
        video_count: storage_summary.video_count,
        content_indexed_image_count: storage_summary.content_indexed_image_count,
        vlm_indexed_image_count: storage_summary.vlm_indexed_image_count,
        ocr_indexed_image_count: storage_summary.ocr_indexed_image_count,
        image_content_missing_count: storage_summary.image_content_missing_count,
        image_text_source_counts: storage_summary.image_text_source_counts,
        embedding_cache_count: storage_summary.embedding_cache_count,
        embedding_entry_count: storage_summary.embedding_entry_count,
        storage_usage_bytes: storage_summary.storage_usage_bytes,
        last_indexed_at: storage_summary.last_indexed_at,
        source_roots,
        blockers,
    }
}

fn knowledge_index_storage_summary(index_path: &Path) -> KnowledgeIndexStorageSummary {
    let mut summary = KnowledgeIndexStorageSummary {
        storage_usage_bytes: directory_storage_bytes(index_path),
        ..KnowledgeIndexStorageSummary::default()
    };
    let Ok(entries) = fs::read_dir(index_path) else {
        return summary;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().to_string();
        if file_name.ends_with(".embeddings.json") {
            summary.embedding_cache_count += 1;
            if let Ok(store) = load_embedding_store(&path) {
                summary.embedding_entry_count += store.entries.len();
            }
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_str::<KnowledgeIndexManifest>(&text) else {
            continue;
        };
        summary.manifest_count += 1;
        summary.manifest_entry_count += manifest.entries.len();
        for indexed_entry in &manifest.entries {
            match indexed_entry.modality {
                KnowledgeModality::Document => summary.document_count += 1,
                KnowledgeModality::Image => {
                    summary.image_count += 1;
                    let mut has_vlm = false;
                    let mut has_ocr = false;
                    for source in &indexed_entry.text_sources {
                        let source_kind = source.source_kind.trim().to_ascii_lowercase();
                        if source_kind.is_empty() {
                            continue;
                        }
                        *summary
                            .image_text_source_counts
                            .entry(source_kind.clone())
                            .or_insert(0) += 1;
                        has_vlm |= source_kind == "vlm";
                        has_ocr |= source_kind == "ocr";
                    }
                    if has_vlm {
                        summary.vlm_indexed_image_count += 1;
                    }
                    if has_ocr {
                        summary.ocr_indexed_image_count += 1;
                    }
                    if has_vlm || has_ocr {
                        summary.content_indexed_image_count += 1;
                    } else {
                        summary.image_content_missing_count += 1;
                    }
                }
                KnowledgeModality::Audio => summary.audio_count += 1,
                KnowledgeModality::Video => summary.video_count += 1,
            }
        }
        summary.last_indexed_at =
            max_unix_timestamp_string(summary.last_indexed_at.take(), Some(manifest.generated_at));
    }
    summary
}

fn resolve_admin_search_source_scope(
    payload: &KnowledgeSearchApiRequest,
    settings: &KnowledgeSettings,
    dvr_settings: Option<&DvrRecordingSettings>,
) -> Result<Vec<String>, String> {
    let scope = payload
        .source_scope
        .as_deref()
        .and_then(non_empty_string)
        .map(|value| value.to_ascii_lowercase());
    let Some(scope) = scope else {
        return Ok(Vec::new());
    };
    if scope == "all" {
        return Ok(Vec::new());
    }
    let dvr_root = dvr_settings
        .map(|settings| {
            media_library_root_path(settings)
                .to_string_lossy()
                .into_owned()
        })
        .and_then(|path| non_empty_string(&path));
    match scope.as_str() {
        "dvr_library" => dvr_root
            .map(|path| vec![path])
            .ok_or_else(|| "DVR media library source scope requires DVR settings.".to_string()),
        "nas_files" => {
            let dvr_root = dvr_root.unwrap_or_default();
            let roots = settings
                .source_roots
                .iter()
                .filter(|root| root.enabled)
                .filter_map(|root| non_empty_string(&root.path))
                .filter(|root_path| {
                    dvr_root.is_empty()
                        || (!path_is_same_or_inside(root_path, &dvr_root)
                            && !path_is_same_or_inside(&dvr_root, root_path))
                })
                .collect::<Vec<_>>();
            if roots.is_empty() {
                Err("No NAS source roots are configured outside the DVR media library.".to_string())
            } else {
                Ok(roots)
            }
        }
        _ => Err(format!(
            "Unsupported Harbor Assistant Search source_scope {}; expected dvr_library, nas_files, or all.",
            scope
        )),
    }
}

fn build_admin_knowledge_search_request(
    payload: KnowledgeSearchApiRequest,
    settings: &KnowledgeSettings,
    focus_paths: Vec<String>,
    scoped_roots: Vec<String>,
) -> Result<KnowledgeSearchRequest, String> {
    let query = payload.query.trim();
    if query.is_empty() {
        return Err("Harbor Assistant Search search requires a non-empty query.".to_string());
    }
    let configured_roots = settings.enabled_source_root_paths();
    if configured_roots.is_empty() {
        return Err("No enabled knowledge source roots are configured.".to_string());
    }
    let mut request = KnowledgeSearchRequest::new(query.to_string());
    request.configured_roots = configured_roots.clone();
    request.roots = if scoped_roots.is_empty() {
        configured_roots
    } else {
        scoped_roots
    };
    request.index_root = non_empty_string(&settings.index_root);
    request.include_documents = payload.include_documents.unwrap_or(true);
    request.include_images = payload.include_images.unwrap_or(true);
    request.include_videos = payload.include_videos.unwrap_or(true);
    request.limit = payload.limit.unwrap_or(24).clamp(1, 50);
    request.privacy_level = settings.privacy_level;
    request.resource_profile = settings.default_resource_profile;
    request.retrieval = settings.retrieval.clone();
    request.require_embeddings = false;
    request.latency_budget_ms = None;
    request.focus_paths = focus_paths;
    Ok(request)
}

fn parse_optional_unix_seconds(value: Option<&str>, field: &str) -> Result<Option<u64>, String> {
    match value.map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => value
            .parse::<u64>()
            .map(Some)
            .map_err(|_| format!("DVR search {field} must be Unix seconds.")),
        None => Ok(None),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct KnowledgePreviewError {
    status: StatusCode,
    message: String,
}

impl KnowledgePreviewError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

fn resolve_knowledge_preview_path(
    requested_path: &str,
    settings: &KnowledgeSettings,
) -> Result<PathBuf, KnowledgePreviewError> {
    let requested = PathBuf::from(requested_path.trim());
    if !requested.is_absolute() {
        return Err(KnowledgePreviewError::new(
            StatusCode(400),
            "knowledge preview path must be absolute",
        ));
    }
    let requested = requested.canonicalize().map_err(|_| {
        KnowledgePreviewError::new(
            StatusCode(404),
            format!("knowledge preview file not found: {}", requested.display()),
        )
    })?;
    if requested.is_dir() {
        return Err(KnowledgePreviewError::new(
            StatusCode(400),
            "knowledge preview does not support directory paths",
        ));
    }
    if !requested.is_file() {
        return Err(KnowledgePreviewError::new(
            StatusCode(404),
            format!("knowledge preview file not found: {}", requested.display()),
        ));
    }
    if !knowledge_preview_mime_supported(&requested) {
        return Err(KnowledgePreviewError::new(
            StatusCode(415),
            format!(
                "knowledge preview supports images, videos, text, and Markdown only: {}",
                requested.display()
            ),
        ));
    }
    let allowed_roots = enabled_existing_knowledge_roots(settings);
    if allowed_roots.is_empty() {
        return Err(KnowledgePreviewError::new(
            StatusCode(422),
            "No enabled knowledge source roots are configured.",
        ));
    }
    if !allowed_roots
        .iter()
        .any(|root| path_is_same_or_inside(&requested.to_string_lossy(), &root.to_string_lossy()))
    {
        return Err(KnowledgePreviewError::new(
            StatusCode(403),
            format!(
                "knowledge preview path is outside configured source roots: {}",
                requested.display()
            ),
        ));
    }
    if !knowledge_preview_path_is_indexed(&requested, settings, &allowed_roots) {
        return Err(KnowledgePreviewError::new(
            StatusCode(404),
            format!(
                "knowledge preview path is not present in the current index: {}",
                requested.display()
            ),
        ));
    }
    Ok(requested)
}

fn enabled_existing_knowledge_roots(settings: &KnowledgeSettings) -> Vec<PathBuf> {
    settings
        .source_roots
        .iter()
        .filter(|root| root.enabled)
        .filter_map(|root| non_empty_string(&root.path))
        .filter_map(|path| PathBuf::from(path).canonicalize().ok())
        .filter(|path| path.is_dir())
        .collect()
}

fn knowledge_preview_path_is_indexed(
    requested: &Path,
    settings: &KnowledgeSettings,
    allowed_roots: &[PathBuf],
) -> bool {
    let service = match KnowledgeIndexConfig::new(PathBuf::from(settings.index_root.clone()))
        .and_then(KnowledgeIndexService::from_config)
    {
        Ok(service) => service,
        Err(_) => return false,
    };
    let requested_string = requested.to_string_lossy();
    allowed_roots
        .iter()
        .filter(|root| path_is_same_or_inside(&requested_string, &root.to_string_lossy()))
        .filter_map(|root| service.load_existing(root).ok())
        .flat_map(|snapshot| snapshot.manifest.entries.into_iter())
        .any(|entry| indexed_entry_matches_path(&entry.path, requested))
}

fn indexed_entry_matches_path(indexed_path: &str, requested: &Path) -> bool {
    let indexed = PathBuf::from(indexed_path);
    let indexed = indexed.canonicalize().unwrap_or(indexed);
    path_is_same_or_inside(&indexed.to_string_lossy(), &requested.to_string_lossy())
        && path_is_same_or_inside(&requested.to_string_lossy(), &indexed.to_string_lossy())
}

fn knowledge_preview_mime_supported(path: &Path) -> bool {
    let mime = mime_type_for_path(path);
    mime.starts_with("image/")
        || mime.starts_with("video/")
        || mime.starts_with("text/plain")
        || mime.starts_with("text/markdown")
}

fn directory_storage_bytes(path: &Path) -> u64 {
    let Ok(metadata) = fs::metadata(path) else {
        return 0;
    };
    if metadata.is_file() {
        return metadata.len();
    }
    let Ok(entries) = fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| directory_storage_bytes(&entry.path()))
        .sum()
}

fn max_unix_timestamp_string(left: Option<String>, right: Option<String>) -> Option<String> {
    match (left, right) {
        (Some(left), Some(right)) => {
            let left_value = left.parse::<u64>().unwrap_or_default();
            let right_value = right.parse::<u64>().unwrap_or_default();
            if right_value > left_value {
                Some(right)
            } else {
                Some(left)
            }
        }
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

fn knowledge_root_status(
    root: &KnowledgeSourceRoot,
    error: Option<String>,
) -> KnowledgeIndexRootStatus {
    let exists = Path::new(root.path.trim()).exists();
    let status = if !root.enabled {
        "disabled"
    } else if error.is_some() {
        "blocked"
    } else if exists && root.last_indexed_at.is_some() {
        "ready"
    } else if exists {
        "needs-index"
    } else {
        "missing"
    }
    .to_string();
    let detail = error.unwrap_or_else(|| {
        if !root.enabled {
            "Source root is disabled.".to_string()
        } else if exists {
            "Source root exists and can be indexed through HarborBeacon.".to_string()
        } else {
            "Source root path does not exist on this host.".to_string()
        }
    });
    KnowledgeIndexRootStatus {
        root_id: root.root_id.clone(),
        label: root.label.clone(),
        path: root.path.clone(),
        enabled: root.enabled,
        exists,
        last_indexed_at: root.last_indexed_at.clone(),
        status,
        detail,
    }
}

fn build_files_browse_response(
    requested_path: Option<&str>,
    settings: &KnowledgeSettings,
) -> Result<FilesBrowseResponse, String> {
    let allowed_roots = knowledge_browse_allowed_roots(settings);
    let requested = requested_path
        .map(PathBuf::from)
        .or_else(|| allowed_roots.first().map(PathBuf::from))
        .ok_or_else(|| "No HarborOS file browse roots are available.".to_string())?;
    if !allowed_roots
        .iter()
        .any(|root| path_is_same_or_inside(&requested.to_string_lossy(), root))
    {
        return Err(format!(
            "requested path is not inside an allowed HarborOS file browse root: {}",
            requested.display()
        ));
    }
    let path = requested.canonicalize().unwrap_or(requested);
    if !path.is_dir() {
        return Err(format!(
            "requested path is not a directory: {}",
            path.display()
        ));
    }
    let mut entries = fs::read_dir(&path)
        .map_err(|error| format!("failed to list directory {}: {error}", path.display()))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            let is_dir = metadata.is_dir();
            let name = entry.file_name().to_string_lossy().into_owned();
            Some(FileBrowseEntry {
                name,
                path: entry.path().to_string_lossy().into_owned(),
                is_dir,
                size_bytes: (!is_dir).then_some(metadata.len()),
            })
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
        right
            .is_dir
            .cmp(&left.is_dir)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });
    Ok(FilesBrowseResponse {
        parent: path
            .parent()
            .map(|parent| parent.to_string_lossy().into_owned()),
        path: path.to_string_lossy().into_owned(),
        readonly: true,
        allowed_roots,
        entries,
    })
}

fn knowledge_browse_allowed_roots(settings: &KnowledgeSettings) -> Vec<String> {
    let mut roots = Vec::new();
    roots.push(harboros_writable_root());
    if Path::new("/mnt").exists() {
        roots.push("/mnt".to_string());
    }
    roots.extend(settings.source_roots.iter().map(|root| root.path.clone()));
    if let Some(parent) = Path::new(&settings.index_root).parent() {
        roots.push(parent.to_string_lossy().into_owned());
    }
    let mut seen = Vec::<String>::new();
    roots
        .into_iter()
        .filter_map(|root| non_empty_string(&root))
        .filter(|root| {
            let exists = seen.iter().any(|seen_root| {
                path_is_same_or_inside(root, seen_root) && path_is_same_or_inside(seen_root, root)
            });
            if exists {
                false
            } else {
                seen.push(root.clone());
                true
            }
        })
        .collect()
}

fn build_rag_model_readiness(model_endpoints: &[ModelEndpoint]) -> Vec<RagModelReadinessCard> {
    [
        (
            ModelKind::Ocr,
            "OCR",
            "Image and scanned document text extraction",
        ),
        (
            ModelKind::Vlm,
            "VLM",
            "Image/keyframe caption and visual summary",
        ),
        (
            ModelKind::Embedder,
            "Embedder",
            "Dense retrieval embeddings",
        ),
        (ModelKind::Llm, "LLM", "Answer synthesis over cited context"),
        (ModelKind::Asr, "ASR", "Audio/video transcript extraction"),
    ]
    .into_iter()
    .map(|(kind, label, detail)| rag_model_readiness_card(model_endpoints, kind, label, detail))
    .collect()
}

fn rag_model_readiness_card(
    model_endpoints: &[ModelEndpoint],
    kind: ModelKind,
    label: &str,
    detail: &str,
) -> RagModelReadinessCard {
    let endpoint = model_endpoints
        .iter()
        .find(|endpoint| {
            endpoint.model_kind == kind && endpoint.status == ModelEndpointStatus::Active
        })
        .or_else(|| {
            model_endpoints
                .iter()
                .find(|endpoint| endpoint.model_kind == kind)
        });
    let status = endpoint
        .map(|endpoint| {
            if endpoint.status == ModelEndpointStatus::Active {
                "ready"
            } else {
                "needs-config"
            }
        })
        .unwrap_or("needs-config")
        .to_string();
    let blocker = match endpoint {
        Some(endpoint) if endpoint.status == ModelEndpointStatus::Active => None,
        Some(endpoint) => Some(format!(
            "{} endpoint {} is {}.",
            label,
            endpoint.model_endpoint_id,
            endpoint.status.as_str()
        )),
        None => Some(format!("{label} endpoint is not configured.")),
    };
    RagModelReadinessCard {
        model_kind: kind.as_str().to_string(),
        label: label.to_string(),
        status,
        endpoint_id: endpoint.map(|endpoint| endpoint.model_endpoint_id.clone()),
        endpoint_kind: endpoint.map(|endpoint| endpoint.endpoint_kind.as_str().to_string()),
        provider_key: endpoint.map(|endpoint| endpoint.provider_key.clone()),
        model_name: endpoint.map(|endpoint| endpoint.model_name.clone()),
        detail: detail.to_string(),
        blocker,
    }
}

fn build_rag_privacy_policy_component(
    knowledge: &KnowledgeSettings,
    model_endpoints: &[ModelEndpoint],
) -> RagReadinessComponent {
    let cloud_endpoint_ready = has_active_endpoint_kind(model_endpoints, ModelEndpointKind::Cloud);
    let privacy = privacy_level_as_str(knowledge.privacy_level);
    let status = match knowledge.privacy_level {
        PrivacyLevel::StrictLocal => "ready",
        PrivacyLevel::AllowRedactedCloud => {
            if cloud_endpoint_ready {
                "needs-config"
            } else {
                "blocked"
            }
        }
        PrivacyLevel::AllowCloud => {
            if cloud_endpoint_ready {
                "needs-config"
            } else {
                "blocked"
            }
        }
    }
    .to_string();
    RagReadinessComponent {
        status,
        summary: format!("Privacy policy: {privacy}"),
        detail: match knowledge.privacy_level {
            PrivacyLevel::StrictLocal => {
                "Cloud execution is blocked by default; RAG must stay local or return degraded status.".to_string()
            }
            PrivacyLevel::AllowRedactedCloud => {
                "Cloud execution is allowed only after redaction and audit records are present.".to_string()
            }
            PrivacyLevel::AllowCloud => {
                "Cloud execution is permitted by workspace policy and still requires audit evidence.".to_string()
            }
        },
        evidence: vec![
            format!("privacy_level={privacy}"),
            format!("cloud_endpoint_ready={cloud_endpoint_ready}"),
        ],
    }
}

fn build_rag_privacy_gateway_readiness(
    knowledge: &KnowledgeSettings,
    audit_records: &[AuditRecord],
) -> RagPrivacyGatewayReadiness {
    let recent = audit_records
        .iter()
        .rev()
        .filter(|record| record.action == PRIVACY_GATEWAY_AUDIT_ACTION)
        .take(PRIVACY_GATEWAY_READINESS_AUDIT_LIMIT)
        .collect::<Vec<_>>();
    let mut decision_counts = BTreeMap::new();
    let mut risk_counts = BTreeMap::new();
    let mut warning_count = 0usize;
    let latest = recent.first().copied();

    for record in &recent {
        let decision = record
            .result_snapshot
            .get("decision")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let risk = record
            .result_snapshot
            .get("risk_level")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        *decision_counts.entry(decision).or_insert(0) += 1;
        *risk_counts.entry(risk).or_insert(0) += 1;
        warning_count += record
            .result_snapshot
            .get("warnings")
            .and_then(Value::as_array)
            .map(Vec::len)
            .unwrap_or_default();
    }

    let recent_audit_count = recent.len();
    let status =
        if knowledge.privacy_level == PrivacyLevel::StrictLocal || recent_audit_count > 0 {
            "ready"
        } else {
            "needs-evidence"
        }
        .to_string();
    let privacy_level = privacy_level_as_str(knowledge.privacy_level).to_string();
    let default_resource_profile = knowledge.default_resource_profile.as_str().to_string();
    let latest_privacy_transform_id = latest
        .and_then(|record| {
            record
                .result_snapshot
                .get("privacy_transform_id")
                .and_then(Value::as_str)
                .or_else(|| {
                    (!record.entity_id.trim().is_empty()).then_some(record.entity_id.as_str())
                })
        })
        .map(ToString::to_string);
    let latest_evaluated_at = latest.and_then(|record| record.created_at.clone());

    RagPrivacyGatewayReadiness {
        status,
        summary: if recent_audit_count > 0 {
            format!("{recent_audit_count} recent Privacy Gateway audit record(s) found.")
        } else if knowledge.privacy_level == PrivacyLevel::StrictLocal {
            "Privacy Gateway is installed; strict_local blocks cloud execution.".to_string()
        } else {
            "Privacy Gateway is installed, but no recent RAG cloud-evaluation audit evidence exists yet."
                .to_string()
        },
        policy_version: PRIVACY_GATEWAY_POLICY_VERSION.to_string(),
        covered_routes: vec!["retrieval.answer".to_string()],
        privacy_level,
        default_resource_profile,
        recent_audit_count,
        decision_counts,
        risk_counts,
        latest_privacy_transform_id,
        latest_evaluated_at,
        warning_count,
        metadata_only: true,
        secret_scan: "clean".to_string(),
        evidence: vec![
            format!("policy_version={PRIVACY_GATEWAY_POLICY_VERSION}"),
            "covered_route=retrieval.answer".to_string(),
            format!("recent_audit_count={recent_audit_count}"),
            format!("audit_action={PRIVACY_GATEWAY_AUDIT_ACTION}"),
            "metadata_only=true".to_string(),
        ],
    }
}

fn build_rag_resource_profiles(
    knowledge: &KnowledgeSettings,
    model_endpoints: &[ModelEndpoint],
    storage_writable: bool,
    embedding_ready: bool,
) -> Vec<RagResourceProfileStatus> {
    [
        RagResourceProfile::CpuOnly,
        RagResourceProfile::LocalGpu,
        RagResourceProfile::SidecarGpu,
        RagResourceProfile::CloudAllowed,
    ]
    .into_iter()
    .map(|profile| {
        rag_resource_profile_status(
            profile,
            knowledge,
            model_endpoints,
            storage_writable,
            embedding_ready,
        )
    })
    .collect()
}

fn rag_resource_profile_status(
    profile: RagResourceProfile,
    knowledge: &KnowledgeSettings,
    model_endpoints: &[ModelEndpoint],
    storage_writable: bool,
    embedding_ready: bool,
) -> RagResourceProfileStatus {
    let mut blockers = Vec::new();
    let mut warnings = Vec::new();
    if !storage_writable {
        blockers.push("RAG index storage is not writable.".to_string());
    }
    if !embedding_ready {
        blockers.push("Embedding runtime is not ready.".to_string());
    }
    match profile {
        RagResourceProfile::CpuOnly => {
            warnings.push("Audio/video ingestion may be slow on CPU-only hosts.".to_string());
        }
        RagResourceProfile::LocalGpu => {
            if !local_gpu_detected() {
                blockers.push("No local GPU was detected by the readiness probe.".to_string());
            }
        }
        RagResourceProfile::SidecarGpu => {
            if !has_active_endpoint_kind(model_endpoints, ModelEndpointKind::Sidecar) {
                blockers.push("No active sidecar model endpoint is configured.".to_string());
            }
        }
        RagResourceProfile::CloudAllowed => {
            if knowledge.privacy_level == PrivacyLevel::StrictLocal {
                blockers.push("privacy_level strict_local blocks cloud execution.".to_string());
            }
            if !has_active_endpoint_kind(model_endpoints, ModelEndpointKind::Cloud) {
                blockers.push("No active cloud model endpoint is configured.".to_string());
            }
            warnings.push(
                "Cloud use requires redaction and audit evidence before execution.".to_string(),
            );
        }
    }
    let status = if blockers.is_empty() {
        "ready"
    } else if storage_writable || embedding_ready {
        "needs-config"
    } else {
        "blocked"
    };
    RagResourceProfileStatus {
        profile: profile.as_str().to_string(),
        label: match profile {
            RagResourceProfile::CpuOnly => "CPU only",
            RagResourceProfile::LocalGpu => "Local GPU",
            RagResourceProfile::SidecarGpu => "Sidecar GPU",
            RagResourceProfile::CloudAllowed => "Cloud allowed",
        }
        .to_string(),
        status: status.to_string(),
        detail: if profile == knowledge.default_resource_profile {
            "Default RAG resource profile.".to_string()
        } else {
            "Optional RAG resource profile.".to_string()
        },
        blockers,
        warnings,
    }
}

fn build_rag_capability_profiles(
    storage_summary: &KnowledgeIndexStorageSummary,
    model_readiness: &[RagModelReadinessCard],
    storage_writable: bool,
    embedding_ready: bool,
    media_parser_ready: bool,
    default_profile: RagResourceProfile,
) -> Vec<RagCapabilityReadinessCard> {
    let mut text_blockers = Vec::new();
    if !storage_writable {
        text_blockers.push("RAG index storage is not writable.".to_string());
    }
    if !embedding_ready {
        text_blockers.push("Embedding runtime is not ready.".to_string());
    }
    let text_status = if text_blockers.is_empty() {
        "ready"
    } else if storage_summary.document_count > 0 {
        "degraded"
    } else {
        "blocked"
    };

    let vlm_ready = rag_model_ready(model_readiness, ModelKind::Vlm);
    let ocr_ready = rag_model_ready(model_readiness, ModelKind::Ocr);
    let mut image_blockers = Vec::new();
    let mut image_warnings = Vec::new();
    if !vlm_ready {
        image_blockers
            .push("VLM endpoint is required for content-level natural photo indexing.".to_string());
    }
    if !storage_writable {
        image_blockers.push("RAG index storage is not writable.".to_string());
    }
    if !embedding_ready {
        image_warnings.push(
            "Embedding runtime is not ready; image retrieval can only use content text lexical matching.".to_string(),
        );
    }
    if !ocr_ready {
        image_warnings
            .push("OCR endpoint is not ready; OCR can only be used as a supplement.".to_string());
    }
    if !media_parser_ready {
        image_warnings.push("No local media parser binary was detected.".to_string());
    }
    if storage_summary.image_count == 0 {
        image_warnings.push("No indexed image entries were found yet.".to_string());
    } else if storage_summary.vlm_indexed_image_count == 0 {
        image_warnings.push(
            "Indexed images do not yet include VLM-derived content; do not treat photo RAG as ready.".to_string(),
        );
    }
    if storage_summary.image_content_missing_count > 0 {
        image_warnings.push(format!(
            "{} indexed image(s) are missing VLM/OCR content text.",
            storage_summary.image_content_missing_count
        ));
    }
    if default_profile == RagResourceProfile::CpuOnly {
        image_warnings.push(
            "CPU-only VLM indexing may be slow, but it must still produce image-derived content."
                .to_string(),
        );
    }
    let image_status = if !image_blockers.is_empty() {
        "blocked"
    } else if embedding_ready && storage_summary.vlm_indexed_image_count > 0 {
        "ready"
    } else {
        "degraded"
    };

    vec![
        RagCapabilityReadinessCard {
            capability_id: "text_rag".to_string(),
            label: "Text RAG".to_string(),
            status: text_status.to_string(),
            summary: format!(
                "{} document(s), {} embedding cache entrie(s)",
                storage_summary.document_count, storage_summary.embedding_entry_count
            ),
            blockers: text_blockers,
            warnings: Vec::new(),
            evidence: vec![
                format!("document_count={}", storage_summary.document_count),
                format!("embedding_ready={embedding_ready}"),
                format!("storage_writable={storage_writable}"),
            ],
        },
        RagCapabilityReadinessCard {
            capability_id: "image_rag".to_string(),
            label: "Image RAG".to_string(),
            status: image_status.to_string(),
            summary: format!(
                "{} image(s), {} content-indexed, {} with VLM, {} with OCR",
                storage_summary.image_count,
                storage_summary.content_indexed_image_count,
                storage_summary.vlm_indexed_image_count,
                storage_summary.ocr_indexed_image_count
            ),
            blockers: image_blockers,
            warnings: image_warnings,
            evidence: vec![
                format!("image_count={}", storage_summary.image_count),
                format!(
                    "content_indexed_image_count={}",
                    storage_summary.content_indexed_image_count
                ),
                format!(
                    "vlm_indexed_image_count={}",
                    storage_summary.vlm_indexed_image_count
                ),
                format!(
                    "ocr_indexed_image_count={}",
                    storage_summary.ocr_indexed_image_count
                ),
                format!(
                    "image_content_missing_count={}",
                    storage_summary.image_content_missing_count
                ),
                format!("vlm_endpoint_ready={vlm_ready}"),
                format!("ocr_endpoint_ready={ocr_ready}"),
                format!("embedding_ready={embedding_ready}"),
                format!("storage_writable={storage_writable}"),
            ],
        },
    ]
}

fn rag_model_ready(model_readiness: &[RagModelReadinessCard], kind: ModelKind) -> bool {
    model_readiness
        .iter()
        .any(|card| card.model_kind == kind.as_str() && card.status == "ready")
}

fn has_active_endpoint_kind(
    model_endpoints: &[ModelEndpoint],
    endpoint_kind: ModelEndpointKind,
) -> bool {
    model_endpoints.iter().any(|endpoint| {
        endpoint.endpoint_kind == endpoint_kind && endpoint.status == ModelEndpointStatus::Active
    })
}

fn local_gpu_detected() -> bool {
    env::var("CUDA_VISIBLE_DEVICES")
        .ok()
        .map(|value| !value.trim().is_empty() && value.trim() != "-1")
        .unwrap_or(false)
        || command_available("nvidia-smi")
        || Path::new("/dev/nvidia0").exists()
        || Path::new("/dev/dri").exists()
}

fn privacy_level_as_str(level: PrivacyLevel) -> &'static str {
    match level {
        PrivacyLevel::StrictLocal => "strict_local",
        PrivacyLevel::AllowRedactedCloud => "allow_redacted_cloud",
        PrivacyLevel::AllowCloud => "allow_cloud",
    }
}

fn recent_knowledge_index_jobs(
    index_jobs: &[KnowledgeIndexJobRecord],
) -> Vec<KnowledgeIndexJobRecord> {
    let mut jobs = index_jobs.to_vec();
    jobs.sort_by(|left, right| {
        right
            .requested_at
            .cmp(&left.requested_at)
            .then_with(|| right.job_id.cmp(&left.job_id))
    });
    jobs.truncate(8);
    jobs
}

fn build_rag_readiness_response(
    runtime: &LocalModelRuntimeProjection,
    knowledge: &KnowledgeSettings,
    model_endpoints: &[ModelEndpoint],
    index_jobs: &[KnowledgeIndexJobRecord],
    audit_records: &[AuditRecord],
) -> RagReadinessResponse {
    let model_endpoints = overlay_model_endpoints_with_runtime_truth(model_endpoints, runtime);
    let generated_at = now_unix_string();
    let index_dir = knowledge.index_root.clone();
    let index_path = Path::new(&index_dir);
    let index_exists = index_path.exists();
    let index_parent_exists = index_path.parent().map(Path::exists).unwrap_or(false);
    let storage_writable = path_can_accept_write(index_path);
    let storage_summary = knowledge_index_storage_summary(index_path);
    let embedding_ready = runtime.ready
        && runtime.backend_ready
        && runtime
            .embedding_model
            .as_ref()
            .is_some_and(|model| !model.trim().is_empty());
    let ffmpeg_ready = command_available("ffmpeg");
    let tesseract_ready = command_available("tesseract");
    let media_parser_ready = ffmpeg_ready || tesseract_ready;
    let enabled_source_roots = knowledge
        .source_roots
        .iter()
        .filter(|root| root.enabled)
        .collect::<Vec<_>>();
    let existing_enabled_source_roots = enabled_source_roots
        .iter()
        .filter(|root| Path::new(root.path.trim()).exists())
        .count();
    let model_readiness = build_rag_model_readiness(&model_endpoints);
    let privacy_policy = build_rag_privacy_policy_component(knowledge, &model_endpoints);
    let privacy_gateway = build_rag_privacy_gateway_readiness(knowledge, audit_records);

    let source_roots = RagReadinessComponent {
        status: if existing_enabled_source_roots > 0 {
            "ready"
        } else if enabled_source_roots.is_empty() {
            "needs-config"
        } else {
            "blocked"
        }
        .to_string(),
        summary: format!(
            "{} enabled source root(s), {} existing",
            enabled_source_roots.len(),
            existing_enabled_source_roots
        ),
        detail: "Knowledge source roots are configured in Harbor Assistant and are the only roots eligible for search or benchmark.".to_string(),
        evidence: knowledge
            .source_roots
            .iter()
            .map(|root| {
                format!(
                    "root_id={} enabled={} exists={} path={}",
                    root.root_id,
                    root.enabled,
                    Path::new(root.path.trim()).exists(),
                    root.path
                )
            })
            .collect(),
    };

    let index_directory = RagReadinessComponent {
        status: if index_exists || index_parent_exists {
            "ready"
        } else {
            "needs-config"
        }
        .to_string(),
        summary: if index_exists {
            format!("Index directory exists: {index_dir}")
        } else {
            format!("Index directory not created yet: {index_dir}")
        },
        detail:
            "RAG index path is persisted in knowledge.index_root and is managed from Harbor Assistant."
                .to_string(),
        evidence: vec![
            format!("index_dir={index_dir}"),
            format!("index_exists={index_exists}"),
            format!("index_parent_exists={index_parent_exists}"),
        ],
    };
    let embedding_model = RagReadinessComponent {
        status: if embedding_ready {
            "ready"
        } else {
            "needs-config"
        }
        .to_string(),
        summary: runtime
            .embedding_model
            .clone()
            .unwrap_or_else(|| "embedding model not detected".to_string()),
        detail: "Embedding readiness is read from harbor-model-api /healthz runtime projection."
            .to_string(),
        evidence: vec![
            format!("runtime_ready={}", runtime.ready),
            format!("backend_ready={}", runtime.backend_ready),
            format!(
                "embedding_model={}",
                runtime
                    .embedding_model
                    .clone()
                    .unwrap_or_else(|| "none".to_string())
            ),
        ],
    };
    let media_parser = RagReadinessComponent {
        status: if media_parser_ready {
            "ready"
        } else {
            "needs-config"
        }
        .to_string(),
        summary: if media_parser_ready {
            "At least one media parser is available".to_string()
        } else {
            "No media parser binary detected".to_string()
        },
        detail: "Multimodal RAG can use ffmpeg for audio/video and tesseract for OCR when present."
            .to_string(),
        evidence: vec![
            format!("ffmpeg_present={ffmpeg_ready}"),
            format!("tesseract_present={tesseract_ready}"),
        ],
    };
    let storage = RagReadinessComponent {
        status: if storage_writable { "ready" } else { "needs-config" }.to_string(),
        summary: if storage_writable {
            "RAG storage path appears writable".to_string()
        } else {
            "RAG storage path is not writable or parent is missing".to_string()
        },
        detail: "Readiness uses non-mutating filesystem metadata checks; it does not create the index automatically.".to_string(),
        evidence: vec![format!("storage_writable={storage_writable}")],
    };
    let resource_profiles = build_rag_resource_profiles(
        knowledge,
        &model_endpoints,
        storage_writable,
        embedding_ready,
    );
    let capability_profiles = build_rag_capability_profiles(
        &storage_summary,
        &model_readiness,
        storage_writable,
        embedding_ready,
        media_parser_ready,
        knowledge.default_resource_profile,
    );

    let mut blockers = Vec::new();
    if !embedding_ready {
        blockers.push("Embedding model is not ready.".to_string());
    }
    if existing_enabled_source_roots == 0 {
        blockers.push("No enabled knowledge source root exists on this host.".to_string());
    }
    if !storage_writable {
        blockers.push("RAG index storage is not writable.".to_string());
    }
    if !media_parser_ready {
        blockers.push("No multimodal media parser was detected.".to_string());
    }
    for model in &model_readiness {
        if model.status != "ready" {
            blockers.push(model.blocker.clone().unwrap_or_else(|| {
                format!("{} model readiness is {}.", model.label, model.status)
            }));
        }
    }
    let mut warnings = Vec::new();
    if knowledge.privacy_level != PrivacyLevel::StrictLocal {
        warnings.push(
            "Cloud-capable privacy policy is configured; redaction and audit are required before cloud execution.".to_string(),
        );
    }
    for capability in &capability_profiles {
        if capability.capability_id == "image_rag" && capability.status == "blocked" {
            blockers.push(format!(
                "Image RAG is blocked: {}",
                capability.blockers.join("; ")
            ));
        } else if capability.capability_id == "image_rag" && capability.status == "degraded" {
            warnings.push(format!("Image RAG is degraded: {}", capability.summary));
        }
    }
    if knowledge.default_resource_profile == RagResourceProfile::CloudAllowed
        && knowledge.privacy_level == PrivacyLevel::StrictLocal
    {
        blockers.push(
            "Default resource profile is cloud_allowed but privacy policy is strict_local."
                .to_string(),
        );
    }
    for profile in &resource_profiles {
        if profile.profile == knowledge.default_resource_profile.as_str()
            && profile.status == "blocked"
        {
            blockers.push(format!(
                "Default resource profile {} is blocked: {}",
                profile.profile,
                profile.blockers.join("; ")
            ));
        }
    }
    let status = if blockers.is_empty() {
        "ready"
    } else if embedding_ready
        || storage_writable
        || media_parser_ready
        || existing_enabled_source_roots > 0
    {
        "needs-config"
    } else {
        "blocked"
    }
    .to_string();
    let mut evidence = Vec::new();
    evidence.extend(source_roots.evidence.clone());
    evidence.extend(index_directory.evidence.clone());
    evidence.extend(embedding_model.evidence.clone());
    evidence.extend(privacy_policy.evidence.clone());
    evidence.extend(media_parser.evidence.clone());
    evidence.extend(storage.evidence.clone());
    evidence.push(format!(
        "default_resource_profile={}",
        knowledge.default_resource_profile.as_str()
    ));

    RagReadinessResponse {
        generated_at,
        status,
        summary: if blockers.is_empty() {
            "Multimodal RAG admin skeleton is configured.".to_string()
        } else {
            format!(
                "{} blocker(s) require admin action before RAG is ready.",
                blockers.len()
            )
        },
        source_roots,
        index_directory,
        embedding_model,
        model_readiness,
        resource_profiles,
        capability_profiles,
        privacy_policy,
        privacy_gateway,
        media_parser,
        storage_writable: storage,
        index_jobs: recent_knowledge_index_jobs(index_jobs),
        blockers,
        warnings,
        evidence,
    }
}

fn path_can_accept_write(path: &Path) -> bool {
    let candidate = if path.exists() {
        path
    } else {
        match path.parent() {
            Some(parent) => parent,
            None => path,
        }
    };
    fs::metadata(candidate)
        .map(|metadata| !metadata.permissions().readonly())
        .unwrap_or(false)
}

fn proc_mem_total_mb() -> Option<u64> {
    let text = fs::read_to_string("/proc/meminfo").ok()?;
    let line = text.lines().find(|line| line.starts_with("MemTotal:"))?;
    let kb = line
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u64>().ok())?;
    Some(kb / 1024)
}

fn proc_meminfo_mb() -> Value {
    let text = match fs::read_to_string("/proc/meminfo") {
        Ok(text) => text,
        Err(error) => {
            return json!({
                "available": false,
                "error": format!("meminfo unavailable: {error}"),
            })
        }
    };
    let lookup_mb = |key: &str| -> Option<u64> {
        let prefix = format!("{key}:");
        let line = text.lines().find(|line| line.starts_with(&prefix))?;
        let kb = line
            .split_whitespace()
            .nth(1)
            .and_then(|value| value.parse::<u64>().ok())?;
        Some(kb / 1024)
    };
    let total = lookup_mb("MemTotal");
    let available = lookup_mb("MemAvailable");
    let pressure = match (total, available) {
        (Some(total), Some(available)) => Some(total.saturating_sub(available)),
        _ => None,
    };
    json!({
        "available": total.is_some() || available.is_some(),
        "totalMiB": total,
        "availableMiB": available,
        "memoryPressureMiB": pressure,
        "direct16EnvelopeMiB": 16 * 1024,
        "plus24EnvelopeMiB": 24 * 1024,
        "direct16Passed": pressure.map(|value| value <= 16 * 1024),
        "plus24Passed": pressure.map(|value| value <= 24 * 1024),
    })
}

fn build_inference_health_alias_response(
    runtime: &LocalModelRuntimeProjection,
) -> InferenceHealthAliasResponse {
    let status = if runtime.ready {
        "ok"
    } else if runtime.error.is_some() {
        "degraded"
    } else {
        "not_ready"
    };
    InferenceHealthAliasResponse {
        status: status.to_string(),
        ready: runtime.ready,
        service: "harborbeacon-local-inference".to_string(),
        backend_kind: runtime.backend_kind.clone(),
        backend: json!({
            "ready": runtime.backend_ready,
            "kind": runtime.backend_kind.clone(),
        }),
        chat_model: runtime.chat_model.clone(),
        embedding_model: runtime.embedding_model.clone(),
        note: runtime.note.clone(),
        error: runtime
            .error
            .as_ref()
            .map(|error| redact_admin_string(error)),
    }
}

fn build_latest_home_assistant_task_api_workflow(task_service: &TaskApiService) -> Value {
    let events = match task_service.conversation_store().recent_events(200) {
        Ok(events) => events,
        Err(error) => {
            return json!({
                "status": "unavailable",
                "message": redact_admin_string(&error),
                "redacted": true,
            })
        }
    };
    let summaries = events
        .iter()
        .filter_map(redacted_home_assistant_task_api_event_summary)
        .collect::<Vec<_>>();
    let Some(latest_event) = summaries.last().cloned() else {
        return json!({
            "status": "not_run",
            "message": "No Home Assistant Task API workflow evidence has been recorded.",
            "redacted": true,
        });
    };
    let recent_events = summaries
        .iter()
        .rev()
        .take(6)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();
    json!({
        "status": "available",
        "redacted": true,
        "latest_event": latest_event,
        "recent_events": recent_events,
    })
}

fn build_latest_family_memory_task_api_workflow(task_service: &TaskApiService) -> Value {
    let events = match task_service.conversation_store().recent_events(200) {
        Ok(events) => events,
        Err(error) => {
            return json!({
                "status": "unavailable",
                "message": redact_admin_string(&error),
                "redacted": true,
            })
        }
    };
    let summaries = events
        .iter()
        .filter_map(redacted_family_memory_task_api_event_summary)
        .collect::<Vec<_>>();
    let Some(latest_event) = summaries.last().cloned() else {
        return json!({
            "status": "not_run",
            "message": "No Family Memory Task API workflow evidence has been recorded.",
            "redacted": true,
        });
    };
    let recent_events = summaries
        .iter()
        .rev()
        .take(6)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();
    json!({
        "status": "available",
        "redacted": true,
        "latest_event": latest_event,
        "recent_events": recent_events,
    })
}

fn build_latest_general_message_nsp_route_workflow(task_service: &TaskApiService) -> Value {
    let steps = match task_service.conversation_store().recent_task_steps(200) {
        Ok(steps) => steps,
        Err(error) => {
            return json!({
                "status": "unavailable",
                "message": redact_admin_string(&error),
                "redacted": true,
            })
        }
    };
    let summaries = steps
        .iter()
        .filter_map(redacted_general_message_nsp_route_summary)
        .collect::<Vec<_>>();
    let Some(latest_step) = summaries.last().cloned() else {
        return json!({
            "status": "not_run",
            "message": "No general.message NSP route evidence has been recorded.",
            "redacted": true,
        });
    };
    let recent_steps = summaries
        .iter()
        .rev()
        .take(6)
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();
    json!({
        "status": "available",
        "redacted": true,
        "latest_route": latest_step,
        "recent_routes": recent_steps,
    })
}

fn redacted_general_message_nsp_route_summary(step: &TaskStepRun) -> Option<Value> {
    if step.domain != "general" || step.operation != "message" {
        return None;
    }
    let nsp_route = step
        .output_payload
        .pointer("/data/reply_pack/nsp_route")
        .or_else(|| {
            step.output_payload
                .pointer("/data/general_message_controller/nsp_route")
        })?;
    let stage = nsp_route
        .pointer("/stage")
        .or_else(|| {
            step.output_payload
                .pointer("/data/general_message_controller/controller_stage")
        })
        .cloned()
        .unwrap_or(Value::Null);
    Some(json!({
        "step_id": step.step_id,
        "task_id": step.task_id,
        "status": step.status,
        "stage": stage,
        "decision": nsp_route.pointer("/decision").cloned().unwrap_or(Value::Null),
        "confidence": nsp_route.pointer("/confidence").cloned().unwrap_or(Value::Null),
        "schema_valid": nsp_route.pointer("/schema_valid").cloned().unwrap_or(Value::Null),
        "local_only": nsp_route.pointer("/local_only").cloned().unwrap_or(Value::Null),
        "fallback_reason": nsp_route
            .pointer("/fallback_reason")
            .and_then(Value::as_str)
            .map(redact_admin_string)
            .map(Value::String)
            .unwrap_or(Value::Null),
        "redacted": true,
    }))
}

fn redacted_home_assistant_task_api_event_summary(event: &EventRecord) -> Option<Value> {
    if !event
        .event_type
        .starts_with("home_assistant.service_action")
    {
        return None;
    }
    let payload = &event.payload;
    Some(json!({
        "event_type": event.event_type,
        "status": json_string_at_paths(payload, &["/status", "/outcome"]),
        "domain": json_string_at_paths(payload, &["/domain", "/entity/domain", "/request/domain"]),
        "service": json_string_at_paths(payload, &["/service", "/entity/service", "/request/service"]),
        "entity_id": json_string_at_paths(payload, &["/entity_id", "/entity/entity_id", "/request/entity_id"]),
        "candidate_count": json_usize_at_paths(payload, &["/candidate_count"]),
        "reason": json_string_at_paths(payload, &["/reason"]),
        "occurred_at": event.occurred_at,
        "redacted": true,
    }))
}

fn redacted_family_memory_task_api_event_summary(event: &EventRecord) -> Option<Value> {
    if !event.event_type.starts_with("family_memory.") {
        return None;
    }
    let payload = &event.payload;
    Some(json!({
        "event_type": event.event_type,
        "status": json_string_at_paths(payload, &["/status"]),
        "event_id": json_string_at_paths(payload, &["/event_id"]),
        "camera_id": json_string_at_paths(payload, &["/camera_id"]),
        "action": json_string_at_paths(payload, &["/feedback/action"]),
        "favorite_count": json_usize_at_paths(payload, &["/memory_overlay/favorite_count", "/favorite_count"]),
        "returned_count": json_usize_at_paths(payload, &["/returned_count"]),
        "metadata_only": true,
        "secret_scan": "clean",
        "occurred_at": event.occurred_at,
        "redacted": true,
    }))
}

fn json_string_at_paths(value: &Value, paths: &[&str]) -> Option<String> {
    paths.iter().find_map(|path| {
        value
            .pointer(path)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string)
    })
}

fn json_usize_at_paths(value: &Value, paths: &[&str]) -> Option<usize> {
    paths.iter().find_map(|path| {
        let value = value.pointer(path)?;
        value
            .as_u64()
            .and_then(|number| usize::try_from(number).ok())
            .or_else(|| {
                value
                    .as_str()
                    .and_then(|text| text.trim().parse::<usize>().ok())
            })
    })
}

fn latest_home_guardian_activity_summaries(mut summaries: Vec<Value>) -> Vec<Value> {
    summaries.sort_by(|left, right| {
        json_string_at_paths(right, &["/created_at", "/evaluated_at"]).cmp(&json_string_at_paths(
            left,
            &["/created_at", "/evaluated_at"],
        ))
    });
    summaries.truncate(HOME_GUARDIAN_ACTIVITY_LIMIT);
    summaries.reverse();
    summaries
}

fn current_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn current_process_thread_count() -> Option<u64> {
    let text = fs::read_to_string("/proc/self/status").ok()?;
    text.lines()
        .find_map(|line| line.strip_prefix("Threads:"))
        .and_then(|value| value.trim().parse::<u64>().ok())
}

fn admin_state_file_fingerprint(path: &Path) -> (u64, Option<u128>) {
    let Ok(metadata) = fs::metadata(path) else {
        return (0, None);
    };
    let modified_epoch_ms = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis());
    (metadata.len(), modified_epoch_ms)
}

fn home_guardian_action_cooldown_key(
    review: &AutomationRuleReview,
    stored: &StoredLocalVisionEvent,
    action_key: &str,
) -> String {
    let rule_key = review
        .rule_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(&review.review_id);
    format!("{}:{}:{}", rule_key, stored.event.camera_id, action_key)
}

fn home_guardian_auto_evaluation_cooldown_key(stored: &StoredLocalVisionEvent) -> String {
    format!("{}:{}", stored.event.camera_id, stored.event.event_type)
}

fn build_redacted_diagnostics_bundle(
    state: &StateResponse,
    home_assistant: &HomeAssistantStatusResponse,
    runtime: &LocalModelRuntimeProjection,
    last_event_notification_attempt: Option<Value>,
    last_vlm_enrichment: Option<Value>,
    last_home_assistant_service_action: Option<Value>,
    last_family_timeline_digest: Option<Value>,
    last_family_memory_feedback: Option<Value>,
    last_guardian_rule_proposal: Option<Value>,
    last_guardian_rule_evaluation: Option<Value>,
    guardian_queue: Value,
    runtime_backpressure: Value,
    admin_state_stats: Value,
    last_general_message_nsp_route: Value,
    last_home_assistant_task_api_workflow: Value,
    last_family_memory_task_api_workflow: Value,
    evt_readiness: Value,
    last_evt_preflight: Option<Value>,
    notification_targets: &[NotificationTargetRecord],
    gateway_status: Option<&Value>,
) -> RedactedDiagnosticsBundleResponse {
    let state_value = serde_json::to_value(state).unwrap_or_else(|_| json!({}));
    let devices = state_value
        .get("devices")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let selected_camera_device_id = state_value
        .get("defaults")
        .and_then(|value| value.get("selected_camera_device_id"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let latest_events = list_recent_local_vision_events_default(5).ok();
    let latest_event_count = latest_events.as_ref().map(Vec::len).unwrap_or(0);
    let event_store_stats = local_vision_event_store_stats_default()
        .map(|stats| serde_json::to_value(stats).unwrap_or_else(|_| json!({})))
        .unwrap_or_else(|error| {
            json!({
                "status": "unavailable",
                "error": redact_admin_string(&error),
                "metadata_only": true,
            })
        });
    let latest_event = latest_events
        .as_ref()
        .and_then(|events| events.first())
        .map(|stored| {
            json!({
                "event_id": stored.event.event_id.clone(),
                "camera_id": stored.event.camera_id.clone(),
                "event_type": stored.event.event_type.clone(),
                "vlm_status": stored.event.vlm.as_ref().map(|vlm| vlm.status.clone()),
                "latency_ms": stored.event.latency_ms,
            })
        });
    RedactedDiagnosticsBundleResponse {
        generated_at: now_unix_string(),
        host: json!({
            "role": "harbornavi-k3",
            "os_userland": "bianbu",
            "k3_local_webui": true,
        }),
        services: [
            "harboros-beacon.service",
            "harborlink-dev-k3.service",
            "harboros-im-gate.service",
            "nginx.service",
            "mosquitto.service",
        ]
        .iter()
        .map(|service| systemd_service_summary(service))
        .collect(),
        memory: proc_meminfo_mb(),
        cameras: json!({
            "count": devices.len(),
            "selected_camera_device_id": selected_camera_device_id,
            "configured_ids": devices
                .iter()
                .filter_map(|device| device.get("device_id").and_then(Value::as_str))
                .collect::<Vec<_>>(),
            "rtsp_urls_redacted": true,
            "credential_values_redacted": true,
        }),
        events: json!({
            "latest_count": latest_event_count,
            "latest_event": latest_event,
            "store": event_store_stats,
            "metadata_only": true,
        }),
        workflow: json!({
            "event_notification": last_event_notification_attempt.unwrap_or_else(|| json!({
                "status": "not_run",
                "message": "No event notification has been attempted in this API process.",
            })),
            "vlm_enrichment": home_guardian_optional_workflow(
                last_vlm_enrichment,
                "No VLM enrichment has been attempted in this API process.",
            ),
            "home_assistant_service_action": last_home_assistant_service_action.unwrap_or_else(|| json!({
                "status": "not_run",
                "message": "No Home Assistant service action has been attempted in this API process.",
            })),
            "family_timeline_digest": home_guardian_optional_workflow(
                last_family_timeline_digest,
                "No Family Timeline digest has been generated in this API process.",
            ),
            "family_memory_feedback": home_guardian_optional_workflow(
                last_family_memory_feedback,
                "No Family Memory feedback has been recorded in this API process.",
            ),
            "guardian_rule_proposal": home_guardian_optional_workflow(
                last_guardian_rule_proposal,
                "No Home Guardian rule proposal has been recorded in this API process.",
            ),
            "guardian_rule_evaluation": home_guardian_optional_workflow(
                last_guardian_rule_evaluation,
                "No Home Guardian rule evaluation has been recorded in this API process.",
            ),
            "guardian_queue": guardian_queue,
            "runtime_backpressure": runtime_backpressure,
            "admin_state": admin_state_stats,
            "general_message_nsp_route": last_general_message_nsp_route,
            "home_assistant_task_api_workflow": last_home_assistant_task_api_workflow,
            "family_memory_task_api_workflow": last_family_memory_task_api_workflow,
            "evt_readiness": evt_readiness_workflow_summary(&evt_readiness),
            "evt_preflight": evt_preflight_workflow_summary(last_evt_preflight),
            "default_notification_target": build_default_notification_target_readiness(
                notification_targets,
                gateway_status,
            ),
            "gateway_delivery_observability": build_redacted_gateway_delivery_observability(
                gateway_status,
            ),
        }),
        home_assistant: HomeAssistantStatusResponse {
            configured: home_assistant.configured,
            enabled: home_assistant.enabled,
            base_url: home_assistant.base_url.clone(),
            token_configured: home_assistant.token_configured,
            token_redacted: true,
            exposed_domains: home_assistant.exposed_domains.clone(),
            status: home_assistant.status.clone(),
            last_error: home_assistant
                .last_error
                .as_ref()
                .map(|error| redact_admin_string(error)),
            last_test_at: home_assistant.last_test_at.clone(),
            last_sync_at: home_assistant.last_sync_at.clone(),
            entity_count: home_assistant.entity_count,
            service_count: home_assistant.service_count,
            version: home_assistant.version.clone(),
            location_name: home_assistant.location_name.clone(),
        },
        models: json!({
            "inference": build_inference_health_alias_response(runtime),
            "api_keys_redacted": true,
            "model_paths_redacted": true,
        }),
        security: json!({
            "metadata_only": true,
            "secret_scan": "clean",
            "excludes": [
                "rtsp_url",
                "ha_token",
                "api_key",
                "private_key",
                "camera_credential",
                "local_snapshot_path",
                "image_bytes"
            ],
        }),
        audit_record: json!({
            "audit_kind": "operator.redacted_diagnostics_bundle_generated",
            "metadata_only": true,
            "secret_scan": "clean",
            "created_at": now_unix_string(),
        }),
    }
}

fn build_evt_diagnostics_workflow(
    evt_readiness: &Value,
    last_evt_preflight: Option<Value>,
    last_event_notification_attempt: Option<Value>,
    last_vlm_enrichment: Option<Value>,
    last_home_assistant_service_action: Option<Value>,
    last_family_timeline_digest: Option<Value>,
    last_family_memory_feedback: Option<Value>,
    last_guardian_rule_proposal: Option<Value>,
    last_guardian_rule_evaluation: Option<Value>,
    guardian_queue: Value,
    runtime_backpressure: Value,
    admin_state_stats: Value,
    last_general_message_nsp_route: Value,
    last_home_assistant_task_api_workflow: Value,
    last_family_memory_task_api_workflow: Value,
) -> Value {
    json!({
        "event_notification": last_event_notification_attempt.unwrap_or_else(|| json!({
            "status": "not_run",
            "message": "No event notification has been attempted in this API process.",
        })),
        "vlm_enrichment": home_guardian_optional_workflow(
            last_vlm_enrichment,
            "No VLM enrichment has been attempted in this API process.",
        ),
        "home_assistant_service_action": last_home_assistant_service_action.unwrap_or_else(|| json!({
            "status": "not_run",
            "message": "No Home Assistant service action has been attempted in this API process.",
        })),
        "family_timeline_digest": home_guardian_optional_workflow(
            last_family_timeline_digest,
            "No Family Timeline digest has been generated in this API process.",
        ),
        "family_memory_feedback": home_guardian_optional_workflow(
            last_family_memory_feedback,
            "No Family Memory feedback has been recorded in this API process.",
        ),
        "guardian_rule_proposal": home_guardian_optional_workflow(
            last_guardian_rule_proposal,
            "No Home Guardian rule proposal has been recorded in this API process.",
        ),
        "guardian_rule_evaluation": home_guardian_optional_workflow(
            last_guardian_rule_evaluation,
            "No Home Guardian rule evaluation has been recorded in this API process.",
        ),
        "guardian_queue": guardian_queue,
        "runtime_backpressure": runtime_backpressure,
        "admin_state": admin_state_stats,
        "general_message_nsp_route": last_general_message_nsp_route,
        "home_assistant_task_api_workflow": last_home_assistant_task_api_workflow,
        "family_memory_task_api_workflow": last_family_memory_task_api_workflow,
        "evt_readiness": evt_readiness_workflow_summary(evt_readiness),
        "evt_preflight": evt_preflight_workflow_summary(last_evt_preflight),
        "redacted": true,
    })
}

fn home_guardian_optional_workflow(value: Option<Value>, message: &str) -> Value {
    value.unwrap_or_else(|| {
        json!({
            "status": "not_run",
            "message": message,
            "metadata_only": true,
            "secret_scan": "clean",
        })
    })
}

fn systemd_service_summary(service: &str) -> Value {
    let output = Command::new("systemctl")
        .args(["is-active", service])
        .output();
    match output {
        Ok(output) => {
            let status = String::from_utf8_lossy(&output.stdout).trim().to_string();
            json!({
                "service": service,
                "status": if status.is_empty() { "unknown" } else { status.as_str() },
                "active": output.status.success(),
            })
        }
        Err(error) => json!({
            "service": service,
            "status": "unavailable",
            "active": false,
            "error": redact_admin_string(&error.to_string()),
        }),
    }
}

fn nvidia_smi_memory_mb() -> (Option<u64>, Option<u64>) {
    let output = Command::new("nvidia-smi")
        .args([
            "--query-gpu=memory.total,memory.free",
            "--format=csv,noheader,nounits",
        ])
        .output();
    let Ok(output) = output else {
        return (None, None);
    };
    if !output.status.success() {
        return (None, None);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut total = 0_u64;
    let mut free = 0_u64;
    let mut any = false;
    for line in text.lines() {
        let mut parts = line.split(',').map(|part| part.trim());
        let Some(total_part) = parts.next() else {
            continue;
        };
        let Some(free_part) = parts.next() else {
            continue;
        };
        let Ok(total_mb) = total_part.parse::<u64>() else {
            continue;
        };
        let Ok(free_mb) = free_part.parse::<u64>() else {
            continue;
        };
        total = total.saturating_add(total_mb);
        free = free.saturating_add(free_mb);
        any = true;
    }
    if any {
        (Some(total), Some(free))
    } else {
        (None, None)
    }
}

fn gpu_probe_evidence() -> Vec<String> {
    let mut evidence = Vec::new();
    let cuda_visible = env::var("CUDA_VISIBLE_DEVICES")
        .ok()
        .map(|value| !value.trim().is_empty() && value.trim() != "-1")
        .unwrap_or(false);
    evidence.push(format!("cuda_visible_devices_present={cuda_visible}"));
    evidence.push(format!(
        "nvidia_smi_present={}",
        command_available("nvidia-smi")
    ));
    evidence.push(format!(
        "dev_nvidia0_present={}",
        Path::new("/dev/nvidia0").exists()
    ));
    evidence.push(format!(
        "dev_dri_present={}",
        Path::new("/dev/dri").exists()
    ));
    let present = cuda_visible
        || command_available("nvidia-smi")
        || Path::new("/dev/nvidia0").exists()
        || Path::new("/dev/dri").exists();
    evidence.push(format!("gpu_present={present}"));
    evidence
}

fn npu_probe_evidence() -> Vec<String> {
    let candidates = [
        "/dev/accel/accel0",
        "/dev/apex_0",
        "/dev/davinci0",
        "/dev/hisi_hdc",
        "/dev/vpu_service",
    ];
    let mut evidence = candidates
        .iter()
        .map(|path| format!("{path}_present={}", Path::new(path).exists()))
        .collect::<Vec<_>>();
    let present = candidates.iter().any(|path| Path::new(path).exists());
    evidence.push(format!("npu_present={present}"));
    evidence
}

fn command_available(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn build_home_assistant_status_response(
    state: &HomeAssistantAdminState,
) -> HomeAssistantStatusResponse {
    let token_configured = !state.access_token.trim().is_empty();
    HomeAssistantStatusResponse {
        configured: !state.base_url.trim().is_empty() && token_configured,
        enabled: state.enabled,
        base_url: state.base_url.clone(),
        token_configured,
        token_redacted: token_configured,
        exposed_domains: state.exposed_domains.clone(),
        status: state.last_status.clone(),
        last_error: state.last_error.clone(),
        last_test_at: state.last_test_at.clone(),
        last_sync_at: state.last_sync_at.clone(),
        entity_count: state.entity_count,
        service_count: state.service_count,
        version: state.version.clone(),
        location_name: state.location_name.clone(),
    }
}

fn home_assistant_client_from_state(
    state: &HomeAssistantAdminState,
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

fn filter_home_assistant_entities(
    entities: Vec<HomeAssistantEntity>,
    exposed_domains: &[String],
) -> Vec<HomeAssistantEntity> {
    if exposed_domains.is_empty() {
        return entities;
    }
    let allowed = exposed_domains
        .iter()
        .map(|domain| domain.as_str())
        .collect::<HashSet<_>>();
    entities
        .into_iter()
        .filter(|entity| allowed.contains(entity.domain.as_str()))
        .collect()
}

fn home_assistant_registry_entity(entity: HomeAssistantEntity) -> HomeAssistantRegistryEntity {
    HomeAssistantRegistryEntity {
        entity_id: entity.entity_id,
        domain: entity.domain,
        display_name: entity.display_name,
        state: entity.state,
        area_id: entity.area_id,
        device_class: entity.device_class,
        last_changed: entity.last_changed,
        last_updated: entity.last_updated,
        attributes: entity.attributes,
    }
}

fn normalize_home_assistant_service_smoke_request(
    request: &HomeAssistantServiceSmokeRequest,
) -> HomeAssistantServiceSmokeRequest {
    normalize_home_assistant_service_action_request(request)
}

fn validate_home_assistant_service_smoke(
    request: &HomeAssistantServiceSmokeRequest,
    state: &HomeAssistantAdminState,
) -> Result<(), String> {
    validate_home_assistant_service_action_request(request, state.enabled, &state.exposed_domains)
}

#[cfg(test)]
fn validate_home_assistant_service_fields(fields: &Value) -> Result<(), String> {
    validate_home_assistant_service_fields_shared(fields)
}

fn find_recent_local_vision_event(
    event_id: &str,
) -> Result<Option<StoredLocalVisionEvent>, String> {
    let event_id = event_id.trim();
    if event_id.is_empty() {
        return Ok(None);
    }
    Ok(list_recent_local_vision_events_default(50)?
        .into_iter()
        .find(|stored| stored.event.event_id == event_id))
}

fn build_vlm_enrichment_evidence(
    stored: &StoredLocalVisionEvent,
    status: &str,
    elapsed_ms: u64,
    model_endpoint_id: Option<Value>,
    reason: &str,
    trigger: &str,
    readiness: Value,
) -> Value {
    json!({
        "kind": "vision_vlm_enrichment_v1",
        "status": status,
        "event_id": stored.event.event_id,
        "camera_id": stored.event.camera_id,
        "event_type": stored.event.event_type,
        "elapsed_ms": elapsed_ms,
        "model_endpoint_id": model_endpoint_id,
        "model_endpoint_id_redacted": false,
        "local_only": true,
        "fallback_allowed": false,
        "queue_result": if status == "busy" { "busy" } else { "accepted" },
        "frame_source": "fresh_camera_snapshot",
        "frame_path_redacted": true,
        "raw_image_included": false,
        "local_paths_included": false,
        "summary_source": if status == "active" { "vlm" } else { "degraded_fallback" },
        "redaction_status": "clean",
        "reason": redact_admin_string(reason),
        "trigger": trigger,
        "readiness": readiness,
        "metadata_only": true,
        "secret_scan": "clean",
    })
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

fn build_local_vision_event_notification_blocked_response(
    stored: &StoredLocalVisionEvent,
    target: Option<&NotificationTargetRecord>,
    message: &str,
) -> LocalVisionEventNotificationResponse {
    build_local_vision_event_notification_response(
        stored,
        target,
        "blocked",
        None,
        None,
        None,
        message,
        "local_vision_event.notification_blocked",
    )
}

fn build_local_vision_event_notification_failed_response(
    stored: &StoredLocalVisionEvent,
    target: Option<&NotificationTargetRecord>,
    delivery_record: Option<NotificationDeliveryRecord>,
    notification_id: Option<String>,
    message: &str,
) -> LocalVisionEventNotificationResponse {
    let delivery_id = delivery_record
        .as_ref()
        .map(|record| record.delivery_id.clone());
    let notification_id = notification_id.or_else(|| {
        delivery_record
            .as_ref()
            .map(|record| record.notification_id.clone())
    });
    let delivery_record = delivery_record
        .as_ref()
        .map(notification_delivery_record_summary);
    build_local_vision_event_notification_response(
        stored,
        target,
        "failed",
        notification_id,
        delivery_id,
        delivery_record,
        message,
        "local_vision_event.notification_failed",
    )
}

fn build_local_vision_event_notification_delivered_response(
    stored: &StoredLocalVisionEvent,
    target: &NotificationTargetRecord,
    delivery_record: NotificationDeliveryRecord,
    intent_audit_record: Value,
) -> LocalVisionEventNotificationResponse {
    let delivery_id = delivery_record.delivery_id.clone();
    let notification_id = delivery_record.notification_id.clone();
    let delivery_record = notification_delivery_record_summary(&delivery_record);
    let mut response = build_local_vision_event_notification_response(
        stored,
        Some(target),
        "delivered",
        Some(notification_id),
        Some(delivery_id),
        Some(delivery_record),
        "Local vision event notification delivered to the default target.",
        "local_vision_event.notification_delivered",
    );
    response.audit_record["intent_audit_record"] = intent_audit_record;
    response
}

fn build_local_vision_event_notification_delivery_error_response(
    stored: &StoredLocalVisionEvent,
    target: &NotificationTargetRecord,
    error: NotificationDeliveryError,
) -> LocalVisionEventNotificationResponse {
    let (status, message, error_kind) = classify_notification_delivery_error(error);
    let mut response = build_local_vision_event_notification_response(
        stored,
        Some(target),
        status,
        None,
        None,
        Some(json!({
            "error_kind": error_kind,
            "message": message.clone(),
            "redacted": true,
        })),
        &message,
        if status == "blocked" {
            "local_vision_event.notification_blocked"
        } else {
            "local_vision_event.notification_failed"
        },
    );
    response.audit_record["delivery_error_kind"] = json!(error_kind);
    response
}

fn build_local_vision_event_notification_response(
    stored: &StoredLocalVisionEvent,
    target: Option<&NotificationTargetRecord>,
    status: &str,
    notification_id: Option<String>,
    delivery_id: Option<String>,
    delivery_record: Option<Value>,
    message: &str,
    audit_kind: &str,
) -> LocalVisionEventNotificationResponse {
    LocalVisionEventNotificationResponse {
        event_id: stored.event.event_id.clone(),
        status: status.to_string(),
        notification_id,
        delivery_id,
        target_label: target.map(|target| target.label.clone()),
        platform_hint: target.and_then(|target| non_empty_string(&target.platform_hint)),
        message: redact_admin_string(message),
        delivery_record,
        audit_record: json!({
            "audit_kind": audit_kind,
            "status": status,
            "metadata_only": true,
            "secret_scan": "clean",
            "event_id": stored.event.event_id,
            "camera_id": stored.event.camera_id,
            "event_type": stored.event.event_type,
            "target_bound": target.is_some(),
            "text_only": true,
            "attachments_included": false,
            "raw_image_included": false,
            "local_paths_included": false,
            "created_at": now_unix_string(),
        }),
    }
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
            "message": redact_admin_string(&error.message),
        })),
        "redacted": true,
    })
}

fn classify_notification_delivery_error(
    error: NotificationDeliveryError,
) -> (&'static str, String, &'static str) {
    match error {
        NotificationDeliveryError::MissingConfiguration(message) => (
            "blocked",
            redact_admin_string(&message),
            "missing_configuration",
        ),
        NotificationDeliveryError::Transport(message) => (
            "failed",
            redact_admin_string(&message),
            "gateway_unreachable",
        ),
        NotificationDeliveryError::InvalidResponse(message) => {
            ("failed", redact_admin_string(&message), "invalid_response")
        }
        NotificationDeliveryError::RequestRejected {
            status_code,
            envelope,
        } => (
            "failed",
            redact_admin_string(&format!(
                "HarborGate rejected the notification with HTTP {status_code}: {} ({})",
                envelope.error.message, envelope.error.code
            )),
            "request_rejected",
        ),
    }
}

fn build_default_notification_target_readiness(
    targets: &[NotificationTargetRecord],
    gateway_status: Option<&Value>,
) -> Value {
    let target = default_notification_target_record(targets);
    let gateway_connected = gateway_connected(gateway_status);
    let gateway_configured = gateway_configured(gateway_status);
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
        "platform_hint": target.and_then(|target| non_empty_string(&target.platform_hint)),
        "gateway_configured": gateway_configured,
        "gateway_connected": gateway_connected,
        "route_key_redacted": target.is_some(),
    })
}

fn build_redacted_gateway_delivery_observability(gateway_status: Option<&Value>) -> Value {
    let Some(payload) = gateway_status else {
        return json!({
            "status": "unavailable",
            "redacted": true,
        });
    };
    let observability = payload
        .get("delivery_observability")
        .cloned()
        .unwrap_or_else(|| json!({}));
    json!({
        "status": if observability.is_object() { "available" } else { "unavailable" },
        "record_count": observability.get("record_count").and_then(Value::as_u64),
        "redacted": true,
    })
}

fn gateway_connected(gateway_status: Option<&Value>) -> bool {
    gateway_status
        .and_then(|value| value.get("connected").and_then(Value::as_bool))
        .or_else(|| {
            gateway_status
                .and_then(|value| value.pointer("/bridge_provider/connected"))
                .and_then(Value::as_bool)
        })
        .unwrap_or(false)
}

fn gateway_configured(gateway_status: Option<&Value>) -> bool {
    gateway_status
        .and_then(|value| value.get("configured").and_then(Value::as_bool))
        .or_else(|| {
            gateway_status
                .and_then(|value| value.pointer("/bridge_provider/configured"))
                .and_then(Value::as_bool)
        })
        .unwrap_or(false)
}

fn build_home_assistant_operator_audit(
    audit_kind: &str,
    status: &str,
    allowed: bool,
    executed: bool,
    message: &str,
    request: &HomeAssistantServiceSmokeRequest,
) -> Value {
    json!({
        "audit_kind": audit_kind,
        "status": status,
        "metadata_only": true,
        "secret_scan": "clean",
        "allowed": allowed,
        "executed": executed,
        "entity_id": request.entity_id.trim().to_lowercase(),
        "domain": request.domain.trim().to_lowercase(),
        "service": request.service.trim().to_lowercase(),
        "message": redact_admin_string(message),
        "created_at": now_unix_string(),
    })
}

fn build_home_assistant_install_status_response() -> HomeAssistantInstallStatusResponse {
    if !command_available("docker") {
        return HomeAssistantInstallStatusResponse {
            app_id: "home-assistant".to_string(),
            status: "blocked".to_string(),
            managed: true,
            runtime: "docker_container".to_string(),
            container_name: Some(HOME_ASSISTANT_CONTAINER_NAME.to_string()),
            onboarding_url: None,
            message:
                "Docker is not available on this HarborOS host; managed install cannot run here."
                    .to_string(),
        };
    }
    if let Some(status) = docker_container_status(HOME_ASSISTANT_CONTAINER_NAME) {
        let running = status == "running";
        return HomeAssistantInstallStatusResponse {
            app_id: "home-assistant".to_string(),
            status: if running { "running" } else { status.as_str() }.to_string(),
            managed: true,
            runtime: "docker_container".to_string(),
            container_name: Some(HOME_ASSISTANT_CONTAINER_NAME.to_string()),
            onboarding_url: running.then(|| HOME_ASSISTANT_ONBOARDING_URL.to_string()),
            message: if running {
                "Home Assistant container is running; finish onboarding, then connect HarborBeacon with a long-lived token."
                    .to_string()
            } else {
                format!("Home Assistant container exists but Docker reports status={status}.")
            },
        };
    }
    HomeAssistantInstallStatusResponse {
        app_id: "home-assistant".to_string(),
        status: "not_installed".to_string(),
        managed: true,
        runtime: "docker_container".to_string(),
        container_name: Some(HOME_ASSISTANT_CONTAINER_NAME.to_string()),
        onboarding_url: None,
        message:
            "Home Assistant Container is not installed yet; request install to create it with Docker."
                .to_string(),
    }
}

fn build_home_assistant_install_plan_response() -> HomeAssistantInstallPlanResponse {
    HomeAssistantInstallPlanResponse {
        app_id: "home-assistant".to_string(),
        target: "Home Assistant Container".to_string(),
        runtime: "docker".to_string(),
        image: HOME_ASSISTANT_IMAGE.to_string(),
        container_name: HOME_ASSISTANT_CONTAINER_NAME.to_string(),
        ports: vec!["8123:8123/tcp".to_string()],
        volumes: vec![format!("{HOME_ASSISTANT_VOLUME_NAME}:/config")],
        next_step: "Create the container through the HarborOS app executor, finish HA onboarding, then connect HarborBeacon with a long-lived access token.".to_string(),
    }
}

const HOME_ASSISTANT_CONTAINER_NAME: &str = "harbor-home-assistant";
const HOME_ASSISTANT_VOLUME_NAME: &str = "harbor-home-assistant-config";
const HOME_ASSISTANT_IMAGE: &str = "ghcr.io/home-assistant/home-assistant:stable";
const HOME_ASSISTANT_ONBOARDING_URL: &str = "http://127.0.0.1:8123";

fn install_home_assistant_container() -> Result<HomeAssistantInstallStatusResponse, String> {
    if !command_available("docker") {
        return Err("Docker is not available on this HarborOS host".to_string());
    }
    if docker_container_exists(HOME_ASSISTANT_CONTAINER_NAME) {
        docker_command(["start", HOME_ASSISTANT_CONTAINER_NAME])?;
        return Ok(build_home_assistant_install_status_response());
    }
    docker_command(["volume", "create", HOME_ASSISTANT_VOLUME_NAME])?;
    docker_command([
        "run",
        "-d",
        "--name",
        HOME_ASSISTANT_CONTAINER_NAME,
        "--restart",
        "unless-stopped",
        "-p",
        "8123:8123",
        "-v",
        &format!("{HOME_ASSISTANT_VOLUME_NAME}:/config"),
        HOME_ASSISTANT_IMAGE,
    ])?;
    Ok(build_home_assistant_install_status_response())
}

fn docker_container_exists(name: &str) -> bool {
    docker_container_status(name).is_some()
}

fn docker_container_status(name: &str) -> Option<String> {
    let output = Command::new("docker")
        .args(["inspect", "-f", "{{.State.Status}}", name])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let status = String::from_utf8_lossy(&output.stdout).trim().to_string();
    non_empty_string(&status)
}

fn docker_command<const N: usize>(args: [&str; N]) -> Result<String, String> {
    let output = Command::new("docker")
        .args(args)
        .output()
        .map_err(|error| format!("failed to execute Docker: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if output.status.success() {
        return Ok(stdout);
    }
    Err(if stderr.is_empty() {
        format!("Docker command failed with status {}", output.status)
    } else {
        stderr
    })
}

fn build_harboros_status_response(public_origin: &str) -> HarborOsStatusResponse {
    let webui_url = harboros_webui_url(public_origin);
    let writable_root = env::var("HARBOR_HARBOROS_WRITABLE_ROOT")
        .unwrap_or_else(|_| "/mnt/software/harborbeacon-agent-ci".to_string());
    let writable_root_exists = Path::new(&writable_root).exists();
    let version = env::var("HARBOROS_VERSION")
        .ok()
        .and_then(|value| non_empty_string(&value))
        .or_else(os_release_pretty_name)
        .unwrap_or_else(|| "unknown".to_string());
    let services = vec![
        HarborOsServiceStatus {
            service_id: "harbor-assistant-admin-api".to_string(),
            label: "Harbor Assistant Admin API".to_string(),
            status: "ready".to_string(),
            detail: "Current process serves Harbor Assistant on port 4174.".to_string(),
        },
        HarborOsServiceStatus {
            service_id: "harboros-webui".to_string(),
            label: "HarborOS WebUI".to_string(),
            status: "external".to_string(),
            detail: format!("Expected at {webui_url}; Harbor Assistant does not own this port."),
        },
        HarborOsServiceStatus {
            service_id: "writable-root".to_string(),
            label: "HarborOS writable root".to_string(),
            status: if writable_root_exists {
                "ready"
            } else {
                "needs-config"
            }
            .to_string(),
            detail: writable_root.clone(),
        },
    ];
    let jobs_alerts = HarborOsServiceStatus {
        service_id: "jobs-alerts-readiness".to_string(),
        label: "Jobs / Alerts entry readiness".to_string(),
        status: "ready".to_string(),
        detail:
            "Safe query capability only; approval is required before any state-changing job action."
                .to_string(),
    };
    let storage_files_entry = HarborOsServiceStatus {
        service_id: "storage-files-entry-readiness".to_string(),
        label: "Storage / Files entry readiness".to_string(),
        status: if writable_root_exists {
            "ready"
        } else {
            "needs-config"
        }
        .to_string(),
        detail: format!(
            "Storage entry is checked through HarborOS System Domain path: {writable_root}"
        ),
    };
    let blockers = if writable_root_exists {
        Vec::new()
    } else {
        vec![format!("writable root not found: {writable_root}")]
    };
    HarborOsStatusResponse {
        generated_at: now_unix_string(),
        status: if blockers.is_empty() {
            "ready"
        } else {
            "needs-config"
        }
        .to_string(),
        version,
        webui_url,
        system_domain_only: true,
        services,
        jobs_alerts,
        storage_files_entry,
        evidence: vec![
            "domain=HarborOS System Domain".to_string(),
            "aiot_management=excluded".to_string(),
            "jobs_alerts.safe_query=true".to_string(),
            "storage_files.entry_ready_checked=true".to_string(),
            format!("writable_root_exists={writable_root_exists}"),
        ],
        blockers,
    }
}

fn os_release_pretty_name() -> Option<String> {
    let text = fs::read_to_string("/etc/os-release").ok()?;
    text.lines()
        .find_map(|line| line.strip_prefix("PRETTY_NAME="))
        .map(|value| value.trim_matches('"').to_string())
        .and_then(|value| non_empty_string(&value))
}

fn build_harboros_im_capability_map() -> HarborOsImCapabilityMapResponse {
    HarborOsImCapabilityMapResponse {
        generated_at: now_unix_string(),
        source: "exports/harboros_webui_manual/harboros_webui_user_guide.md".to_string(),
        items: vec![
            harboros_im_capability(
                "dashboard.status",
                "Dashboard system status",
                true,
                "low",
                false,
                "Dashboard",
                "Query version, hostname, uptime, CPU, memory, and temperature.",
            ),
            harboros_im_capability(
                "jobs.alerts",
                "Jobs and Alerts",
                true,
                "low",
                false,
                "Jobs / Alerts",
                "Read-only recent task and alert summaries are IM-safe.",
            ),
            harboros_im_capability(
                "services.status",
                "Service status",
                true,
                "low",
                false,
                "System / Services",
                "Read service state without changing autostart or configuration.",
            ),
            harboros_im_capability(
                "storage.summary",
                "Storage summary",
                true,
                "medium",
                false,
                "Storage / Datasets",
                "Read pool and dataset summary; mutation requires approval.",
            ),
            harboros_im_capability(
                "file.entrypoints",
                "File Manager entrypoints",
                true,
                "medium",
                false,
                "File Manager",
                "Expose locations and status, not arbitrary file mutation.",
            ),
            harboros_im_capability(
                "services.restart",
                "Service restart",
                false,
                "high",
                true,
                "System / Services",
                "High-risk operation; show capability and require approval before execution.",
            ),
            harboros_im_capability(
                "backup.restore",
                "Backup and restore",
                false,
                "high",
                true,
                "Data Protection",
                "High-risk state-changing workflow; not auto-executed from readiness.",
            ),
        ],
    }
}

fn harboros_im_capability(
    capability_id: &str,
    label: &str,
    im_ready: bool,
    risk_level: &str,
    approval_required: bool,
    harboros_surface: &str,
    notes: &str,
) -> HarborOsImCapabilityItem {
    HarborOsImCapabilityItem {
        capability_id: capability_id.to_string(),
        label: label.to_string(),
        capability_class: if approval_required {
            "approval_required_action"
        } else if im_ready {
            "safe_query"
        } else {
            "unsupported_high_risk"
        }
        .to_string(),
        im_ready,
        risk_level: risk_level.to_string(),
        approval_required,
        harboros_surface: harboros_surface.to_string(),
        notes: notes.to_string(),
    }
}

fn build_local_model_catalog(
    download_jobs: Vec<ModelDownloadJobRecord>,
) -> LocalModelCatalogResponse {
    build_local_model_catalog_for_roots(local_model_cache_roots(), download_jobs)
}

fn build_local_model_catalog_for_model_state(
    model_state: &AdminModelCenterState,
    download_jobs: Vec<ModelDownloadJobRecord>,
) -> LocalModelCatalogResponse {
    build_local_model_catalog_for_roots(
        local_model_cache_roots_for_model_state(model_state),
        download_jobs,
    )
}

fn build_local_model_catalog_for_roots(
    cache_roots: Vec<String>,
    download_jobs: Vec<ModelDownloadJobRecord>,
) -> LocalModelCatalogResponse {
    let latest_download_jobs = latest_model_download_jobs(&download_jobs);
    let hardware = build_hardware_readiness_response();
    let models = local_model_catalog_specs()
        .into_iter()
        .map(|spec| {
            local_model_catalog_item_with_hardware(
                &cache_roots,
                &latest_download_jobs,
                spec,
                &hardware,
            )
        })
        .collect::<Vec<_>>();
    let generated_at = now_unix_string();
    LocalModelCatalogResponse {
        generated_at: generated_at.clone(),
        checked_at: generated_at,
        status: if models.is_empty() {
            "needs-config"
        } else {
            "ready"
        }
        .to_string(),
        cache_roots,
        models,
        download_jobs: latest_download_jobs.clone(),
        downloads: latest_download_jobs,
        blockers: Vec::new(),
        warnings: Vec::new(),
    }
}

fn build_model_capabilities_response(
    model_state: &AdminModelCenterState,
    endpoints: &[ModelEndpoint],
    route_policies: &[ModelRoutePolicy],
    download_jobs: Vec<ModelDownloadJobRecord>,
    runtime: &LocalModelRuntimeProjection,
) -> ModelCapabilitiesResponse {
    let download_jobs = latest_model_download_jobs(&download_jobs);
    let catalog = build_local_model_catalog_for_model_state(model_state, download_jobs.clone());
    let model_store = build_model_store_status(model_state);
    let generated_at = now_unix_string();
    let runtime_manager = build_model_runtime_manager_response(model_state, runtime);
    let capabilities = vec![
        build_model_capability_status(
            model_state,
            "semantic_router",
            "问题理解",
            ModelKind::Llm,
            "semantic.router",
            endpoints,
            route_policies,
            &catalog.models,
            &download_jobs,
            runtime,
            true,
        ),
        build_model_capability_status(
            model_state,
            "embedder",
            "向量检索",
            ModelKind::Embedder,
            "retrieval.embed",
            endpoints,
            route_policies,
            &catalog.models,
            &download_jobs,
            runtime,
            true,
        ),
        build_model_capability_status(
            model_state,
            "retrieval_answer",
            "对话回答",
            ModelKind::Llm,
            "retrieval.answer",
            endpoints,
            route_policies,
            &catalog.models,
            &download_jobs,
            runtime,
            true,
        ),
        build_model_capability_status(
            model_state,
            "vlm",
            "图片/视频理解",
            ModelKind::Vlm,
            "retrieval.vision_summary",
            endpoints,
            route_policies,
            &catalog.models,
            &download_jobs,
            runtime,
            false,
        ),
        build_model_capability_status(
            model_state,
            "ocr",
            "文字识别",
            ModelKind::Ocr,
            "retrieval.ocr",
            endpoints,
            route_policies,
            &catalog.models,
            &download_jobs,
            runtime,
            false,
        ),
        build_model_capability_status(
            model_state,
            "asr",
            "语音转文字",
            ModelKind::Asr,
            "retrieval.asr",
            endpoints,
            route_policies,
            &catalog.models,
            &download_jobs,
            runtime,
            false,
        ),
    ];
    let blockers = capabilities
        .iter()
        .filter(|capability| {
            matches!(
                capability.status.as_str(),
                "needs_model" | "needs_runtime" | "degraded"
            )
        })
        .map(|capability| format!("{}: {}", capability.capability_id, capability.next_action))
        .collect::<Vec<_>>();
    let status = if capabilities
        .iter()
        .any(|capability| capability.status == "degraded")
    {
        "degraded"
    } else if capabilities
        .iter()
        .any(|capability| capability.status == "needs_runtime")
    {
        "needs-runtime"
    } else if capabilities
        .iter()
        .any(|capability| capability.status == "needs_model")
    {
        "needs-config"
    } else {
        "ready"
    }
    .to_string();

    ModelCapabilitiesResponse {
        generated_at: generated_at.clone(),
        checked_at: generated_at,
        status,
        model_store,
        runtime_manager,
        capabilities,
        blockers,
        warnings: Vec::new(),
    }
}

fn build_model_runtime_manager_response(
    model_state: &AdminModelCenterState,
    runtime: &LocalModelRuntimeProjection,
) -> ModelRuntimeManagerResponse {
    let generated_at = now_unix_string();
    let runtimes = model_runtime_records_for_state(model_state)
        .into_iter()
        .map(|record| model_runtime_status(record, runtime))
        .collect::<Vec<_>>();
    let blockers = runtimes
        .iter()
        .filter(|runtime| {
            runtime.record.managed && !runtime.installed && runtime.record.installable
        })
        .map(|runtime| format!("{}: {}", runtime.record.runtime_id, runtime.next_action))
        .collect::<Vec<_>>();
    let status = if runtimes.iter().any(|runtime| runtime.active) {
        "ready"
    } else if runtimes
        .iter()
        .any(|runtime| runtime.record.managed && runtime.installed)
    {
        "installed"
    } else if runtimes
        .iter()
        .any(|runtime| runtime.record.managed && runtime.record.installable)
    {
        "needs-runtime"
    } else {
        "degraded"
    }
    .to_string();

    ModelRuntimeManagerResponse {
        generated_at: generated_at.clone(),
        checked_at: generated_at,
        status,
        runtimes,
        blockers,
        warnings: Vec::new(),
    }
}

fn model_runtime_records_for_state(model_state: &AdminModelCenterState) -> Vec<ModelRuntimeRecord> {
    let mut runtimes = model_state.runtimes.clone();
    let mut seen = runtimes
        .iter()
        .map(|runtime| runtime.runtime_id.clone())
        .collect::<HashSet<_>>();
    for runtime in default_model_runtimes_for_store_root(&model_state.model_store_root) {
        if seen.insert(runtime.runtime_id.clone()) {
            runtimes.push(runtime);
        }
    }
    runtimes.sort_by(|left, right| left.runtime_id.cmp(&right.runtime_id));
    runtimes
}

fn model_runtime_status(
    mut record: ModelRuntimeRecord,
    runtime: &LocalModelRuntimeProjection,
) -> ModelRuntimeStatus {
    let runtime_ready = runtime.ready
        && runtime.backend_ready
        && runtime
            .embedding_model
            .as_ref()
            .is_some_and(|model| !model.trim().is_empty());
    let installed = matches!(
        record.status.as_str(),
        "installed" | "active" | "ready" | "running"
    ) || record.enabled;
    let active = installed
        && runtime_ready
        && record.runtime_profiles.iter().any(|profile| {
            runtime_profile_matches_backend(profile, runtime.backend_kind.as_deref())
        });
    if active {
        let has_loaded_model = runtime.chat_model.is_some() || runtime.embedding_model.is_some();
        record.status = if has_loaded_model { "active" } else { "idle" }.to_string();
        record.message = if has_loaded_model {
            "Runtime 已启用，并正在服务 Harbor-managed 推理路径。".to_string()
        } else {
            "Runtime 已启用并处于空闲状态；选择或安装模型后会按需加载。".to_string()
        };
    }
    let next_action = if active {
        if runtime.chat_model.is_some() || runtime.embedding_model.is_some() {
            "可以使用".to_string()
        } else {
            "选择或安装 Harbor-managed 模型".to_string()
        }
    } else if installed {
        "选择支持的模型后会启动 runtime".to_string()
    } else if record.installable {
        format!("安装 {}", record.display_name)
    } else {
        "当前 ISO 未包含该 runtime 包，可在高级设置接入 OpenAI-compatible endpoint".to_string()
    };

    ModelRuntimeStatus {
        record,
        installed,
        active,
        next_action,
    }
}

fn model_runtime_status_for_profile(
    model_state: &AdminModelCenterState,
    runtime: &LocalModelRuntimeProjection,
    profile: &str,
) -> Option<ModelRuntimeStatus> {
    let canonical = canonical_managed_runtime_profile(profile)?;
    model_runtime_records_for_state(model_state)
        .into_iter()
        .find(|record| {
            record.runtime_id == canonical
                || record
                    .runtime_profiles
                    .iter()
                    .any(|profile| canonical_managed_runtime_profile(profile) == Some(canonical))
        })
        .map(|record| model_runtime_status(record, runtime))
}

fn runtime_profile_matches_backend(profile: &str, backend_kind: Option<&str>) -> bool {
    let Some(canonical) = canonical_managed_runtime_profile(profile) else {
        return false;
    };
    matches!(
        (canonical, backend_kind.map(|value| value.trim().to_ascii_lowercase())),
        ("harbor-candle", Some(kind)) if kind == "candle"
    )
}

fn canonical_managed_runtime_profile(profile: &str) -> Option<&'static str> {
    match profile.trim().to_ascii_lowercase().as_str() {
        "harbor-candle" | "harbor-model-api-candle" => Some("harbor-candle"),
        "harbor-vlm-sidecar" | "cpu-vlm-sidecar" => Some("harbor-vlm-sidecar"),
        "harbor-ocr-runtime" => Some("harbor-ocr-runtime"),
        "harbor-asr-runtime" => Some("harbor-asr-runtime"),
        _ => None,
    }
}

fn default_managed_runtime_profile_for_capability(
    capability_id: &str,
    model_kind: ModelKind,
) -> Option<&'static str> {
    match capability_id.trim() {
        "semantic_router" | "retrieval_answer" => Some("harbor-candle"),
        "embedder" => Some("harbor-candle"),
        "vlm" => Some("harbor-vlm-sidecar"),
        "ocr" => Some("harbor-ocr-runtime"),
        "asr" => Some("harbor-asr-runtime"),
        _ => match model_kind {
            ModelKind::Llm | ModelKind::Embedder => Some("harbor-candle"),
            ModelKind::Vlm => Some("harbor-vlm-sidecar"),
            ModelKind::Ocr => Some("harbor-ocr-runtime"),
            ModelKind::Asr => Some("harbor-asr-runtime"),
            _ => None,
        },
    }
}

fn managed_runtime_profile_for_model(model: &LocalModelCatalogItem) -> Option<&'static str> {
    model
        .runtime_profiles
        .iter()
        .find_map(|profile| canonical_managed_runtime_profile(profile))
}

fn external_runtime_profile_for_model(model: &LocalModelCatalogItem) -> bool {
    managed_runtime_profile_for_model(model).is_none()
        && model
            .runtime_profiles
            .iter()
            .any(|profile| runtime_profile_is_external(profile))
}

fn runtime_profile_is_external(profile: &str) -> bool {
    let normalized = profile.trim().to_ascii_lowercase();
    normalized.contains("openai-compatible")
        || normalized.contains("vllm")
        || normalized.contains("sglang")
}

#[allow(clippy::too_many_arguments)]
fn build_model_capability_status(
    model_state: &AdminModelCenterState,
    capability_id: &str,
    label: &str,
    model_kind: ModelKind,
    route_policy_id: &str,
    endpoints: &[ModelEndpoint],
    route_policies: &[ModelRoutePolicy],
    catalog_models: &[LocalModelCatalogItem],
    download_jobs: &[ModelDownloadJobRecord],
    runtime: &LocalModelRuntimeProjection,
    runtime_bound: bool,
) -> ModelCapabilityStatus {
    let endpoint = select_model_endpoint(
        endpoints,
        preferred_endpoint_id_for_model_kind(model_kind),
        model_kind,
    );
    let runtime_ready = model_capability_runtime_ready(model_kind, runtime, runtime_bound);
    let selected_model_id = selected_model_id_for_capability(model_state, capability_id);
    let installable_models = catalog_models
        .iter()
        .filter(|model| {
            model.installable
                && catalog_model_matches_kind_or_capability(model, model_kind, capability_id)
        })
        .filter(|model| !model.installed)
        .map(model_capability_installable_model)
        .collect::<Vec<_>>();
    let installed_models = catalog_models
        .iter()
        .filter(|model| {
            model.installed
                && catalog_model_matches_kind_or_capability(model, model_kind, capability_id)
        })
        .map(model_capability_installable_model)
        .collect::<Vec<_>>();
    let capability_jobs = latest_model_download_jobs(
        &download_jobs
            .iter()
            .filter(|job| {
                job.metadata
                    .get("capability_id")
                    .and_then(Value::as_str)
                    .is_some_and(|value| value == capability_id)
                    || catalog_models
                        .iter()
                        .find(|model| model.model_id == job.model_id)
                        .is_some_and(|model| {
                            catalog_model_matches_kind_or_capability(
                                model,
                                model_kind,
                                capability_id,
                            )
                        })
            })
            .cloned()
            .collect::<Vec<_>>(),
    );
    let active_download = capability_jobs.iter().any(model_download_job_is_active);
    let installed_model = catalog_models
        .iter()
        .find(|model| {
            selected_model_id
                .as_ref()
                .is_some_and(|selected| selected == &model.model_id)
        })
        .or_else(|| {
            catalog_models.iter().find(|model| {
                model.installed
                    && catalog_model_matches_kind_or_capability(model, model_kind, capability_id)
            })
        });
    let selected_external_model =
        installed_model.is_some_and(|model| managed_runtime_profile_for_model(model).is_none());
    let required_runtime_profile = installed_model
        .and_then(managed_runtime_profile_for_model)
        .or_else(|| {
            if selected_external_model {
                None
            } else {
                default_managed_runtime_profile_for_capability(capability_id, model_kind)
            }
        });
    let runtime_status = required_runtime_profile
        .and_then(|profile| model_runtime_status_for_profile(model_state, runtime, profile));
    let runtime_installed = runtime_status
        .as_ref()
        .is_some_and(|status| status.installed);
    let runtime_installable = runtime_status
        .as_ref()
        .is_some_and(|status| status.record.installable);
    let runtime_status_value = runtime_status
        .as_ref()
        .map(|status| status.record.status.clone());
    let runtime_next_action = runtime_status
        .as_ref()
        .map(|status| status.next_action.clone());
    let runtime_missing = required_runtime_profile.is_some() && !runtime_installed;
    let endpoint_ready = endpoint.is_some_and(|value| value.status == ModelEndpointStatus::Active);
    let ready = if runtime_bound {
        runtime_ready
    } else {
        endpoint_ready
    };
    let unsupported =
        endpoint.is_none() && installable_models.is_empty() && capability_jobs.is_empty();
    let status = if ready {
        "ready"
    } else if active_download {
        "downloading"
    } else if runtime_missing {
        "needs_runtime"
    } else if installed_model.is_some() {
        "installed_not_running"
    } else if runtime_installed {
        "needs_model"
    } else if unsupported {
        "unsupported"
    } else if endpoint.is_some() {
        "degraded"
    } else {
        "needs_model"
    }
    .to_string();
    let current_model = (status != "needs_model")
        .then(|| {
            endpoint.map(|endpoint| ModelCapabilityCurrentModel {
                model_endpoint_id: endpoint.model_endpoint_id.clone(),
                model_name: runtime_model_name_for_kind(model_kind, runtime)
                    .unwrap_or_else(|| endpoint.model_name.clone()),
                provider_key: endpoint.provider_key.clone(),
                status: endpoint.status.as_str().to_string(),
            })
        })
        .flatten();
    let runtime_model_id = runtime_model_name_for_kind(model_kind, runtime);
    let policy = find_route_policy(route_policies, route_policy_id);
    let next_action = match status.as_str() {
        "ready" => "可以使用".to_string(),
        "downloading" => "等待模型下载完成".to_string(),
        "needs_runtime" => runtime_next_action
            .clone()
            .unwrap_or_else(|| "安装 Harbor-managed runtime".to_string()),
        "installed_not_running" => "模型已安装，点击选择会自动切换并启动".to_string(),
        "unsupported" => "暂不支持该能力".to_string(),
        "degraded" => runtime
            .error
            .clone()
            .unwrap_or_else(|| "模型服务需要检查".to_string()),
        _ => "选择或安装模型".to_string(),
    };
    let mut evidence = vec![
        format!("route_policy={route_policy_id}"),
        format!("route_policy_status={}", policy_status_value(policy)),
        format!("runtime_bound={runtime_bound}"),
        format!("runtime_ready={runtime_ready}"),
    ];
    if let Some(profile) = required_runtime_profile {
        evidence.push(format!("required_runtime_profile={profile}"));
    }
    if let Some(status) = runtime_status.as_ref() {
        evidence.push(format!(
            "runtime={} status={} installed={} active={}",
            status.record.runtime_id, status.record.status, status.installed, status.active
        ));
    }
    if let Some(endpoint) = endpoint {
        evidence.push(format!(
            "endpoint={} status={}",
            endpoint.model_endpoint_id,
            endpoint.status.as_str()
        ));
    }
    if let Some(model) = installed_model {
        evidence.push(format!("installed_model={}", model.model_id));
    }

    ModelCapabilityStatus {
        capability_id: capability_id.to_string(),
        label: label.to_string(),
        model_kind: model_kind.as_str().to_string(),
        status,
        selected_model_id,
        runtime_model_id,
        current_model,
        installed_models,
        installable_models,
        download_jobs: capability_jobs,
        next_action,
        runtime_ready,
        required_runtime_profile: required_runtime_profile.map(str::to_string),
        runtime_installed,
        runtime_installable,
        runtime_status: runtime_status_value,
        runtime_next_action,
        source_of_truth: if runtime_bound {
            "runtime manager + local inference runtime + route policy + model catalog".to_string()
        } else {
            "runtime manager + route policy + model endpoint + model catalog".to_string()
        },
        evidence,
    }
}

#[derive(Debug, Clone)]
struct LocalModelCatalogSpec {
    model_id: &'static str,
    display_name: &'static str,
    provider_key: &'static str,
    model_kind: &'static str,
    recommended_hardware: &'static str,
    download_size_hint: &'static str,
    source_kind: &'static str,
    repo_id: Option<&'static str>,
    revision: &'static str,
    file_policy: &'static str,
    runtime_profiles: &'static [&'static str],
    expected_capabilities: &'static [&'static str],
    acceptance_note: Option<&'static str>,
}

fn local_model_catalog_specs() -> Vec<LocalModelCatalogSpec> {
    vec![
        LocalModelCatalogSpec {
            model_id: "Qwen/Qwen3.5-4B",
            display_name: "Qwen3.5 4B",
            provider_key: "qwen",
            model_kind: "llm_vlm",
            recommended_hardware: "GPU 16GB+; primary 182 live-test target",
            download_size_hint: "10-15 GB",
            source_kind: "huggingface",
            repo_id: Some("Qwen/Qwen3.5-4B"),
            revision: "main",
            file_policy: "runtime_snapshot",
            runtime_profiles: &["vllm-openai-compatible", "sglang-openai-compatible"],
            expected_capabilities: &["llm", "vlm", "image_text_to_text"],
            acceptance_note: Some("primary-live-test"),
        },
        LocalModelCatalogSpec {
            model_id: "Qwen/Qwen3.5-9B",
            display_name: "Qwen3.5 9B",
            provider_key: "qwen",
            model_kind: "llm_vlm",
            recommended_hardware: "GPU 16GB+ only after 4B path is green",
            download_size_hint: "20-30 GB",
            source_kind: "huggingface",
            repo_id: Some("Qwen/Qwen3.5-9B"),
            revision: "main",
            file_policy: "runtime_snapshot",
            runtime_profiles: &["vllm-openai-compatible", "sglang-openai-compatible"],
            expected_capabilities: &["llm", "vlm", "image_text_to_text"],
            acceptance_note: Some("stretch-after-4b"),
        },
        LocalModelCatalogSpec {
            model_id: "Qwen/Qwen3.6-35B-A3B",
            display_name: "Qwen3.6 35B A3B",
            provider_key: "qwen",
            model_kind: "llm_vlm",
            recommended_hardware: "multi-GPU recommended; not a 182 16GB acceptance target",
            download_size_hint: "60+ GB",
            source_kind: "huggingface",
            repo_id: Some("Qwen/Qwen3.6-35B-A3B"),
            revision: "main",
            file_policy: "runtime_snapshot",
            runtime_profiles: &["vllm-openai-compatible", "sglang-openai-compatible"],
            expected_capabilities: &["llm", "vlm", "image_text_to_text"],
            acceptance_note: Some("not-today-acceptance"),
        },
        LocalModelCatalogSpec {
            model_id: "Qwen/Qwen3-Embedding-0.6B",
            display_name: "Qwen3 Embedding 0.6B",
            provider_key: "qwen",
            model_kind: "embedder",
            recommended_hardware: "CPU or GPU",
            download_size_hint: "1-2 GB",
            source_kind: "huggingface",
            repo_id: Some("Qwen/Qwen3-Embedding-0.6B"),
            revision: "main",
            file_policy: "runtime_snapshot",
            runtime_profiles: &["openai-compatible-embedding"],
            expected_capabilities: &["embedding"],
            acceptance_note: Some("retrieval-quality-upgrade"),
        },
        LocalModelCatalogSpec {
            model_id: "Qwen/Qwen2.5-0.5B-Instruct",
            display_name: "Qwen2.5 0.5B Bootstrap Instruct",
            provider_key: "qwen",
            model_kind: "llm",
            recommended_hardware: "CPU 4GB+; ISO bootstrap natural-language entry",
            download_size_hint: "500 MB-1 GB",
            source_kind: "huggingface",
            repo_id: Some("Qwen/Qwen2.5-0.5B-Instruct"),
            revision: "main",
            file_policy: "runtime_snapshot",
            runtime_profiles: &["harbor-candle", "harbor-model-api-candle"],
            expected_capabilities: &[
                "llm",
                "semantic_router",
                "assistant_input_parser",
                "setup_guidance",
            ],
            acceptance_note: Some("iso-bootstrap"),
        },
        LocalModelCatalogSpec {
            model_id: "qwen2.5-1.5b-instruct",
            display_name: "Qwen2.5 1.5B Instruct",
            provider_key: "qwen",
            model_kind: "llm",
            recommended_hardware: "CPU 8GB+",
            download_size_hint: "1-2 GB",
            source_kind: "huggingface",
            repo_id: Some("Qwen/Qwen2.5-1.5B-Instruct"),
            revision: "main",
            file_policy: "runtime_snapshot",
            runtime_profiles: &["harbor-candle", "harbor-model-api-candle"],
            expected_capabilities: &["llm"],
            acceptance_note: Some("legacy-catalog"),
        },
        LocalModelCatalogSpec {
            model_id: "jina-embeddings-v2-base-zh",
            display_name: "Jina Embeddings v2 zh",
            provider_key: "jina",
            model_kind: "embedder",
            recommended_hardware: "CPU 4GB+",
            download_size_hint: "300-700 MB",
            source_kind: "manual_or_url",
            repo_id: None,
            revision: "main",
            file_policy: "single_file_or_existing_cache",
            runtime_profiles: &["harbor-candle", "harbor-model-api-candle"],
            expected_capabilities: &["embedding"],
            acceptance_note: Some("legacy-catalog"),
        },
        LocalModelCatalogSpec {
            model_id: "bge-m3",
            display_name: "BGE M3 Embedding",
            provider_key: "bge",
            model_kind: "embedder",
            recommended_hardware: "CPU 4GB+",
            download_size_hint: "1-2 GB",
            source_kind: "huggingface",
            repo_id: Some("BAAI/bge-m3"),
            revision: "main",
            file_policy: "runtime_snapshot",
            runtime_profiles: &["openai-compatible-embedding"],
            expected_capabilities: &["embedding"],
            acceptance_note: Some("legacy-catalog"),
        },
        LocalModelCatalogSpec {
            model_id: "HuggingFaceTB/SmolVLM-256M-Instruct",
            display_name: "SmolVLM 256M Instruct",
            provider_key: "huggingfacetb",
            model_kind: "vlm",
            recommended_hardware:
                "CPU smoke VLM; slow but suitable for VM content-index validation",
            download_size_hint: "1-2 GB",
            source_kind: "huggingface",
            repo_id: Some("HuggingFaceTB/SmolVLM-256M-Instruct"),
            revision: "main",
            file_policy: "runtime_snapshot",
            runtime_profiles: &["harbor-vlm-sidecar", "openai-compatible-vlm"],
            expected_capabilities: &["vlm", "image_text_to_text"],
            acceptance_note: Some("vm-cpu-photo-rag"),
        },
        LocalModelCatalogSpec {
            model_id: "minicpm-v-2.6",
            display_name: "MiniCPM-V 2.6",
            provider_key: "minicpm",
            model_kind: "vlm",
            recommended_hardware: "GPU recommended",
            download_size_hint: "4-8 GB",
            source_kind: "manual_or_url",
            repo_id: None,
            revision: "main",
            file_policy: "single_file_or_existing_cache",
            runtime_profiles: &["openai-compatible-vlm"],
            expected_capabilities: &["vlm"],
            acceptance_note: Some("legacy-catalog"),
        },
    ]
}

fn local_model_catalog_item(
    cache_roots: &[String],
    download_jobs: &[ModelDownloadJobRecord],
    spec: LocalModelCatalogSpec,
) -> LocalModelCatalogItem {
    let hardware = build_hardware_readiness_response();
    local_model_catalog_item_with_hardware(cache_roots, download_jobs, spec, &hardware)
}

fn local_model_catalog_item_with_hardware(
    cache_roots: &[String],
    download_jobs: &[ModelDownloadJobRecord],
    spec: LocalModelCatalogSpec,
    hardware: &HardwareReadinessResponse,
) -> LocalModelCatalogItem {
    let latest_job = latest_model_download_job(download_jobs, spec.model_id);
    let candidate_path = find_cached_model_path(cache_roots, spec.model_id).or_else(|| {
        latest_job
            .and_then(|job| job.target_path.as_ref())
            .filter(|path| Path::new(path).exists())
            .cloned()
    });
    let candidate_size = candidate_path
        .as_ref()
        .and_then(|path| model_path_size(Path::new(path)).ok());
    let local_path = candidate_path
        .as_ref()
        .zip(candidate_size)
        .and_then(|(path, size)| (size > 0).then(|| path.clone()));
    let installed = local_path.is_some();
    let size_bytes = installed.then_some(candidate_size).flatten();
    let status = if installed {
        "ready"
    } else if let Some(job) = latest_job {
        match job.status.as_str() {
            "queued" | "running" | "downloading" => "running",
            "completed" | "failed" => "blocked",
            "canceled" | "cancelled" => "needs-config",
            _ => "needs-config",
        }
    } else {
        "needs-config"
    }
    .to_string();
    let installable = local_model_catalog_spec_is_installable(&spec);
    let manual_only = !installable;
    let mut evidence = vec![
        format!("model_id={}", spec.model_id),
        format!("source_kind={}", spec.source_kind),
        format!("file_policy={}", spec.file_policy),
        format!("installable={installable}"),
        format!("manual_only={manual_only}"),
    ];
    if let Some(repo_id) = spec.repo_id {
        evidence.push(format!("repo_id={repo_id}"));
        evidence.push(format!("revision={}", spec.revision));
    }
    if let Some(path) = local_path.as_ref() {
        evidence.push(format!("local_path={path}"));
    } else if let Some(path) = candidate_path.as_ref() {
        evidence.push(format!("ignored_incomplete_local_path={path}"));
        if let Some(size) = candidate_size {
            evidence.push(format!("ignored_incomplete_size_bytes={size}"));
        }
    }
    if let Some(job) = latest_job {
        evidence.push(format!("latest_download_job={}", job.job_id));
        evidence.push(format!("latest_download_status={}", job.status));
    }
    let recommendation = model_hardware_recommendation(&spec, hardware, installed);
    evidence.push(format!("hardware_fit={}", recommendation.hardware_fit));
    evidence.push(format!(
        "recommendation_group={}",
        recommendation.recommendation_group
    ));
    let detail = if installed {
        format!(
            "{} is installed at {}.",
            spec.display_name,
            local_path.clone().unwrap_or_default()
        )
    } else if let Some(job) = latest_job {
        format!(
            "{} latest download job is {} with status {}.",
            spec.display_name, job.job_id, job.status
        )
    } else if spec.source_kind == "huggingface" {
        format!(
            "Direct Hugging Face snapshot download is available for {}.",
            spec.repo_id.unwrap_or(spec.model_id)
        )
    } else {
        "Configure a source_url or install into the model cache root.".to_string()
    };
    LocalModelCatalogItem {
        model_id: spec.model_id.to_string(),
        label: spec.display_name.to_string(),
        display_name: spec.display_name.to_string(),
        provider: spec.provider_key.to_string(),
        provider_key: spec.provider_key.to_string(),
        model_kind: spec.model_kind.to_string(),
        recommended_hardware: spec.recommended_hardware.to_string(),
        status,
        installed,
        local_path,
        size_bytes,
        download_job_id: latest_job.map(|job| job.job_id.clone()),
        download_size_hint: spec.download_size_hint.to_string(),
        hardware_fit: recommendation.hardware_fit,
        fit_reason: recommendation.fit_reason,
        recommendation_group: recommendation.recommendation_group,
        detail,
        source_kind: spec.source_kind.to_string(),
        installable,
        manual_only,
        repo_id: spec.repo_id.map(str::to_string),
        revision: Some(spec.revision.to_string()),
        file_policy: spec.file_policy.to_string(),
        default_hf_endpoint: (spec.source_kind == "huggingface")
            .then(|| DEFAULT_HF_ENDPOINT.to_string()),
        runtime_profiles: spec
            .runtime_profiles
            .iter()
            .map(|item| item.to_string())
            .collect(),
        expected_capabilities: spec
            .expected_capabilities
            .iter()
            .map(|item| item.to_string())
            .collect(),
        acceptance_note: spec.acceptance_note.map(str::to_string),
        evidence,
    }
}

fn local_model_catalog_spec_is_installable(spec: &LocalModelCatalogSpec) -> bool {
    spec.source_kind == "huggingface" && spec.repo_id.is_some()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelHardwareRecommendation {
    hardware_fit: String,
    fit_reason: String,
    recommendation_group: String,
}

fn model_hardware_recommendation(
    spec: &LocalModelCatalogSpec,
    hardware: &HardwareReadinessResponse,
    installed: bool,
) -> ModelHardwareRecommendation {
    let class = hardware.hardware_class.as_str();
    let model_id = spec.model_id.to_ascii_lowercase();
    let recommended = |reason: &str, group: &str| ModelHardwareRecommendation {
        hardware_fit: "recommended".to_string(),
        fit_reason: reason.to_string(),
        recommendation_group: group.to_string(),
    };
    let compatible = |reason: &str, group: &str| ModelHardwareRecommendation {
        hardware_fit: "compatible".to_string(),
        fit_reason: reason.to_string(),
        recommendation_group: group.to_string(),
    };
    let not_recommended = |reason: &str| ModelHardwareRecommendation {
        hardware_fit: "not_recommended".to_string(),
        fit_reason: reason.to_string(),
        recommendation_group: if installed {
            "installed_not_recommended".to_string()
        } else {
            "high_end_experimental".to_string()
        },
    };

    if model_id.contains("35b") {
        return if class == "multi_gpu_or_remote" {
            recommended(
                "multi-GPU or remote class hardware detected.",
                "high_end_experimental",
            )
        } else {
            not_recommended("35B class models need multi-GPU or remote inference.")
        };
    }
    if model_id.contains("9b") {
        return if matches!(class, "gpu_24gb_plus" | "multi_gpu_or_remote") {
            recommended("24GB+ GPU class hardware detected.", "current_recommended")
        } else {
            not_recommended("9B class models are reserved for 24GB+ GPU hardware.")
        };
    }
    if model_id.contains("qwen3.5-4b") {
        return if matches!(class, "gpu_16gb" | "gpu_24gb_plus" | "multi_gpu_or_remote") {
            recommended("16GB+ GPU class hardware detected.", "current_recommended")
        } else {
            not_recommended("4B model is installed, but current hardware has no confirmed 16GB+ usable GPU memory.")
        };
    }
    if model_id.contains("minicpm") {
        return if matches!(class, "gpu_16gb" | "gpu_24gb_plus" | "multi_gpu_or_remote") {
            compatible("GPU class hardware detected.", "current_recommended")
        } else {
            not_recommended("MiniCPM-V should stay experimental without confirmed GPU memory.")
        };
    }
    if model_id.contains("smolvlm")
        || model_id.contains("0.5b")
        || model_id.contains("1.5b")
        || model_id.contains("embedding")
        || model_id.contains("bge")
        || model_id.contains("jina")
    {
        return recommended(
            "Lightweight local profile fits the current machine better than 4B+ models.",
            "lightweight_local",
        );
    }

    compatible(
        "No strict hardware rule matched this model.",
        "current_recommended",
    )
}

fn catalog_model_matches_kind(model: &LocalModelCatalogItem, kind: ModelKind) -> bool {
    let model_kind = model.model_kind.trim().to_ascii_lowercase();
    match kind {
        ModelKind::Llm => {
            model_kind == "llm"
                || model_kind == "llm_vlm"
                || model_kind == "llm+vlm"
                || model.expected_capabilities.iter().any(|capability| {
                    capability.eq_ignore_ascii_case("llm")
                        || capability.eq_ignore_ascii_case("text_generation")
                })
        }
        ModelKind::Vlm => {
            model_kind == "vlm"
                || model_kind == "llm_vlm"
                || model_kind == "llm+vlm"
                || model.expected_capabilities.iter().any(|capability| {
                    let normalized = capability.to_ascii_lowercase();
                    normalized.contains("vlm")
                        || normalized.contains("vision")
                        || normalized.contains("image")
                })
        }
        ModelKind::Embedder => {
            model_kind == "embedder"
                || model_kind == "embedding"
                || model.expected_capabilities.iter().any(|capability| {
                    let normalized = capability.to_ascii_lowercase();
                    normalized.contains("embed")
                })
        }
        _ => model_kind == kind.as_str(),
    }
}

fn catalog_model_matches_kind_or_capability(
    model: &LocalModelCatalogItem,
    kind: ModelKind,
    capability_id: &str,
) -> bool {
    catalog_model_matches_kind(model, kind)
        || model
            .expected_capabilities
            .iter()
            .any(|capability| capability.eq_ignore_ascii_case(capability_id))
}

fn selected_model_id_for_capability(
    model_state: &AdminModelCenterState,
    capability_id: &str,
) -> Option<String> {
    model_state
        .capability_bindings
        .iter()
        .find(|binding| binding.capability_id == capability_id)
        .map(|binding| binding.model_id.clone())
        .and_then(|model_id| non_empty_string(&model_id))
}

fn model_capability_installable_model(
    model: &LocalModelCatalogItem,
) -> ModelCapabilityInstallableModel {
    ModelCapabilityInstallableModel {
        model_id: model.model_id.clone(),
        display_name: model.display_name.clone(),
        provider_key: model.provider_key.clone(),
        model_kind: model.model_kind.clone(),
        status: model.status.clone(),
        installed: model.installed,
        local_path: model.local_path.clone(),
        download_job_id: model.download_job_id.clone(),
        download_size_hint: model.download_size_hint.clone(),
        hardware_fit: model.hardware_fit.clone(),
        fit_reason: model.fit_reason.clone(),
        recommendation_group: model.recommendation_group.clone(),
        source_kind: model.source_kind.clone(),
        repo_id: model.repo_id.clone(),
        file_policy: model.file_policy.clone(),
        runtime_profiles: model.runtime_profiles.clone(),
        expected_capabilities: model.expected_capabilities.clone(),
    }
}

fn preferred_endpoint_id_for_model_kind(kind: ModelKind) -> &'static str {
    match kind {
        ModelKind::Llm => "llm-local-openai-compatible",
        ModelKind::Vlm => "vlm-local-openai-compatible",
        ModelKind::Ocr => "ocr-local-tesseract",
        ModelKind::Asr => "asr-local",
        ModelKind::Detector => "detector-local",
        ModelKind::Embedder => "embed-local-openai-compatible",
        ModelKind::Reranker => "rerank-local-compatible",
    }
}

fn runtime_model_kind_for_capability(capability_id: &str) -> Option<ModelKind> {
    match capability_id.trim() {
        "semantic_router" | "retrieval_answer" => Some(ModelKind::Llm),
        "embedder" => Some(ModelKind::Embedder),
        _ => None,
    }
}

fn model_kind_for_capability(capability_id: &str) -> Option<ModelKind> {
    match capability_id.trim() {
        "semantic_router" | "retrieval_answer" => Some(ModelKind::Llm),
        "embedder" => Some(ModelKind::Embedder),
        "vlm" => Some(ModelKind::Vlm),
        "ocr" => Some(ModelKind::Ocr),
        "asr" => Some(ModelKind::Asr),
        _ => None,
    }
}

fn model_capability_runtime_ready(
    kind: ModelKind,
    runtime: &LocalModelRuntimeProjection,
    runtime_bound: bool,
) -> bool {
    if !runtime_bound {
        return false;
    }
    if !(runtime.ready && runtime.backend_ready) {
        return false;
    }
    match kind {
        ModelKind::Llm => runtime.chat_model.is_some(),
        ModelKind::Embedder => runtime.embedding_model.is_some(),
        _ => false,
    }
}

fn runtime_model_name_for_kind(
    kind: ModelKind,
    runtime: &LocalModelRuntimeProjection,
) -> Option<String> {
    match kind {
        ModelKind::Llm => runtime.chat_model.clone(),
        ModelKind::Embedder => runtime.embedding_model.clone(),
        _ => None,
    }
}

fn model_download_job_is_active(job: &ModelDownloadJobRecord) -> bool {
    matches!(
        job.status.as_str(),
        "queued" | "running" | "downloading" | "installing"
    )
}

fn latest_model_download_job<'a>(
    jobs: &'a [ModelDownloadJobRecord],
    model_id: &str,
) -> Option<&'a ModelDownloadJobRecord> {
    jobs.iter()
        .filter(|job| job.model_id == model_id)
        .max_by(|left, right| {
            if model_download_job_is_newer(left, right) {
                std::cmp::Ordering::Greater
            } else if model_download_job_is_newer(right, left) {
                std::cmp::Ordering::Less
            } else {
                std::cmp::Ordering::Equal
            }
        })
}

fn latest_model_download_jobs(jobs: &[ModelDownloadJobRecord]) -> Vec<ModelDownloadJobRecord> {
    let mut latest_jobs = latest_model_download_jobs_by_model(jobs)
        .values()
        .map(|job| (*job).clone())
        .collect::<Vec<_>>();
    latest_jobs.sort_by(|left, right| {
        if model_download_job_is_newer(left, right) {
            std::cmp::Ordering::Less
        } else if model_download_job_is_newer(right, left) {
            std::cmp::Ordering::Greater
        } else {
            left.job_id.cmp(&right.job_id)
        }
    });
    latest_jobs
}

fn enrich_model_download_metadata(metadata: &mut Value, item: &LocalModelCatalogItem) {
    if !metadata.is_object() {
        *metadata = json!({});
    }
    let Some(object) = metadata.as_object_mut() else {
        return;
    };
    object
        .entry("source_kind".to_string())
        .or_insert_with(|| json!(item.source_kind));
    if let Some(repo_id) = item.repo_id.as_ref() {
        object
            .entry("repo_id".to_string())
            .or_insert_with(|| json!(repo_id));
    }
    if let Some(revision) = item.revision.as_ref() {
        object
            .entry("revision".to_string())
            .or_insert_with(|| json!(revision));
    }
    object
        .entry("file_policy".to_string())
        .or_insert_with(|| json!(item.file_policy));
}

fn model_download_jobs_status(jobs: &[ModelDownloadJobRecord]) -> &'static str {
    let latest_jobs = latest_model_download_jobs_by_model(jobs);
    if latest_jobs
        .values()
        .any(|job| matches!(job.status.as_str(), "queued" | "running" | "downloading"))
    {
        return "running";
    }
    if latest_jobs.values().any(|job| job.status == "failed") {
        return "blocked";
    }
    "ready"
}

fn latest_model_download_jobs_by_model<'a>(
    jobs: &'a [ModelDownloadJobRecord],
) -> HashMap<&'a str, &'a ModelDownloadJobRecord> {
    let mut latest_jobs: HashMap<&str, &ModelDownloadJobRecord> = HashMap::new();
    for job in jobs {
        let key = if job.model_id.trim().is_empty() {
            job.job_id.as_str()
        } else {
            job.model_id.as_str()
        };
        match latest_jobs.get(key) {
            Some(current) if !model_download_job_is_newer(job, current) => {}
            _ => {
                latest_jobs.insert(key, job);
            }
        }
    }
    latest_jobs
}

fn model_download_job_is_newer(
    candidate: &ModelDownloadJobRecord,
    current: &ModelDownloadJobRecord,
) -> bool {
    let candidate_key = (
        model_download_job_timestamp(&candidate.updated_at),
        model_download_job_timestamp(&candidate.requested_at),
        candidate.job_id.as_str(),
    );
    let current_key = (
        model_download_job_timestamp(&current.updated_at),
        model_download_job_timestamp(&current.requested_at),
        current.job_id.as_str(),
    );
    candidate_key > current_key
}

fn model_download_job_timestamp(value: &str) -> u64 {
    value.trim().parse::<u64>().unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ModelDownloadTransferStats {
    bytes_written: u64,
    total_bytes: Option<u64>,
    file_count: Option<usize>,
    manifest_path: Option<String>,
    message: String,
}

const MODEL_DOWNLOAD_CANCELED: &str = "model download canceled";

fn spawn_model_download_worker(
    store: AdminConsoleStore,
    job: ModelDownloadJobRecord,
) -> Result<(), String> {
    thread::Builder::new()
        .name("harborbeacon-model-download".to_string())
        .spawn(move || run_model_download_job(store, job))
        .map(|_| ())
        .map_err(|error| format!("failed to spawn model download worker: {error}"))
}

fn mark_model_download_spawn_failed(
    store: &AdminConsoleStore,
    mut job: ModelDownloadJobRecord,
    error: String,
) -> ModelDownloadJobRecord {
    let now = now_unix_string();
    job.status = "failed".to_string();
    job.progress_percent = job.progress_percent.or(Some(0));
    job.completed_at = Some(now.clone());
    job.updated_at = now;
    job.error_message = Some(error.clone());
    job.message = format!("download worker failed to start: {error}");
    store.save_model_download_job(job.clone()).unwrap_or(job)
}

fn run_model_download_job(store: AdminConsoleStore, mut job: ModelDownloadJobRecord) {
    if model_download_job_cancel_requested(&store, &job.job_id) {
        mark_model_download_canceled(&store, job, "canceled_before_start");
        return;
    }

    let started_at = now_unix_string();
    job.status = "downloading".to_string();
    job.started_at = job.started_at.or_else(|| Some(started_at.clone()));
    job.updated_at = started_at;
    job.progress_percent = Some(0);
    job.error_message = None;
    job.message = "download job started by explicit admin action".to_string();
    if let Err(error) = store.save_model_download_job(job.clone()) {
        mark_model_download_failed(&store, job, "job_state_write_failed", error);
        return;
    }

    let result = run_model_download_transfer_with_progress(&mut job, Some(&store));
    if model_download_job_cancel_requested(&store, &job.job_id) {
        mark_model_download_canceled(&store, job, "canceled_after_transfer");
        return;
    }

    let finished_at = now_unix_string();
    match result {
        Ok(stats) => {
            job.status = "completed".to_string();
            job.progress_percent = Some(100);
            job.bytes_downloaded = Some(stats.bytes_written);
            job.total_bytes = stats.total_bytes.or(Some(stats.bytes_written));
            job.completed_at = Some(finished_at.clone());
            job.updated_at = finished_at;
            job.error_message = None;
            job.message = stats.message;
            merge_model_download_metadata(
                &mut job.metadata,
                json!({
                    "file_count": stats.file_count,
                    "snapshot_manifest_path": stats.manifest_path,
                }),
            );
            let _ = store.save_model_download_job(job);
        }
        Err(error) if error == MODEL_DOWNLOAD_CANCELED => {
            mark_model_download_canceled(&store, job, "canceled_during_transfer");
        }
        Err(error) => mark_model_download_failed(&store, job, "transfer_failed", error),
    }
}

fn mark_model_download_failed(
    store: &AdminConsoleStore,
    mut job: ModelDownloadJobRecord,
    phase: &str,
    error: String,
) {
    let now = now_unix_string();
    job.status = "failed".to_string();
    job.progress_percent = job.progress_percent.or(Some(0));
    job.completed_at = Some(now.clone());
    job.updated_at = now;
    job.error_message = Some(error.clone());
    job.message = format!("download job failed at {phase}: {error}");
    let _ = store.save_model_download_job(job);
}

fn mark_model_download_canceled(
    store: &AdminConsoleStore,
    mut job: ModelDownloadJobRecord,
    phase: &str,
) {
    let now = now_unix_string();
    job.status = "canceled".to_string();
    job.progress_percent = job.progress_percent.or(Some(0));
    job.completed_at = Some(now.clone());
    job.updated_at = now;
    job.error_message = None;
    job.message = format!("download job canceled: {phase}");
    let _ = store.save_model_download_job(job);
}

fn model_download_job_cancel_requested(store: &AdminConsoleStore, job_id: &str) -> bool {
    matches!(
        store
            .model_download_job(job_id)
            .ok()
            .flatten()
            .map(|job| job.status),
        Some(status) if status == "canceled" || status == "cancelled"
    )
}

fn save_model_download_checkpoint(
    store: Option<&AdminConsoleStore>,
    job_id: &str,
    progress_percent: Option<u8>,
    bytes_downloaded: Option<u64>,
    total_bytes: Option<u64>,
    message: impl Into<String>,
) {
    let Some(store) = store else {
        return;
    };
    let Ok(Some(mut job)) = store.model_download_job(job_id) else {
        return;
    };
    if matches!(
        job.status.as_str(),
        "canceled" | "cancelled" | "completed" | "failed"
    ) {
        return;
    }
    job.status = "downloading".to_string();
    job.progress_percent = progress_percent;
    job.bytes_downloaded = bytes_downloaded;
    if total_bytes.is_some() {
        job.total_bytes = total_bytes;
    }
    job.updated_at = now_unix_string();
    job.message = message.into();
    let _ = store.save_model_download_job(job);
}

fn run_model_download_transfer(
    job: &ModelDownloadJobRecord,
) -> Result<ModelDownloadTransferStats, String> {
    let mut job = job.clone();
    run_model_download_transfer_with_progress(&mut job, None)
}

fn run_model_download_transfer_with_progress(
    job: &mut ModelDownloadJobRecord,
    store: Option<&AdminConsoleStore>,
) -> Result<ModelDownloadTransferStats, String> {
    if model_download_huggingface_repo_id(job).is_some() {
        return run_huggingface_snapshot_download(job, store);
    }

    let target_path = job
        .target_path
        .as_deref()
        .and_then(non_empty_string)
        .unwrap_or_else(|| default_model_download_target_path(&job.model_id));
    let target = PathBuf::from(&target_path);
    if target.exists() {
        let bytes = model_path_size(&target)?;
        return Ok(ModelDownloadTransferStats {
            bytes_written: bytes,
            total_bytes: Some(bytes),
            file_count: None,
            manifest_path: None,
            message: format!("model already present at {}", target.display()),
        });
    }

    let source = model_download_source_url(&job.metadata).ok_or_else(|| {
        "download source_url is required for executable download; configure source_url or install the model out-of-band".to_string()
    })?;

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            format!(
                "failed to create target directory {}: {error}",
                parent.display()
            )
        })?;
    }

    if let Some(source_path) = model_download_source_file_path(&source) {
        fs::copy(&source_path, &target).map_err(|error| {
            format!(
                "failed to copy model from {} to {}: {error}",
                source_path.display(),
                target.display()
            )
        })?;
        let bytes = target
            .metadata()
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        return Ok(ModelDownloadTransferStats {
            bytes_written: bytes,
            total_bytes: Some(bytes),
            file_count: Some(1),
            manifest_path: None,
            message: format!("model copied to {}", target.display()),
        });
    }

    let client = Client::builder()
        .timeout(Duration::from_secs(60))
        .build()
        .map_err(|error| format!("failed to build model download client: {error}"))?;
    let mut response = client
        .get(&source)
        .send()
        .map_err(|error| format!("model download request failed: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "model download failed with HTTP {}",
            status.as_u16()
        ));
    }
    let total_bytes = response.content_length();
    let mut file = fs::File::create(&target).map_err(|error| {
        format!(
            "failed to create model target {}: {error}",
            target.display()
        )
    })?;
    let mut buffer = [0_u8; 64 * 1024];
    let mut bytes_written = 0_u64;
    loop {
        let read = response
            .read(&mut buffer)
            .map_err(|error| format!("model download stream failed: {error}"))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read]).map_err(|error| {
            format!("failed to write model target {}: {error}", target.display())
        })?;
        bytes_written += read as u64;
        if let Some(total) = total_bytes {
            let progress = ((bytes_written as f64 / total.max(1) as f64) * 100.0)
                .floor()
                .clamp(0.0, 99.0) as u8;
            save_model_download_checkpoint(
                store,
                &job.job_id,
                Some(progress),
                Some(bytes_written),
                Some(total),
                format!(
                    "downloading {} bytes to {}",
                    bytes_written,
                    target.display()
                ),
            );
        }
    }

    Ok(ModelDownloadTransferStats {
        bytes_written,
        total_bytes,
        file_count: Some(1),
        manifest_path: None,
        message: format!("model downloaded to {}", target.display()),
    })
}

fn run_huggingface_snapshot_download(
    job: &mut ModelDownloadJobRecord,
    store: Option<&AdminConsoleStore>,
) -> Result<ModelDownloadTransferStats, String> {
    if store
        .map(|store| model_download_job_cancel_requested(store, &job.job_id))
        .unwrap_or(false)
    {
        return Err(MODEL_DOWNLOAD_CANCELED.to_string());
    }

    let repo_id = model_download_huggingface_repo_id(job)
        .ok_or_else(|| "huggingface repo_id is required for snapshot download".to_string())?;
    let revision = model_download_metadata_string(&job.metadata, "revision")
        .unwrap_or_else(|| "main".to_string());
    let hf_endpoints = model_download_huggingface_endpoints(&job.metadata);
    let file_policy = model_download_metadata_string(&job.metadata, "file_policy")
        .unwrap_or_else(|| "runtime_snapshot".to_string());
    let allow_patterns = model_download_allow_patterns(&job.metadata);
    let target_path = job
        .target_path
        .as_deref()
        .and_then(non_empty_string)
        .unwrap_or_else(|| default_model_download_target_path(&job.model_id));
    let target = PathBuf::from(&target_path);
    let manifest_path = target.join("snapshot_manifest.json");
    if manifest_path.exists() {
        let bytes = model_path_size(&target)?;
        return Ok(ModelDownloadTransferStats {
            bytes_written: bytes,
            total_bytes: Some(bytes),
            file_count: None,
            manifest_path: Some(manifest_path.display().to_string()),
            message: format!(
                "huggingface snapshot already present at {}",
                target.display()
            ),
        });
    }

    fs::create_dir_all(&target).map_err(|error| {
        format!(
            "failed to create model snapshot directory {}: {error}",
            target.display()
        )
    })?;
    let hf_cache_dir = target.join(".hf-cache").join("hub");
    fs::create_dir_all(&hf_cache_dir).map_err(|error| {
        format!(
            "failed to create huggingface cache directory {}: {error}",
            hf_cache_dir.display()
        )
    })?;

    let mut endpoint_errors = Vec::new();
    let mut resolved_info = None;
    for hf_endpoint in &hf_endpoints {
        save_model_download_checkpoint(
            store,
            &job.job_id,
            Some(1),
            Some(0),
            None,
            format!("resolving Hugging Face snapshot {repo_id}@{revision} via {hf_endpoint}"),
        );
        let mut builder = HfApiBuilder::from_cache(HfCache::new(hf_cache_dir.clone()))
            .with_progress(false)
            .with_retries(3);
        if let Some(token) = huggingface_token_from_env() {
            builder = builder.with_token(Some(token));
        }
        builder = builder.with_endpoint(hf_endpoint.clone());
        let api = match builder.build() {
            Ok(api) => api,
            Err(error) => {
                endpoint_errors.push(format!("{hf_endpoint}: client init failed: {error}"));
                continue;
            }
        };
        let repo = api.repo(Repo::with_revision(
            repo_id.clone(),
            RepoType::Model,
            revision.clone(),
        ));
        match repo.info() {
            Ok(info) => {
                resolved_info = Some((hf_endpoint.clone(), info));
                break;
            }
            Err(error) => endpoint_errors.push(format!("{hf_endpoint}: {error}")),
        }
    }
    let (hf_endpoint, info) = resolved_info.ok_or_else(|| {
        format!(
            "failed to read Hugging Face repo info for {repo_id} from configured endpoints: {}",
            endpoint_errors.join("; ")
        )
    })?;
    let mut builder = HfApiBuilder::from_cache(HfCache::new(hf_cache_dir))
        .with_progress(false)
        .with_retries(3)
        .with_endpoint(hf_endpoint.clone());
    if let Some(token) = huggingface_token_from_env() {
        builder = builder.with_token(Some(token));
    }
    let api = builder
        .build()
        .map_err(|error| format!("failed to initialize Hugging Face client: {error}"))?;
    let repo = api.repo(Repo::with_revision(
        repo_id.clone(),
        RepoType::Model,
        revision.clone(),
    ));
    let resolved_sha = info.sha.clone();
    let mut files = info
        .siblings
        .into_iter()
        .map(|sibling| sibling.rfilename)
        .filter(|filename| model_snapshot_file_allowed(filename, &allow_patterns))
        .collect::<Vec<_>>();
    files.sort();
    if files.is_empty() {
        return Err(format!(
            "no downloadable files matched allow_patterns for Hugging Face repo {repo_id}"
        ));
    }

    let mut downloaded_files = Vec::new();
    let mut bytes_written = 0_u64;
    for (index, filename) in files.iter().enumerate() {
        if store
            .map(|store| model_download_job_cancel_requested(store, &job.job_id))
            .unwrap_or(false)
        {
            return Err(MODEL_DOWNLOAD_CANCELED.to_string());
        }
        let relative = safe_snapshot_relative_path(filename)?;
        let progress = HfModelDownloadProgress::new(
            store.cloned(),
            job.job_id.clone(),
            index,
            files.len(),
            bytes_written,
            filename.clone(),
        );
        let destination = target.join(&relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                format!(
                    "failed to create snapshot file directory {}: {error}",
                    parent.display()
                )
            })?;
        }
        let cached = match repo.download_with_progress(filename, progress) {
            Ok(cached) => Some(cached),
            Err(error) => {
                let error_message = error.to_string();
                if !huggingface_download_should_fallback_to_plain_http(&error_message)
                    && !huggingface_download_should_try_endpoint_fallback(&error_message)
                {
                    return Err(format!(
                        "failed to download Hugging Face file {filename} from {repo_id}: {error_message}"
                    ));
                }
                let mut fallback_errors = Vec::new();
                let mut fallback_ok = false;
                for fallback_endpoint in &hf_endpoints {
                    let fallback_progress = HfModelDownloadProgress::new(
                        store.cloned(),
                        job.job_id.clone(),
                        index,
                        files.len(),
                        bytes_written,
                        filename.clone(),
                    );
                    match download_huggingface_file_via_plain_http(
                        fallback_endpoint,
                        &repo_id,
                        &revision,
                        filename,
                        &destination,
                        fallback_progress,
                    ) {
                        Ok(()) => {
                            fallback_ok = true;
                            break;
                        }
                        Err(fallback_error) => {
                            fallback_errors.push(format!("{fallback_endpoint}: {fallback_error}"))
                        }
                    }
                }
                if !fallback_ok {
                    return Err(format!(
                        "failed to download Hugging Face file {filename} from {repo_id}: {error_message}; endpoint fallbacks failed: {}",
                        fallback_errors.join("; ")
                    ));
                }
                None
            }
        };
        if store
            .map(|store| model_download_job_cancel_requested(store, &job.job_id))
            .unwrap_or(false)
        {
            return Err(MODEL_DOWNLOAD_CANCELED.to_string());
        }
        if let Some(cached) = cached {
            fs::copy(&cached, &destination).map_err(|error| {
                format!(
                    "failed to copy Hugging Face cache file {} to {}: {error}",
                    cached.display(),
                    destination.display()
                )
            })?;
        }
        let bytes = destination
            .metadata()
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        bytes_written += bytes;
        downloaded_files.push(json!({
            "path": filename,
            "bytes": bytes,
        }));
        let progress_percent = (((index + 1) as f64 / files.len() as f64) * 100.0)
            .floor()
            .clamp(1.0, 99.0) as u8;
        save_model_download_checkpoint(
            store,
            &job.job_id,
            Some(progress_percent),
            Some(bytes_written),
            None,
            format!(
                "downloaded {}/{} Hugging Face snapshot files",
                index + 1,
                files.len()
            ),
        );
    }

    let manifest = json!({
        "source_kind": "huggingface",
        "hf_endpoint": hf_endpoint,
        "hf_endpoint_candidates": hf_endpoints,
        "repo_id": repo_id,
        "revision": revision,
        "resolved_sha": resolved_sha,
        "file_policy": file_policy,
        "allow_patterns": allow_patterns,
        "downloaded_at": now_unix_string(),
        "file_count": downloaded_files.len(),
        "bytes_written": bytes_written,
        "files": downloaded_files,
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("failed to serialize snapshot manifest: {error}"))?;
    fs::write(&manifest_path, manifest_bytes).map_err(|error| {
        format!(
            "failed to write snapshot manifest {}: {error}",
            manifest_path.display()
        )
    })?;
    let total_bytes = model_path_size(&target).ok();

    Ok(ModelDownloadTransferStats {
        bytes_written,
        total_bytes,
        file_count: Some(files.len()),
        manifest_path: Some(manifest_path.display().to_string()),
        message: format!(
            "huggingface snapshot downloaded to {} with manifest {}",
            target.display(),
            manifest_path.display()
        ),
    })
}

fn huggingface_download_should_fallback_to_plain_http(error_message: &str) -> bool {
    let normalized = error_message.to_ascii_lowercase();
    normalized.contains("header content-range is missing")
        || normalized.contains("header etag is missing")
}

fn huggingface_download_should_try_endpoint_fallback(error_message: &str) -> bool {
    let normalized = error_message.to_ascii_lowercase();
    normalized.contains("status code 404")
        || normalized.contains("status code 403")
        || normalized.contains("status code 429")
        || normalized.contains("status code 5")
        || normalized.contains("timed out")
        || normalized.contains("connection")
}

fn download_huggingface_file_via_plain_http(
    endpoint: &str,
    repo_id: &str,
    revision: &str,
    filename: &str,
    destination: &Path,
    mut progress: HfModelDownloadProgress,
) -> Result<(), String> {
    let url = huggingface_resolve_url(endpoint, repo_id, revision, filename)?;
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| format!("failed to build Hugging Face fallback client: {error}"))?;
    let mut request = client
        .get(url)
        .header("User-Agent", "HarborBeacon model downloader");
    if let Some(token) = huggingface_token_from_env() {
        request = request.bearer_auth(token);
    }
    let mut response = request
        .send()
        .map_err(|error| format!("plain HTTP request failed: {error}"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(format!(
            "plain HTTP request failed with HTTP {}",
            status.as_u16()
        ));
    }

    let total = response.content_length().unwrap_or(0);
    progress.init(total.try_into().unwrap_or(usize::MAX), filename);
    let partial = partial_snapshot_download_path(destination);
    let _ = fs::remove_file(&partial);
    let mut file = fs::File::create(&partial).map_err(|error| {
        format!(
            "failed to create temporary Hugging Face file {}: {error}",
            partial.display()
        )
    })?;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = response
            .read(&mut buffer)
            .map_err(|error| format!("plain HTTP stream failed: {error}"))?;
        if read == 0 {
            break;
        }
        file.write_all(&buffer[..read]).map_err(|error| {
            format!(
                "failed to write temporary Hugging Face file {}: {error}",
                partial.display()
            )
        })?;
        progress.update(read);
    }
    drop(file);
    fs::rename(&partial, destination).map_err(|error| {
        let _ = fs::remove_file(&partial);
        format!(
            "failed to move temporary Hugging Face file {} to {}: {error}",
            partial.display(),
            destination.display()
        )
    })?;
    progress.finish();
    Ok(())
}

fn huggingface_resolve_url(
    endpoint: &str,
    repo_id: &str,
    revision: &str,
    filename: &str,
) -> Result<Url, String> {
    let base = format!("{}/", endpoint.trim_end_matches('/'));
    let mut url = Url::parse(&base)
        .map_err(|error| format!("invalid Hugging Face endpoint {endpoint}: {error}"))?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|_| format!("invalid Hugging Face endpoint {endpoint}"))?;
        for segment in repo_id.split('/') {
            segments.push(segment);
        }
        segments.push("resolve");
        segments.push(revision);
        for segment in filename.split('/') {
            segments.push(segment);
        }
    }
    Ok(url)
}

fn partial_snapshot_download_path(destination: &Path) -> PathBuf {
    let mut partial = destination.to_path_buf();
    let file_name = destination
        .file_name()
        .map(|value| value.to_string_lossy())
        .unwrap_or_else(|| "download".into());
    partial.set_file_name(format!(".{file_name}.download"));
    partial
}

#[derive(Debug, Clone)]
struct HfModelDownloadProgress {
    store: Option<AdminConsoleStore>,
    job_id: String,
    file_index: usize,
    file_count: usize,
    completed_bytes: u64,
    filename: String,
    current_file_size: u64,
    current_file_bytes: u64,
    last_saved_bytes: u64,
    last_saved_percent: u8,
}

impl HfModelDownloadProgress {
    fn new(
        store: Option<AdminConsoleStore>,
        job_id: String,
        file_index: usize,
        file_count: usize,
        completed_bytes: u64,
        filename: String,
    ) -> Self {
        Self {
            store,
            job_id,
            file_index,
            file_count: file_count.max(1),
            completed_bytes,
            filename,
            current_file_size: 0,
            current_file_bytes: 0,
            last_saved_bytes: 0,
            last_saved_percent: 0,
        }
    }

    fn percent(&self) -> u8 {
        let file_fraction = if self.current_file_size > 0 {
            (self.current_file_bytes as f64 / self.current_file_size as f64).clamp(0.0, 1.0)
        } else {
            0.0
        };
        (((self.file_index as f64 + file_fraction) / self.file_count as f64) * 100.0)
            .floor()
            .clamp(1.0, 99.0) as u8
    }

    fn checkpoint(&mut self, force: bool) {
        let percent = self.percent();
        let bytes = self.completed_bytes + self.current_file_bytes;
        if !force
            && bytes.saturating_sub(self.last_saved_bytes) < 16 * 1024 * 1024
            && percent == self.last_saved_percent
        {
            return;
        }
        save_model_download_checkpoint(
            self.store.as_ref(),
            &self.job_id,
            Some(percent),
            Some(bytes),
            None,
            format!(
                "downloading {} ({}/{})",
                self.filename,
                self.file_index + 1,
                self.file_count
            ),
        );
        self.last_saved_bytes = bytes;
        self.last_saved_percent = percent;
    }
}

impl HfProgress for HfModelDownloadProgress {
    fn init(&mut self, size: usize, filename: &str) {
        self.current_file_size = size as u64;
        self.current_file_bytes = 0;
        self.filename = filename.to_string();
        self.checkpoint(true);
    }

    fn update(&mut self, size: usize) {
        self.current_file_bytes = self.current_file_bytes.saturating_add(size as u64);
        self.checkpoint(false);
    }

    fn finish(&mut self) {
        if self.current_file_size > 0 {
            self.current_file_bytes = self.current_file_size;
        }
        self.checkpoint(true);
    }
}

fn model_download_huggingface_repo_id(job: &ModelDownloadJobRecord) -> Option<String> {
    model_download_metadata_string(&job.metadata, "repo_id").or_else(|| {
        let source_kind = model_download_metadata_string(&job.metadata, "source_kind")
            .unwrap_or_default()
            .to_ascii_lowercase();
        (source_kind == "huggingface" && job.model_id.contains('/')).then(|| job.model_id.clone())
    })
}

fn model_download_huggingface_endpoint(metadata: &Value) -> String {
    model_download_huggingface_endpoints(metadata)
        .into_iter()
        .next()
        .unwrap_or_else(|| DEFAULT_HF_ENDPOINT.to_string())
}

fn model_download_huggingface_endpoints(metadata: &Value) -> Vec<String> {
    let mut endpoints = Vec::new();
    if let Some(items) = metadata.get("hf_endpoints").and_then(Value::as_array) {
        for item in items {
            if let Some(value) = item.as_str() {
                push_normalized_huggingface_endpoint(&mut endpoints, value);
            }
        }
    }
    if let Some(value) = model_download_metadata_string(metadata, "hf_endpoint") {
        push_normalized_huggingface_endpoint(&mut endpoints, &value);
    }
    if let Ok(value) = env::var("HF_ENDPOINTS") {
        for item in value.split([',', ';', '\n']) {
            push_normalized_huggingface_endpoint(&mut endpoints, item);
        }
    }
    if let Ok(value) = env::var("HF_ENDPOINT") {
        push_normalized_huggingface_endpoint(&mut endpoints, &value);
    }
    push_normalized_huggingface_endpoint(&mut endpoints, DEFAULT_HF_ENDPOINT);
    push_normalized_huggingface_endpoint(&mut endpoints, "https://huggingface.co");
    endpoints
}

fn normalize_huggingface_endpoint(value: &str) -> Option<String> {
    let normalized = value.trim().trim_end_matches('/').to_string();
    (!normalized.is_empty()).then_some(normalized)
}

fn push_normalized_huggingface_endpoint(endpoints: &mut Vec<String>, value: &str) {
    let Some(endpoint) = normalize_huggingface_endpoint(value) else {
        return;
    };
    if !endpoints.iter().any(|existing| existing == &endpoint) {
        endpoints.push(endpoint);
    }
}

fn model_download_metadata_string(metadata: &Value, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .and_then(non_empty_string)
}

fn model_download_allow_patterns(metadata: &Value) -> Vec<String> {
    let configured = metadata
        .get("allow_patterns")
        .and_then(|value| {
            if let Some(items) = value.as_array() {
                Some(
                    items
                        .iter()
                        .filter_map(Value::as_str)
                        .filter_map(non_empty_string)
                        .collect::<Vec<_>>(),
                )
            } else {
                value.as_str().map(|text| {
                    text.split(',')
                        .filter_map(non_empty_string)
                        .collect::<Vec<_>>()
                })
            }
        })
        .unwrap_or_default();
    if !configured.is_empty() {
        return configured;
    }
    [
        ".gitattributes",
        "*.json",
        "*.safetensors",
        "*.model",
        "*.txt",
        "*.tiktoken",
        "*.jinja",
        "*.md",
        "tokenizer*",
        "vocab.*",
        "merges.txt",
        "*.py",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn model_snapshot_file_allowed(filename: &str, allow_patterns: &[String]) -> bool {
    if safe_snapshot_relative_path(filename).is_err() {
        return false;
    }
    let normalized = filename.replace('\\', "/");
    let lower = normalized.to_ascii_lowercase();
    if lower.ends_with(".bin")
        || lower.ends_with(".pt")
        || lower.ends_with(".pth")
        || lower.ends_with(".ckpt")
        || lower.ends_with(".h5")
        || lower.ends_with(".onnx")
        || lower.ends_with(".pkl")
        || lower.ends_with(".pickle")
        || lower.ends_with(".msgpack")
    {
        return false;
    }
    allow_patterns
        .iter()
        .any(|pattern| wildcard_match(pattern, &normalized))
}

fn wildcard_match(pattern: &str, value: &str) -> bool {
    let pattern = pattern.as_bytes();
    let value = value.as_bytes();
    let (mut pattern_index, mut value_index) = (0_usize, 0_usize);
    let mut star_index = None;
    let mut retry_value_index = 0_usize;

    while value_index < value.len() {
        if pattern_index < pattern.len()
            && (pattern[pattern_index] == b'?' || pattern[pattern_index] == value[value_index])
        {
            pattern_index += 1;
            value_index += 1;
        } else if pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
            star_index = Some(pattern_index);
            retry_value_index = value_index;
            pattern_index += 1;
        } else if let Some(star) = star_index {
            pattern_index = star + 1;
            retry_value_index += 1;
            value_index = retry_value_index;
        } else {
            return false;
        }
    }

    while pattern_index < pattern.len() && pattern[pattern_index] == b'*' {
        pattern_index += 1;
    }
    pattern_index == pattern.len()
}

fn safe_snapshot_relative_path(filename: &str) -> Result<PathBuf, String> {
    let trimmed = filename.trim();
    if trimmed.is_empty() {
        return Err("snapshot filename is empty".to_string());
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(format!("snapshot filename must be relative: {filename}"));
    }
    for component in path.components() {
        if !matches!(component, std::path::Component::Normal(_)) {
            return Err(format!("unsafe snapshot filename component in {filename}"));
        }
    }
    Ok(path.to_path_buf())
}

fn huggingface_token_from_env() -> Option<String> {
    ["HF_TOKEN", "HUGGING_FACE_HUB_TOKEN"]
        .into_iter()
        .find_map(|key| {
            env::var(key)
                .ok()
                .and_then(|value| non_empty_string(&value))
        })
}

fn merge_model_download_metadata(metadata: &mut Value, patch: Value) {
    if !metadata.is_object() {
        *metadata = json!({});
    }
    let Some(object) = metadata.as_object_mut() else {
        return;
    };
    if let Some(patch_object) = patch.as_object() {
        for (key, value) in patch_object {
            if !value.is_null() {
                object.insert(key.clone(), value.clone());
            }
        }
    }
}

fn model_path_size(path: &Path) -> Result<u64, String> {
    let metadata = path
        .metadata()
        .map_err(|error| format!("failed to inspect model path {}: {error}", path.display()))?;
    if metadata.is_file() {
        return Ok(metadata.len());
    }
    if !metadata.is_dir() {
        return Ok(0);
    }
    let mut total = 0_u64;
    for entry in fs::read_dir(path)
        .map_err(|error| format!("failed to read model directory {}: {error}", path.display()))?
    {
        let entry = entry.map_err(|error| {
            format!(
                "failed to read model directory entry under {}: {error}",
                path.display()
            )
        })?;
        if entry.file_name().to_string_lossy() == ".hf-cache" {
            continue;
        }
        total = total.saturating_add(model_path_size(&entry.path())?);
    }
    Ok(total)
}

fn model_download_source_url(metadata: &Value) -> Option<String> {
    metadata
        .get("source_url")
        .and_then(Value::as_str)
        .and_then(non_empty_string)
        .or_else(|| {
            metadata
                .get("url")
                .and_then(Value::as_str)
                .and_then(non_empty_string)
        })
}

fn model_download_source_file_path(source: &str) -> Option<PathBuf> {
    let path = PathBuf::from(source);
    if path.exists() {
        return Some(path);
    }
    if let Ok(url) = Url::parse(source) {
        if url.scheme() == "file" {
            return url.to_file_path().ok();
        }
        return None;
    }
    None
}

fn default_model_download_target_path(model_id: &str) -> String {
    default_model_download_target_path_in_root(&default_model_store_root(), model_id)
}

fn default_model_download_target_path_for_model_state(
    model_state: &AdminModelCenterState,
    model_id: &str,
) -> String {
    default_model_download_target_path_in_root(&model_store_root(model_state), model_id)
}

fn default_model_download_target_path_in_root(root: &str, model_id: &str) -> String {
    if model_id.trim() == "Qwen/Qwen2.5-0.5B-Instruct" {
        return Path::new(root)
            .join("runtimes")
            .join("harbor-candle")
            .join("bootstrap-llm")
            .display()
            .to_string();
    }
    let slug = model_id
        .trim()
        .chars()
        .map(|ch| {
            if matches!(ch, '/' | '\\' | ':') {
                '-'
            } else {
                ch
            }
        })
        .collect::<String>()
        .to_ascii_lowercase();
    Path::new(root)
        .join(if slug.is_empty() {
            "model"
        } else {
            slug.as_str()
        })
        .display()
        .to_string()
}

fn local_model_cache_roots() -> Vec<String> {
    let mut roots = vec![default_model_store_root()];
    append_legacy_model_cache_roots(&mut roots);
    roots
}

fn local_model_cache_roots_for_model_state(model_state: &AdminModelCenterState) -> Vec<String> {
    let mut roots = vec![model_store_root(model_state)];
    append_legacy_model_cache_roots(&mut roots);
    roots
}

fn append_legacy_model_cache_roots(roots: &mut Vec<String>) {
    for key in [
        "HARBOR_MODEL_CACHE_DIR",
        "HARBOR_MODEL_DIR",
        "HARBOR_MODEL_STORE_DIR",
    ] {
        if let Ok(value) = env::var(key) {
            if let Some(value) = non_empty_string(&value) {
                push_unique_root(roots, value);
            }
        }
    }
    for root in [
        "/mnt/software/harborbeacon-models".to_string(),
        "/mnt/software/harborbeacon/models".to_string(),
        "/models".to_string(),
        ".harborbeacon/models".to_string(),
    ] {
        push_unique_root(roots, root);
    }
}

fn model_store_root(model_state: &AdminModelCenterState) -> String {
    non_empty_string(&model_state.model_store_root).unwrap_or_else(default_model_store_root)
}

fn build_model_store_status(model_state: &AdminModelCenterState) -> ModelStoreStatusResponse {
    let path = model_store_root(model_state);
    let store_path = Path::new(&path);
    let writable = path_can_accept_write(store_path);
    let runtime_readable = if store_path.exists() {
        fs::read_dir(store_path).is_ok()
    } else {
        writable
    };
    let mut blockers = Vec::new();
    let mut warnings = Vec::new();
    if !writable {
        blockers.push("模型保存位置不可写".to_string());
    }
    if !runtime_readable {
        blockers.push("模型服务无法读取该位置".to_string());
    }
    if !store_path.exists() {
        warnings.push("目录会在首次下载模型时自动创建".to_string());
    }
    let status = if !writable {
        "not_writable"
    } else if !runtime_readable {
        "runtime_unreadable"
    } else {
        "ready"
    }
    .to_string();
    let next_action = match status.as_str() {
        "ready" => "可用".to_string(),
        "not_writable" => "请选择 HarborOS 可写目录".to_string(),
        "runtime_unreadable" => "请选择本地模型服务可读取的目录".to_string(),
        _ => "需要检查模型保存位置".to_string(),
    };
    ModelStoreStatusResponse {
        path,
        status,
        writable,
        runtime_readable,
        next_action,
        blockers,
        warnings,
    }
}

fn push_unique_root(roots: &mut Vec<String>, root: String) {
    if !roots.iter().any(|existing| existing == &root) {
        roots.push(root);
    }
}

fn find_cached_model_path(cache_roots: &[String], model_id: &str) -> Option<String> {
    let slug = model_id.replace('/', "-").to_ascii_lowercase();
    for root in cache_roots {
        let runtime_default =
            PathBuf::from(default_model_download_target_path_in_root(root, model_id));
        if runtime_default.exists() {
            return Some(runtime_default.display().to_string());
        }
        let direct = Path::new(root).join(model_id);
        if direct.exists() {
            return Some(direct.display().to_string());
        }
        let slugged = Path::new(root).join(&slug);
        if slugged.exists() {
            return Some(slugged.display().to_string());
        }
    }
    None
}

fn public_origin_port(public_origin: &str) -> Option<u16> {
    Url::parse(public_origin).ok()?.port_or_known_default()
}

fn harboros_webui_url(public_origin: &str) -> String {
    if let Ok(url) = Url::parse(public_origin) {
        if let Some(host) = url.host_str() {
            return format!("{}://{host}/ui/", url.scheme());
        }
    }
    "http://192.168.3.182/ui/".to_string()
}

fn build_rtsp_url_from_patch(
    device: &CameraDevice,
    rtsp_path: Option<&str>,
    rtsp_port: Option<u16>,
) -> Result<String, String> {
    let host = device
        .ip_address
        .clone()
        .or_else(|| rtsp_host_from_url(&device.primary_stream.url))
        .ok_or_else(|| format!("device {} does not expose an RTSP host", device.device_id))?;
    let port = rtsp_port
        .filter(|port| *port > 0)
        .or_else(|| rtsp_port_from_url(&device.primary_stream.url))
        .unwrap_or(554);
    let path = rtsp_path
        .and_then(non_empty_string)
        .or_else(|| rtsp_path_from_url(&device.primary_stream.url))
        .unwrap_or_else(|| "/stream1".to_string());
    let path = if path.starts_with('/') {
        path
    } else {
        format!("/{path}")
    };
    Ok(format!("rtsp://{host}:{port}{path}"))
}

fn camera_stream_url_with_credentials(
    device: &CameraDevice,
    state: &AdminConsoleState,
) -> Option<String> {
    let credential = state
        .device_credentials
        .iter()
        .find(|credential| credential.device_id == device.device_id);
    let username = credential
        .and_then(|credential| non_empty_string(&credential.username))
        .or_else(|| non_empty_string(&state.defaults.rtsp_username));
    let password = credential
        .and_then(|credential| non_empty_string(&credential.password))
        .or_else(|| non_empty_string(&state.defaults.rtsp_password));
    if username.is_none() && password.is_none() {
        return None;
    }

    let host = device
        .ip_address
        .clone()
        .or_else(|| rtsp_host_from_url(&device.primary_stream.url))?;
    let port = credential
        .and_then(|credential| credential.rtsp_port)
        .or_else(|| rtsp_port_from_url(&device.primary_stream.url))
        .unwrap_or(state.defaults.rtsp_port);
    let path = credential
        .and_then(|credential| {
            credential
                .rtsp_paths
                .iter()
                .find_map(|path| non_empty_string(path))
        })
        .or_else(|| rtsp_path_from_url(&device.primary_stream.url))
        .or_else(|| {
            state
                .defaults
                .rtsp_paths
                .iter()
                .find_map(|path| non_empty_string(path))
        })
        .unwrap_or_else(|| "/stream1".to_string());
    let path = if path.starts_with('/') {
        path
    } else {
        format!("/{path}")
    };

    let mut url = Url::parse(&format!("rtsp://{host}:{port}{path}")).ok()?;
    if let Some(username) = username {
        let _ = url.set_username(&username);
    }
    if let Some(password) = password {
        let _ = url.set_password(Some(&password));
    }
    Some(url.to_string())
}

fn redact_secret_json_value(mut value: Value) -> Value {
    redact_secret_json_value_in_place(&mut value);
    value
}

fn redact_secret_json_value_in_place(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, value) in map.iter_mut() {
                if is_secret_key(key) {
                    let configured = value
                        .as_str()
                        .map(|text| !text.trim().is_empty())
                        .unwrap_or(!value.is_null());
                    *value = Value::String(String::new());
                    if configured {
                        // Keep callers able to show configured state without exposing material.
                    }
                } else {
                    redact_secret_json_value_in_place(value);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_secret_json_value_in_place(item);
            }
        }
        Value::String(text) => {
            *text = redact_admin_string(text);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn is_secret_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase();
    normalized.contains("password")
        || normalized.contains("secret")
        || normalized.contains("token")
        || normalized.contains("api_key")
        || normalized.contains("apikey")
}

fn redact_state_snapshot(mut state: StateResponse) -> StateResponse {
    state.defaults.rtsp_password.clear();
    state.binding.session_code.clear();
    state.binding.qr_token.clear();
    state.binding.setup_url.clear();
    state.binding.static_setup_url.clear();
    state.bridge_provider = redact_bridge_provider_config(state.bridge_provider);
    for device in &mut state.devices {
        redact_camera_device_projection(device);
    }
    let Ok(mut value) = serde_json::to_value(&state) else {
        return state;
    };
    redact_value_stream_credentials(&mut value);
    serde_json::from_value::<StateResponse>(value).unwrap_or(state)
}

fn redact_camera_device_projection(device: &mut CameraDevice) {
    device.primary_stream.url = redact_camera_primary_stream_url(&device.primary_stream.url);
    if let Some(snapshot_url) = device.snapshot_url.as_mut() {
        *snapshot_url = redact_stream_url_credentials(snapshot_url);
    }
    if let Some(onvif_url) = device.onvif_device_service_url.as_mut() {
        *onvif_url = redact_stream_url_credentials(onvif_url);
    }
}

fn redact_camera_primary_stream_url(value: &str) -> String {
    if Url::parse(value)
        .ok()
        .is_some_and(|url| url.scheme().eq_ignore_ascii_case("rtsp"))
    {
        "__harbor_redacted_rtsp_url__".to_string()
    } else {
        redact_stream_url_credentials(value)
    }
}

fn redact_camera_task_response(mut response: TaskResponse) -> TaskResponse {
    redact_value_stream_credentials(&mut response.result.data);
    for event in &mut response.result.events {
        redact_value_stream_credentials(event);
    }
    for artifact in &mut response.result.artifacts {
        if let Some(url) = artifact.url.as_mut() {
            *url = redact_stream_url_credentials(url);
        }
        redact_value_stream_credentials(&mut artifact.metadata);
    }
    response
}

fn redact_value_stream_credentials(value: &mut Value) {
    match value {
        Value::String(text) => {
            *text = redact_admin_string(text);
        }
        Value::Array(items) => {
            for item in items {
                redact_value_stream_credentials(item);
            }
        }
        Value::Object(map) => {
            for item in map.values_mut() {
                redact_value_stream_credentials(item);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn redact_account_management_snapshot(
    mut snapshot: AccountManagementSnapshot,
) -> AccountManagementSnapshot {
    snapshot.gateway = redact_gateway_status_summary(snapshot.gateway);
    snapshot
}

fn redact_gateway_status_summary(mut gateway: GatewayStatusSummary) -> GatewayStatusSummary {
    gateway.setup_url.clear();
    gateway.static_setup_url.clear();
    gateway.bridge_provider = redact_bridge_provider_config(gateway.bridge_provider);
    gateway
}

fn apply_bridge_provider_binding_projection(
    status: &mut String,
    metric: &mut String,
    bound_user: &mut Option<String>,
    provider: &BridgeProviderConfig,
) {
    if provider.connected {
        *status = "Gateway 已连接".to_string();
        *metric = "Gateway 在线".to_string();
    } else if provider.configured {
        *status = "Gateway 已启用".to_string();
        *metric = "Gateway 未连通".to_string();
    } else {
        *status = "等待 Gateway".to_string();
        *metric = "Gateway 未配置".to_string();
    }

    *bound_user = if !provider.app_name.trim().is_empty() {
        Some(provider.app_name.clone())
    } else if !provider.platform.trim().is_empty() {
        Some(format!("{} gateway", provider.platform))
    } else {
        None
    };
}

fn apply_bridge_provider_projection_to_state(
    state: &mut StateResponse,
    provider: &BridgeProviderConfig,
) {
    state.bridge_provider = provider.clone();
    apply_bridge_provider_binding_projection(
        &mut state.binding.status,
        &mut state.binding.metric,
        &mut state.binding.bound_user,
        provider,
    );
}

fn apply_bridge_provider_projection_to_gateway_summary(
    gateway: &mut GatewayStatusSummary,
    provider: &BridgeProviderConfig,
) {
    gateway.bridge_provider = provider.clone();
    apply_bridge_provider_binding_projection(
        &mut gateway.binding_status,
        &mut gateway.binding_metric,
        &mut gateway.binding_bound_user,
        provider,
    );
}

fn bridge_provider_config_from_platforms(
    gateway_base_url: &str,
    platforms: &[GatewayPlatformStatus],
) -> BridgeProviderConfig {
    let selected = platforms
        .iter()
        .find(|platform| platform.connected)
        .or_else(|| platforms.iter().find(|platform| platform.enabled))
        .or_else(|| platforms.first());
    let mut provider = BridgeProviderConfig {
        gateway_base_url: gateway_base_url.trim().to_string(),
        ..Default::default()
    };
    let Some(selected) = selected else {
        provider.status = "HarborGate 未配置平台".to_string();
        return provider;
    };

    provider.configured = selected.enabled;
    provider.connected = selected.connected;
    provider.platform = selected.platform.trim().to_string();
    provider.app_name = selected.display_name.trim().to_string();
    provider.status = if selected.connected {
        "已连接".to_string()
    } else if selected.enabled {
        "已启用，待连接".to_string()
    } else {
        "未启用".to_string()
    };
    provider.capabilities.reply = selected.capabilities.reply;
    provider.capabilities.update = selected.capabilities.update;
    provider.capabilities.attachments = selected.capabilities.attachments;
    provider
}

fn env_var_with_legacy_alias(primary: &str, legacy: &str) -> Option<String> {
    if let Ok(value) = env::var(primary) {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    if let Ok(value) = env::var(legacy) {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    None
}

fn gateway_status_endpoint(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.ends_with("/api/gateway/status") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/api/gateway/status")
    }
}

fn live_bridge_provider_from_setup_status(payload: &Value) -> Option<BridgeProviderConfig> {
    const PRIMARY_BASE_URL: &str = "HARBORGATE_BASE_URL";
    const LEGACY_BASE_URL: &str = "HARBOR_IM_GATEWAY_BASE_URL";

    let channels = payload
        .get("channels")
        .cloned()
        .or_else(|| payload.pointer("/gateway_status/channels").cloned())?;
    let platforms: Vec<GatewayPlatformStatus> = serde_json::from_value(channels).ok()?;
    if platforms.is_empty() {
        return None;
    }

    let gateway_base_url = env_var_with_legacy_alias(PRIMARY_BASE_URL, LEGACY_BASE_URL)
        .or_else(|| {
            payload
                .get("public_origin")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
        })
        .unwrap_or_default();
    Some(bridge_provider_config_from_platforms(
        &gateway_base_url,
        &platforms,
    ))
}

fn fetch_remote_gateway_status() -> Result<Value, String> {
    const PRIMARY_BASE_URL: &str = "HARBORGATE_BASE_URL";
    const LEGACY_BASE_URL: &str = "HARBOR_IM_GATEWAY_BASE_URL";
    const PRIMARY_TOKEN: &str = "HARBORGATE_BEARER_TOKEN";
    const LEGACY_TOKEN: &str = "HARBOR_IM_GATEWAY_BEARER_TOKEN";

    let base_url = env_var_with_legacy_alias(PRIMARY_BASE_URL, LEGACY_BASE_URL)
        .ok_or_else(|| format!("missing required env var {PRIMARY_BASE_URL}"))?;
    let endpoint = gateway_status_endpoint(&base_url);
    let client = Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|error| format!("failed to build HarborGate status client: {error}"))?;

    let mut request = client.get(endpoint).header("X-Contract-Version", "2.0");
    if let Some(token) = env_var_with_legacy_alias(PRIMARY_TOKEN, LEGACY_TOKEN) {
        request = request.header("Authorization", format!("Bearer {token}"));
    }

    let response = request
        .send()
        .map_err(|error| format!("HarborGate status request failed: {error}"))?;
    let status = response.status();
    let body = response
        .text()
        .map_err(|error| format!("failed to read HarborGate status response: {error}"))?;
    if !status.is_success() {
        return Err(format!(
            "HarborGate status request failed with HTTP {}: {}",
            status.as_u16(),
            body
        ));
    }

    serde_json::from_str(&body)
        .map_err(|error| format!("failed to parse HarborGate status response: {error}"))
}

fn probe_local_model_runtime(endpoints: &[ModelEndpoint]) -> LocalModelRuntimeProjection {
    let builtin_defaults = default_model_endpoints();
    let preferred = endpoints
        .iter()
        .find(|endpoint| is_builtin_local_openai_endpoint(endpoint))
        .cloned()
        .or_else(|| {
            builtin_defaults
                .iter()
                .find(|endpoint| is_builtin_local_openai_endpoint(endpoint))
                .cloned()
        });
    let Some(template) = preferred else {
        return LocalModelRuntimeProjection {
            error: Some("local OpenAI-compatible runtime is not configured".to_string()),
            ..Default::default()
        };
    };
    let fallback = builtin_defaults
        .iter()
        .find(|endpoint| endpoint.model_endpoint_id == template.model_endpoint_id)
        .or_else(|| {
            builtin_defaults
                .iter()
                .find(|endpoint| is_builtin_local_openai_endpoint(endpoint))
        });

    let template_is_builtin = is_builtin_local_openai_endpoint(&template);
    let raw_base_url = metadata_string_value(&template.metadata, "base_url");
    let fallback_base_url =
        fallback.and_then(|endpoint| metadata_string_value(&endpoint.metadata, "base_url"));
    let base_url = raw_base_url
        .filter(|value| !(template_is_builtin && is_legacy_model_api_url(value)))
        .or(fallback_base_url)
        .unwrap_or_default();
    let raw_healthz_url = metadata_string_value(&template.metadata, "healthz_url");
    let fallback_healthz_url =
        fallback.and_then(|endpoint| metadata_string_value(&endpoint.metadata, "healthz_url"));
    let healthz_url = raw_healthz_url
        .filter(|value| !(template_is_builtin && is_legacy_model_api_url(value)))
        .or(fallback_healthz_url)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| infer_healthz_url(&base_url));
    let api_key_configured = metadata_bool_value(&template.metadata, "api_key_configured")
        || metadata_string_value(&template.metadata, "api_key")
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false);
    let api_key_configured = api_key_configured
        || fallback
            .map(|endpoint| {
                metadata_bool_value(&endpoint.metadata, "api_key_configured")
                    || metadata_string_value(&endpoint.metadata, "api_key")
                        .map(|value| !value.trim().is_empty())
                        .unwrap_or(false)
            })
            .unwrap_or(false);

    if healthz_url.trim().is_empty() {
        return LocalModelRuntimeProjection {
            base_url,
            healthz_url,
            api_key_configured,
            error: Some("local model healthz URL is not configured".to_string()),
            ..Default::default()
        };
    }

    let client = match Client::builder().timeout(Duration::from_secs(3)).build() {
        Ok(client) => client,
        Err(error) => {
            return LocalModelRuntimeProjection {
                base_url,
                healthz_url,
                api_key_configured,
                error: Some(format!(
                    "failed to build local runtime probe client: {error}"
                )),
                ..Default::default()
            }
        }
    };

    let response = match client.get(&healthz_url).send() {
        Ok(response) => response,
        Err(error) => {
            return LocalModelRuntimeProjection {
                base_url,
                healthz_url,
                api_key_configured,
                error: Some(format!("local model healthz request failed: {error}")),
                ..Default::default()
            }
        }
    };
    let body = match response.text() {
        Ok(body) => body,
        Err(error) => {
            return LocalModelRuntimeProjection {
                base_url,
                healthz_url,
                api_key_configured,
                error: Some(format!(
                    "failed to read local model healthz response: {error}"
                )),
                ..Default::default()
            }
        }
    };
    let payload = match serde_json::from_str::<Value>(&body) {
        Ok(payload) => payload,
        Err(error) => {
            return LocalModelRuntimeProjection {
                base_url,
                healthz_url,
                api_key_configured,
                error: Some(format!(
                    "local model healthz returned invalid JSON: {error}"
                )),
                ..Default::default()
            }
        }
    };

    LocalModelRuntimeProjection {
        base_url,
        healthz_url,
        api_key_configured,
        ready: payload
            .get("ready")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        backend_ready: payload
            .get("backend")
            .and_then(|value| value.get("ready"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
        backend_kind: payload
            .get("backend")
            .and_then(|value| value.get("kind"))
            .and_then(Value::as_str)
            .map(str::to_string),
        chat_model: payload
            .get("chat_model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        embedding_model: payload
            .get("embedding_model")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
        note: payload
            .get("note")
            .and_then(Value::as_str)
            .map(str::to_string),
        error: None,
    }
}

fn overlay_model_endpoints_with_runtime_truth(
    endpoints: &[ModelEndpoint],
    runtime: &LocalModelRuntimeProjection,
) -> Vec<ModelEndpoint> {
    let builtin_defaults = default_model_endpoints()
        .into_iter()
        .map(|endpoint| (endpoint.model_endpoint_id.clone(), endpoint))
        .collect::<HashMap<_, _>>();

    endpoints
        .iter()
        .map(|endpoint| {
            let mut overlayed = endpoint.clone();
            let original_status = overlayed.status;
            let mut projection_mismatch = false;

            if let Some(default_endpoint) = builtin_defaults.get(&overlayed.model_endpoint_id) {
                if is_builtin_local_openai_endpoint(default_endpoint) {
                    let legacy_base_url = metadata_string_value(&overlayed.metadata, "base_url")
                        .is_some_and(|value| is_legacy_model_api_url(&value));
                    if metadata_missing_or_empty(&overlayed.metadata, "base_url") || legacy_base_url
                    {
                        if let Some(base_url) =
                            metadata_string_value(&default_endpoint.metadata, "base_url")
                        {
                            set_metadata_string(&mut overlayed.metadata, "base_url", base_url);
                            projection_mismatch = true;
                        }
                        if legacy_base_url {
                            set_metadata_bool(
                                &mut overlayed.metadata,
                                "legacy_model_api_migrated",
                                true,
                            );
                        }
                    }
                    let legacy_healthz_url =
                        metadata_string_value(&overlayed.metadata, "healthz_url")
                            .is_some_and(|value| is_legacy_model_api_url(&value));
                    if metadata_missing_or_empty(&overlayed.metadata, "healthz_url")
                        || legacy_healthz_url
                    {
                        if let Some(healthz_url) =
                            metadata_string_value(&default_endpoint.metadata, "healthz_url")
                        {
                            set_metadata_string(
                                &mut overlayed.metadata,
                                "healthz_url",
                                healthz_url,
                            );
                            projection_mismatch = true;
                        }
                        if legacy_healthz_url {
                            set_metadata_bool(
                                &mut overlayed.metadata,
                                "legacy_model_api_migrated",
                                true,
                            );
                        }
                    }
                    if metadata_missing_or_empty(&overlayed.metadata, "api_key") {
                        if let Some(api_key) =
                            metadata_string_value(&default_endpoint.metadata, "api_key")
                        {
                            set_metadata_string(&mut overlayed.metadata, "api_key", api_key);
                            projection_mismatch = true;
                        }
                    }
                    if !metadata_bool_value(&overlayed.metadata, "api_key_configured")
                        && metadata_bool_value(&default_endpoint.metadata, "api_key_configured")
                    {
                        set_metadata_bool(&mut overlayed.metadata, "api_key_configured", true);
                        projection_mismatch = true;
                    }

                    set_metadata_string(
                        &mut overlayed.metadata,
                        "projection_source",
                        "local_runtime_overlay".to_string(),
                    );
                    set_metadata_bool(
                        &mut overlayed.metadata,
                        "runtime_ready",
                        runtime.ready && runtime.backend_ready,
                    );
                    if let Some(kind) = runtime.backend_kind.as_ref() {
                        set_metadata_string(
                            &mut overlayed.metadata,
                            "runtime_backend_kind",
                            kind.clone(),
                        );
                    }
                    if let Some(chat_model) = runtime.chat_model.as_ref() {
                        set_metadata_string(
                            &mut overlayed.metadata,
                            "runtime_chat_model",
                            chat_model.clone(),
                        );
                    }
                    if let Some(embedding_model) = runtime.embedding_model.as_ref() {
                        set_metadata_string(
                            &mut overlayed.metadata,
                            "runtime_embedding_model",
                            embedding_model.clone(),
                        );
                    }
                    if let Some(note) = runtime.note.as_ref() {
                        set_metadata_string(&mut overlayed.metadata, "runtime_note", note.clone());
                    }
                    if let Some(error) = runtime.error.as_ref() {
                        set_metadata_string(
                            &mut overlayed.metadata,
                            "runtime_error",
                            error.clone(),
                        );
                    }

                    if matches!(overlayed.model_kind, ModelKind::Llm | ModelKind::Embedder) {
                        let runtime_model_available = match overlayed.model_kind {
                            ModelKind::Llm => runtime.chat_model.is_some(),
                            ModelKind::Embedder => runtime.embedding_model.is_some(),
                            _ => false,
                        };
                        if runtime.ready && runtime.backend_ready && runtime_model_available {
                            overlayed.status = ModelEndpointStatus::Active;
                        } else if overlayed.status == ModelEndpointStatus::Active {
                            overlayed.status = ModelEndpointStatus::Degraded;
                        }
                    }
                }
            }

            if overlayed.status != original_status {
                projection_mismatch = true;
            }
            if projection_mismatch {
                set_metadata_bool(&mut overlayed.metadata, "projection_mismatch", true);
                set_metadata_string(
                    &mut overlayed.metadata,
                    "projection_mismatch_reason",
                    "runtime truth overrode stale admin endpoint state".to_string(),
                );
            }

            overlayed
        })
        .collect()
}

fn build_feature_availability_response(
    endpoints: &[ModelEndpoint],
    route_policies: &[ModelRoutePolicy],
    account_management: &AccountManagementSnapshot,
    gateway_status: Option<&Value>,
    runtime: &LocalModelRuntimeProjection,
) -> FeatureAvailabilityResponse {
    let retrieval_group = FeatureAvailabilityGroup {
        group_id: "retrieval".to_string(),
        label: "Retrieval & Models".to_string(),
        items: vec![
            build_ocr_feature(endpoints, route_policies),
            build_embed_feature(endpoints, route_policies, runtime),
            build_answer_feature(endpoints, route_policies, runtime),
            build_vision_summary_feature(endpoints, route_policies),
        ],
    };
    let delivery_group = FeatureAvailabilityGroup {
        group_id: "delivery".to_string(),
        label: "Interaction & Delivery".to_string(),
        items: vec![
            build_interactive_reply_feature(account_management),
            build_proactive_delivery_feature(account_management, gateway_status),
        ],
    };
    let binding_group = FeatureAvailabilityGroup {
        group_id: "binding".to_string(),
        label: "Binding & Access".to_string(),
        items: vec![build_binding_availability_feature(
            account_management,
            gateway_status,
        )],
    };

    FeatureAvailabilityResponse {
        groups: vec![retrieval_group, delivery_group, binding_group],
    }
}

fn build_ocr_feature(
    endpoints: &[ModelEndpoint],
    route_policies: &[ModelRoutePolicy],
) -> FeatureAvailabilityItem {
    let policy = find_route_policy(route_policies, "retrieval.ocr");
    let endpoint = select_model_endpoint(endpoints, "ocr-local-tesseract", ModelKind::Ocr);
    let fallback_order = policy_fallback_order(policy);
    let endpoint_status = endpoint
        .map(|value| value.status.as_str().to_string())
        .unwrap_or_else(|| "missing".to_string());
    let status = match endpoint {
        Some(value) if value.status == ModelEndpointStatus::Active => "available",
        Some(_) => "degraded",
        None => "not_configured",
    };
    let blocker = if status == "available" {
        String::new()
    } else if endpoint.is_none() {
        "No OCR endpoint is configured.".to_string()
    } else {
        "OCR route is present, but the local tesseract path still needs verification.".to_string()
    };

    FeatureAvailabilityItem {
        feature_id: "retrieval.ocr".to_string(),
        label: "OCR extraction".to_string(),
        owner_lane: "harbor-framework".to_string(),
        status: status.to_string(),
        source_of_truth: "route_policy + model_endpoint".to_string(),
        current_option: endpoint
            .map(|value| format!("{} / {}", value.model_endpoint_id, value.provider_key))
            .unwrap_or_else(|| "unconfigured".to_string()),
        fallback_order,
        blocker,
        evidence: vec![
            format!("route_policy_status={}", policy_status_value(policy)),
            format!("endpoint_status={endpoint_status}"),
            format!(
                "provider={}",
                endpoint
                    .map(|value| value.provider_key.clone())
                    .unwrap_or_else(|| "none".to_string())
            ),
        ],
    }
}

fn build_embed_feature(
    endpoints: &[ModelEndpoint],
    route_policies: &[ModelRoutePolicy],
    runtime: &LocalModelRuntimeProjection,
) -> FeatureAvailabilityItem {
    let policy = find_route_policy(route_policies, "retrieval.embed");
    let endpoint = select_model_endpoint(
        endpoints,
        "embed-local-openai-compatible",
        ModelKind::Embedder,
    );
    let runtime_ready = runtime.ready && runtime.backend_ready;
    let projection_mismatch = endpoint.is_some_and(has_projection_mismatch);
    let status = if runtime_ready {
        "available"
    } else if endpoint.is_some() {
        "degraded"
    } else {
        "not_configured"
    };
    let blocker = if runtime_ready {
        String::new()
    } else if let Some(error) = runtime.error.as_ref() {
        error.clone()
    } else if runtime.ready && runtime.backend_ready {
        "Local embeddings runtime is ready, but no embedding model is installed or selected."
            .to_string()
    } else {
        "Local embeddings runtime is not ready.".to_string()
    };

    let mut evidence = vec![
        format!("route_policy_status={}", policy_status_value(policy)),
        format!("runtime_ready={runtime_ready}"),
    ];
    if let Some(kind) = runtime.backend_kind.as_ref() {
        evidence.push(format!("local_inference.backend.kind={kind}"));
    }
    if let Some(model) = runtime.embedding_model.as_ref() {
        evidence.push(format!("embedding_model={model}"));
    }
    if let Some(endpoint) = endpoint {
        evidence.push(format!(
            "endpoint={} status={}",
            endpoint.model_endpoint_id,
            endpoint.status.as_str()
        ));
        if projection_mismatch {
            evidence.push("projection_mismatch=runtime_overrode_stale_admin_state".to_string());
        }
    }

    FeatureAvailabilityItem {
        feature_id: "retrieval.embed".to_string(),
        label: "Embedding retrieval".to_string(),
        owner_lane: "harbor-framework".to_string(),
        status: status.to_string(),
        source_of_truth: "local inference /healthz + route_policy".to_string(),
        current_option: endpoint
            .map(|value| {
                format!(
                    "{} / {}",
                    value.model_endpoint_id,
                    runtime
                        .backend_kind
                        .clone()
                        .unwrap_or_else(|| value.provider_key.clone())
                )
            })
            .unwrap_or_else(|| {
                runtime
                    .backend_kind
                    .as_deref()
                    .map(|kind| format!("local runtime / {kind}"))
                    .unwrap_or_else(|| "unconfigured".to_string())
            }),
        fallback_order: policy_fallback_order(policy),
        blocker,
        evidence,
    }
}

fn build_answer_feature(
    endpoints: &[ModelEndpoint],
    route_policies: &[ModelRoutePolicy],
    runtime: &LocalModelRuntimeProjection,
) -> FeatureAvailabilityItem {
    let policy = find_route_policy(route_policies, "retrieval.answer");
    let endpoint = select_model_endpoint(endpoints, "llm-local-openai-compatible", ModelKind::Llm);
    let runtime_ready = runtime.ready
        && runtime.backend_ready
        && runtime
            .chat_model
            .as_ref()
            .is_some_and(|model| !model.trim().is_empty());
    let projection_mismatch = endpoint.is_some_and(has_projection_mismatch);
    let status = if runtime_ready {
        "available"
    } else if endpoint.is_some() {
        "degraded"
    } else {
        "not_configured"
    };
    let blocker = if runtime_ready {
        String::new()
    } else if let Some(error) = runtime.error.as_ref() {
        error.clone()
    } else if runtime.ready && runtime.backend_ready {
        "Local answer runtime is ready, but no chat model is installed or selected.".to_string()
    } else {
        "Local answer runtime is not ready.".to_string()
    };

    let mut evidence = vec![
        format!("route_policy_status={}", policy_status_value(policy)),
        format!("runtime_ready={runtime_ready}"),
    ];
    if let Some(kind) = runtime.backend_kind.as_ref() {
        evidence.push(format!("local_inference.backend.kind={kind}"));
    }
    if let Some(model) = runtime.chat_model.as_ref() {
        evidence.push(format!("chat_model={model}"));
    }
    if let Some(endpoint) = endpoint {
        evidence.push(format!(
            "endpoint={} status={}",
            endpoint.model_endpoint_id,
            endpoint.status.as_str()
        ));
        if projection_mismatch {
            evidence.push("projection_mismatch=runtime_overrode_stale_admin_state".to_string());
        }
    }

    FeatureAvailabilityItem {
        feature_id: "retrieval.answer".to_string(),
        label: "Retrieval answer synthesis".to_string(),
        owner_lane: "harbor-framework".to_string(),
        status: status.to_string(),
        source_of_truth: "local inference /healthz + route_policy".to_string(),
        current_option: endpoint
            .map(|value| {
                format!(
                    "{} / {}",
                    value.model_endpoint_id,
                    runtime
                        .backend_kind
                        .clone()
                        .unwrap_or_else(|| value.provider_key.clone())
                )
            })
            .unwrap_or_else(|| {
                runtime
                    .backend_kind
                    .as_deref()
                    .map(|kind| format!("local runtime / {kind}"))
                    .unwrap_or_else(|| "unconfigured".to_string())
            }),
        fallback_order: policy_fallback_order(policy),
        blocker,
        evidence,
    }
}

fn build_vision_summary_feature(
    endpoints: &[ModelEndpoint],
    route_policies: &[ModelRoutePolicy],
) -> FeatureAvailabilityItem {
    let policy = find_route_policy(route_policies, "retrieval.vision_summary");
    let endpoint = select_model_endpoint(endpoints, "vlm-local-openai-compatible", ModelKind::Vlm);
    let status = match endpoint {
        Some(value) if value.status == ModelEndpointStatus::Active => "available",
        Some(value) if value.status == ModelEndpointStatus::Degraded => "degraded",
        Some(_) if policy.is_some_and(|value| value.status.eq_ignore_ascii_case("degraded")) => {
            "degraded"
        }
        _ if policy.is_some_and(|value| value.status.eq_ignore_ascii_case("degraded")) => {
            "degraded"
        }
        _ => "not_configured",
    };
    let blocker = if status == "available" {
        String::new()
    } else {
        "No live VLM endpoint is enabled for still-image summary.".to_string()
    };

    FeatureAvailabilityItem {
        feature_id: "retrieval.vision_summary".to_string(),
        label: "Still-image vision summary".to_string(),
        owner_lane: "harbor-framework".to_string(),
        status: status.to_string(),
        source_of_truth: "route_policy + vlm endpoint".to_string(),
        current_option: endpoint
            .map(|value| format!("{} / {}", value.model_endpoint_id, value.provider_key))
            .unwrap_or_else(|| "unconfigured".to_string()),
        fallback_order: policy_fallback_order(policy),
        blocker,
        evidence: vec![
            format!("route_policy_status={}", policy_status_value(policy)),
            format!(
                "endpoint_status={}",
                endpoint
                    .map(|value| value.status.as_str().to_string())
                    .unwrap_or_else(|| "missing".to_string())
            ),
        ],
    }
}

fn build_interactive_reply_feature(
    account_management: &AccountManagementSnapshot,
) -> FeatureAvailabilityItem {
    let delivery_policy = &account_management.delivery_policy.interactive_reply;
    let gateway = &account_management.gateway;
    let configured = gateway.bridge_provider.configured;
    let connected = gateway.bridge_provider.connected;
    let status = if delivery_policy.eq_ignore_ascii_case("source_bound") && connected {
        "available"
    } else if delivery_policy.eq_ignore_ascii_case("source_bound") && configured {
        "degraded"
    } else if delivery_policy.trim().is_empty() {
        "not_configured"
    } else {
        "blocked"
    };
    let blocker = match status {
        "available" => String::new(),
        "degraded" => "Gateway is configured but not fully connected.".to_string(),
        "not_configured" => "Interactive reply policy is not configured.".to_string(),
        _ => format!(
            "Interactive reply must stay source_bound, but current option is {}.",
            to_non_empty_option(delivery_policy)
        ),
    };

    FeatureAvailabilityItem {
        feature_id: "interactive_reply".to_string(),
        label: "Interaction-linked reply".to_string(),
        owner_lane: "harbor-im-gateway".to_string(),
        status: status.to_string(),
        source_of_truth: "delivery_policy + gateway_status".to_string(),
        current_option: to_non_empty_option(delivery_policy),
        fallback_order: Vec::new(),
        blocker,
        evidence: vec![
            format!(
                "binding_channel={}",
                to_non_empty_option(&gateway.binding_channel)
            ),
            format!("gateway_configured={}", yes_no(configured)),
            format!("gateway_connected={}", yes_no(connected)),
        ],
    }
}

fn build_proactive_delivery_feature(
    account_management: &AccountManagementSnapshot,
    gateway_status: Option<&Value>,
) -> FeatureAvailabilityItem {
    let delivery_policy = &account_management.delivery_policy.proactive_delivery;
    let default_target = account_management
        .notification_targets
        .iter()
        .find(|target| target.is_default)
        .or_else(|| account_management.notification_targets.first());
    let gateway_blocker = gateway_platform_blocker(gateway_status, "weixin");
    let status = if gateway_blocker.is_some() {
        "blocked"
    } else if default_target.is_none() {
        "not_configured"
    } else if account_management.gateway.bridge_provider.connected {
        "available"
    } else if account_management.gateway.bridge_provider.configured {
        "degraded"
    } else {
        "not_configured"
    };
    let blocker = if let Some(blocker) = gateway_blocker.clone() {
        blocker
    } else if default_target.is_none() {
        "No default notification target is configured.".to_string()
    } else if status == "degraded" {
        "Bridge provider is configured but not yet connected for proactive delivery.".to_string()
    } else {
        String::new()
    };

    let mut evidence = vec![
        format!("delivery_policy={}", to_non_empty_option(delivery_policy)),
        format!(
            "default_target={}",
            default_target
                .map(|target| format!("{} / {}", target.label, target.route_key))
                .unwrap_or_else(|| "none".to_string())
        ),
    ];
    if let Some(record_count) = gateway_delivery_record_count(gateway_status) {
        evidence.push(format!(
            "delivery_observability.record_count={record_count}"
        ));
    }

    FeatureAvailabilityItem {
        feature_id: "proactive_delivery".to_string(),
        label: "Proactive delivery".to_string(),
        owner_lane: "harbor-im-gateway".to_string(),
        status: status.to_string(),
        source_of_truth: "delivery_policy + notification_targets + gateway_status".to_string(),
        current_option: to_non_empty_option(delivery_policy),
        fallback_order: Vec::new(),
        blocker,
        evidence,
    }
}

fn build_binding_availability_feature(
    account_management: &AccountManagementSnapshot,
    gateway_status: Option<&Value>,
) -> FeatureAvailabilityItem {
    let bindings = &account_management.identity_bindings;
    let available_count = bindings
        .iter()
        .filter(|binding| binding.binding_available)
        .count();
    let status = if bindings.is_empty() {
        "not_configured"
    } else if available_count == bindings.len() {
        "available"
    } else if available_count > 0 {
        "degraded"
    } else {
        "blocked"
    };
    let blocker = if bindings.is_empty() {
        "No HarborGate-owned identity bindings are projected yet.".to_string()
    } else {
        bindings
            .iter()
            .find(|binding| !binding.binding_available)
            .map(|binding| binding.binding_availability_note.clone())
            .or_else(|| gateway_platform_blocker(gateway_status, "weixin"))
            .unwrap_or_default()
    };

    FeatureAvailabilityItem {
        feature_id: "binding_availability".to_string(),
        label: "Binding availability".to_string(),
        owner_lane: "harbor-im-gateway".to_string(),
        status: status.to_string(),
        source_of_truth: "account_management.identity_bindings + gateway_status".to_string(),
        current_option: format!("identity_bindings={}", bindings.len()),
        fallback_order: Vec::new(),
        blocker,
        evidence: vec![
            format!("available_bindings={available_count}"),
            format!(
                "binding_surfaces={}",
                if bindings.is_empty() {
                    "none".to_string()
                } else {
                    bindings
                        .iter()
                        .map(|binding| binding.proactive_delivery_surface.clone())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ),
        ],
    }
}

fn find_route_policy<'a>(
    route_policies: &'a [ModelRoutePolicy],
    route_policy_id: &str,
) -> Option<&'a ModelRoutePolicy> {
    route_policies
        .iter()
        .find(|policy| policy.route_policy_id == route_policy_id)
}

fn select_model_endpoint<'a>(
    endpoints: &'a [ModelEndpoint],
    preferred_id: &str,
    model_kind: ModelKind,
) -> Option<&'a ModelEndpoint> {
    endpoints
        .iter()
        .find(|endpoint| endpoint.model_endpoint_id == preferred_id)
        .or_else(|| {
            endpoints
                .iter()
                .filter(|endpoint| endpoint.model_kind == model_kind)
                .min_by_key(|endpoint| {
                    (
                        model_endpoint_status_rank(endpoint.status),
                        endpoint.model_endpoint_id.clone(),
                    )
                })
        })
}

fn model_endpoint_status_rank(status: ModelEndpointStatus) -> usize {
    match status {
        ModelEndpointStatus::Active => 0,
        ModelEndpointStatus::Degraded => 1,
        ModelEndpointStatus::Disabled => 2,
    }
}

fn is_builtin_local_openai_endpoint(endpoint: &ModelEndpoint) -> bool {
    endpoint.endpoint_kind == ModelEndpointKind::Local
        && endpoint
            .provider_key
            .eq_ignore_ascii_case("openai_compatible")
        && matches!(
            endpoint.model_kind,
            ModelKind::Llm | ModelKind::Embedder | ModelKind::Vlm
        )
}

fn infer_healthz_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if let Some(prefix) = trimmed.strip_suffix("/v1") {
        format!("{prefix}/healthz")
    } else if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}/healthz")
    }
}

fn metadata_missing_or_empty(metadata: &Value, key: &str) -> bool {
    metadata_string_value(metadata, key)
        .map(|value| value.trim().is_empty())
        .unwrap_or(true)
}

fn is_legacy_model_api_url(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    normalized.contains("127.0.0.1:4176") || normalized.contains("localhost:4176")
}

fn metadata_string_value(metadata: &Value, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn metadata_bool_value(metadata: &Value, key: &str) -> bool {
    metadata.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn ensure_metadata_object(metadata: &mut Value) -> &mut serde_json::Map<String, Value> {
    if !metadata.is_object() {
        *metadata = json!({});
    }
    metadata.as_object_mut().expect("metadata object")
}

fn set_metadata_string(metadata: &mut Value, key: &str, value: String) {
    ensure_metadata_object(metadata).insert(key.to_string(), Value::String(value));
}

fn set_metadata_bool(metadata: &mut Value, key: &str, value: bool) {
    ensure_metadata_object(metadata).insert(key.to_string(), Value::Bool(value));
}

fn has_projection_mismatch(endpoint: &ModelEndpoint) -> bool {
    metadata_bool_value(&endpoint.metadata, "projection_mismatch")
}

fn policy_fallback_order(policy: Option<&ModelRoutePolicy>) -> Vec<String> {
    policy
        .map(|value| value.fallback_order.clone())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            vec![
                "local".to_string(),
                "sidecar".to_string(),
                "cloud".to_string(),
            ]
        })
}

fn policy_status_value(policy: Option<&ModelRoutePolicy>) -> String {
    policy
        .map(|value| value.status.clone())
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "missing".to_string())
}

fn gateway_platform_blocker(payload: Option<&Value>, platform: &str) -> Option<String> {
    let from_platform_summary = payload
        .and_then(|value| value.get(platform))
        .and_then(|platform_value| {
            platform_value
                .get("blocker_category")
                .and_then(Value::as_str)
                .or_else(|| platform_value.get("blocker").and_then(Value::as_str))
                .or_else(|| platform_value.get("error").and_then(Value::as_str))
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if from_platform_summary.is_some() {
        return from_platform_summary;
    }

    let release_v1_key = format!("{platform}_blocker_category");
    payload
        .and_then(|value| value.get("release_v1"))
        .and_then(|value| value.get(release_v1_key.as_str()))
        .and_then(Value::as_str)
        .or_else(|| {
            payload
                .and_then(|value| value.get(release_v1_key.as_str()))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn gateway_delivery_record_count(payload: Option<&Value>) -> Option<u64> {
    payload
        .and_then(|value| value.get("delivery_observability"))
        .and_then(|value| value.get("record_count"))
        .and_then(Value::as_u64)
}

fn yes_no(value: bool) -> &'static str {
    if value {
        "yes"
    } else {
        "no"
    }
}

fn to_non_empty_option(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        "unconfigured".to_string()
    } else {
        trimmed.to_string()
    }
}

fn redact_model_endpoint_response(endpoints: &[ModelEndpoint]) -> ModelEndpointsResponse {
    ModelEndpointsResponse {
        endpoints: endpoints.iter().map(redact_model_endpoint).collect(),
    }
}

fn redact_bridge_provider_config(mut config: BridgeProviderConfig) -> BridgeProviderConfig {
    config.app_id.clear();
    config.app_secret.clear();
    config.bot_open_id.clear();
    config
}

fn task_error_message(response: &TaskResponse) -> String {
    response
        .prompt
        .clone()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| response.result.message.clone())
}

fn non_empty_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn now_unix_string() -> String {
    remote_view::now_unix_secs().to_string()
}

fn build_device_credential_statuses(
    state: &AdminConsoleState,
    devices: &[CameraDevice],
) -> Vec<DeviceCredentialStatusResponse> {
    devices
        .iter()
        .map(|device| build_device_credential_status(state, device))
        .collect()
}

fn build_device_credential_status(
    state: &AdminConsoleState,
    device: &CameraDevice,
) -> DeviceCredentialStatusResponse {
    let credential = state
        .device_credentials
        .iter()
        .find(|credential| credential.device_id == device.device_id);
    let fallback_configured = state.defaults.selected_camera_device_id.as_deref()
        == Some(device.device_id.as_str())
        && !state.defaults.rtsp_password.trim().is_empty();
    let platform_credential_configured = state.platform.credentials.iter().any(|credential| {
        credential.credential_id == device_rtsp_credential_id(&device.device_id)
            || credential
                .scope
                .get("device_id")
                .and_then(Value::as_str)
                .is_some_and(|value| value == device.device_id)
    });
    let configured = credential.is_some_and(|credential| !credential.password.trim().is_empty())
        || platform_credential_configured
        || fallback_configured;
    let username = credential
        .and_then(|credential| non_empty_string(&credential.username))
        .or_else(|| fallback_configured.then(|| state.defaults.rtsp_username.clone()))
        .and_then(|value| non_empty_string(&value));
    let rtsp_port = credential
        .and_then(|credential| credential.rtsp_port)
        .or_else(|| fallback_configured.then_some(state.defaults.rtsp_port))
        .or_else(|| rtsp_port_from_url(&device.primary_stream.url));
    let path_count = credential
        .map(|credential| credential.rtsp_paths.len())
        .filter(|count| *count > 0)
        .or_else(|| rtsp_path_from_url(&device.primary_stream.url).map(|_| 1))
        .unwrap_or_else(|| state.defaults.rtsp_paths.len());

    DeviceCredentialStatusResponse {
        device_id: device.device_id.clone(),
        configured,
        redacted: configured,
        username,
        rtsp_port,
        path_count,
        source: if credential.is_some() {
            "device_rtsp".to_string()
        } else if fallback_configured {
            "default_rtsp".to_string()
        } else {
            "none".to_string()
        },
        updated_at: credential.and_then(|credential| credential.updated_at.clone()),
        last_verified_at: credential.and_then(|credential| credential.last_verified_at.clone()),
    }
}

fn build_rtsp_check_evidence(
    device: &CameraDevice,
    check: &RtspCheckResponse,
    validation_id: Option<&str>,
) -> DeviceEvidenceRecord {
    let summary = if check.reachable {
        "RTSP probe reached a video stream"
    } else {
        "RTSP probe did not reach a video stream"
    };
    device_evidence_record(
        &device.device_id,
        "rtsp_check",
        if check.reachable { "passed" } else { "failed" },
        &check.checked_at,
        summary,
        json!({
            "validation_id": validation_id,
            "reachable": check.reachable,
            "stream_url": check.stream_url.clone(),
            "transport": check.transport.clone(),
            "requires_auth": check.requires_auth,
            "capabilities": check.capabilities.clone(),
            "error_message": check.error_message.clone(),
        }),
    )
}

fn build_rtsp_check_error_evidence(
    device: &CameraDevice,
    error: &str,
    observed_at: &str,
    validation_id: Option<&str>,
) -> DeviceEvidenceRecord {
    device_evidence_record(
        &device.device_id,
        "rtsp_check",
        "failed",
        observed_at,
        "RTSP probe could not run for this device",
        json!({
            "validation_id": validation_id,
            "reachable": false,
            "error_message": redact_stream_url_credentials(error),
        }),
    )
}

fn build_snapshot_check_evidence(
    device: &CameraDevice,
    response: &TaskResponse,
    validation_id: Option<&str>,
) -> DeviceEvidenceRecord {
    let redacted = redact_camera_task_response(response.clone());
    let (status, summary) = match redacted.status {
        TaskStatus::Completed => ("passed", "Snapshot capture produced metadata"),
        TaskStatus::NeedsInput => ("skipped", "Snapshot capture needs more input"),
        TaskStatus::Failed => ("failed", "Snapshot capture failed"),
    };
    let observed_at = redacted
        .result
        .data
        .pointer("/snapshot/captured_at_epoch_ms")
        .and_then(Value::as_u64)
        .map(|value| value.to_string())
        .unwrap_or_else(now_unix_string);
    let snapshot = redacted
        .result
        .data
        .pointer("/snapshot")
        .cloned()
        .unwrap_or(Value::Null);
    let artifact_count = redacted.result.artifacts.len();
    let artifacts = serde_json::to_value(&redacted.result.artifacts).unwrap_or(Value::Null);
    let task_id = redacted.task_id.clone();
    let trace_id = redacted.trace_id.clone();
    let executor_used = redacted.executor_used.clone();
    let message = redacted.result.message.clone();
    device_evidence_record(
        &device.device_id,
        "snapshot_check",
        status,
        &observed_at,
        summary,
        json!({
            "validation_id": validation_id,
            "task_id": task_id,
            "trace_id": trace_id,
            "executor_used": executor_used,
            "message": message,
            "snapshot": snapshot,
            "artifact_count": artifact_count,
            "artifacts": artifacts,
        }),
    )
}

fn build_snapshot_skipped_evidence(
    device: &CameraDevice,
    reason: &str,
    validation_id: Option<&str>,
) -> DeviceEvidenceRecord {
    device_evidence_record(
        &device.device_id,
        "snapshot_check",
        "skipped",
        &now_unix_string(),
        "Snapshot capture skipped",
        json!({
            "validation_id": validation_id,
            "reason": reason,
        }),
    )
}

fn build_snapshot_asset_evidence(
    device: &CameraDevice,
    asset: &MediaAsset,
) -> DeviceEvidenceRecord {
    let observed_at = asset.captured_at.clone().unwrap_or_else(now_unix_string);
    DeviceEvidenceRecord {
        evidence_id: format!("media-asset-{}", asset.asset_id),
        device_id: device.device_id.clone(),
        evidence_kind: "snapshot_check".to_string(),
        status: "passed".to_string(),
        observed_at,
        summary: "Recent persisted snapshot media asset".to_string(),
        details: redact_secret_json_value(json!({
            "media_asset_id": asset.asset_id.clone(),
            "storage_uri": asset.storage_uri.clone(),
            "mime_type": asset.mime_type.clone(),
            "byte_size": asset.byte_size,
            "captured_at": asset.captured_at.clone(),
            "tags": asset.tags.clone(),
            "metadata": asset.metadata.clone(),
        })),
    }
}

fn build_share_link_evidence(summary: &ShareLinkSummary) -> DeviceEvidenceRecord {
    let evidence_kind = if summary.status == "revoked" {
        "share_link_revoke"
    } else {
        "share_link_create"
    };
    let observed_at = if summary.status == "revoked" {
        summary
            .revoked_at
            .clone()
            .or_else(|| summary.ended_at.clone())
            .or_else(|| summary.started_at.clone())
    } else {
        summary
            .started_at
            .clone()
            .or_else(|| summary.expires_at.clone())
    }
    .unwrap_or_else(now_unix_string);
    DeviceEvidenceRecord {
        evidence_id: format!("{}-{}", evidence_kind, summary.share_link_id),
        device_id: summary.device_id.clone(),
        evidence_kind: evidence_kind.to_string(),
        status: summary.status.clone(),
        observed_at,
        summary: format!("Share link is {}", summary.status),
        details: redact_secret_json_value(json!({
            "share_link_id": summary.share_link_id.clone(),
            "media_session_id": summary.media_session_id.clone(),
            "access_scope": summary.access_scope.clone(),
            "session_status": summary.session_status.clone(),
            "expires_at": summary.expires_at.clone(),
            "revoked_at": summary.revoked_at.clone(),
            "started_at": summary.started_at.clone(),
            "ended_at": summary.ended_at.clone(),
            "can_revoke": summary.can_revoke,
        })),
    }
}

fn device_evidence_record(
    device_id: &str,
    evidence_kind: &str,
    status: &str,
    observed_at: &str,
    summary: &str,
    details: Value,
) -> DeviceEvidenceRecord {
    DeviceEvidenceRecord {
        evidence_id: format!(
            "device-evidence-{}-{}",
            sanitize_id_fragment(evidence_kind),
            Uuid::new_v4().simple()
        ),
        device_id: device_id.to_string(),
        evidence_kind: evidence_kind.to_string(),
        status: status.to_string(),
        observed_at: observed_at.to_string(),
        summary: redact_stream_url_credentials(summary),
        details: redact_secret_json_value(details),
    }
}

fn redact_device_evidence_records(records: Vec<DeviceEvidenceRecord>) -> Vec<DeviceEvidenceRecord> {
    records
        .into_iter()
        .map(redact_device_evidence_record)
        .collect()
}

fn redact_device_evidence_record(mut record: DeviceEvidenceRecord) -> DeviceEvidenceRecord {
    record.summary = redact_stream_url_credentials(&record.summary);
    record.details = redact_secret_json_value(record.details);
    record
}

fn validation_status(
    rtsp_check: &DeviceEvidenceRecord,
    snapshot_check: &DeviceEvidenceRecord,
) -> String {
    match (rtsp_check.status.as_str(), snapshot_check.status.as_str()) {
        ("passed", "passed") => "passed",
        ("failed", "failed") | ("failed", "skipped") => "failed",
        _ => "degraded",
    }
    .to_string()
}

fn device_has_snapshot_path(device: &CameraDevice) -> bool {
    !device.primary_stream.url.trim().is_empty()
        || device
            .snapshot_url
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
}

fn sanitize_id_fragment(value: &str) -> String {
    let mut output = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    while output.contains("--") {
        output = output.replace("--", "-");
    }
    let output = output.trim_matches('-').to_string();
    if output.is_empty() {
        "item".to_string()
    } else {
        output
    }
}

fn rtsp_host_from_url(value: &str) -> Option<String> {
    Url::parse(value)
        .ok()
        .filter(|url| url.scheme().eq_ignore_ascii_case("rtsp"))
        .and_then(|url| url.host_str().map(str::to_string))
}

fn rtsp_port_from_url(value: &str) -> Option<u16> {
    Url::parse(value)
        .ok()
        .filter(|url| url.scheme().eq_ignore_ascii_case("rtsp"))
        .and_then(|url| url.port_or_known_default())
}

fn rtsp_path_from_url(value: &str) -> Option<String> {
    Url::parse(value)
        .ok()
        .filter(|url| url.scheme().eq_ignore_ascii_case("rtsp"))
        .map(|url| url.path().to_string())
        .filter(|path| !path.trim().is_empty() && path != "/")
}

fn redact_stream_url_credentials(value: &str) -> String {
    let parsed = Url::parse(value).ok().map(|mut url| {
        if !url.username().is_empty() || url.password().is_some() {
            let _ = url.set_username("redacted");
            let _ = url.set_password(Some("redacted"));
        }
        url.to_string()
    });
    redact_query_like_secrets(&redact_url_userinfo_occurrences(
        parsed.as_deref().unwrap_or(value),
    ))
}

fn redact_admin_string(value: &str) -> String {
    redact_local_path_occurrences(&redact_url_query_secrets(&redact_stream_url_credentials(
        value,
    )))
}

fn redact_local_path_occurrences(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut index = 0;
    while index < value.len() {
        let remaining = &value[index..];
        if remaining.starts_with('/') && is_local_path_boundary(value, index) {
            let end = consume_local_path(value, index);
            if end > index + 1 && looks_like_posix_local_path(value, index, end) {
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

fn looks_like_posix_local_path(value: &str, start: usize, end: usize) -> bool {
    let candidate = &value[start..end];
    const LOCAL_PATH_PREFIXES: &[&str] = &[
        "/app/",
        "/data/",
        "/etc/",
        "/home/",
        "/media/",
        "/mnt/",
        "/opt/",
        "/private/",
        "/root/",
        "/run/",
        "/srv/",
        "/tmp/",
        "/usr/",
        "/var/",
        "/Volumes/",
        "/workspace/",
    ];
    LOCAL_PATH_PREFIXES
        .iter()
        .any(|prefix| candidate.starts_with(prefix))
}

fn is_local_path_boundary(value: &str, index: usize) -> bool {
    if index == 0 {
        return true;
    }
    value[..index]
        .chars()
        .last()
        .map(|ch| ch.is_whitespace() || matches!(ch, '"' | '\'' | '(' | '[' | '{' | '='))
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

fn redact_url_query_secrets(value: &str) -> String {
    let Ok(mut url) = Url::parse(value) else {
        return value.to_string();
    };
    let pairs = url
        .query_pairs()
        .map(|(key, value)| {
            if is_secret_key(key.as_ref()) {
                (key.to_string(), "redacted".to_string())
            } else {
                (key.to_string(), value.to_string())
            }
        })
        .collect::<Vec<_>>();
    if pairs.iter().all(|(key, value)| {
        url.query_pairs().any(|(original_key, original_value)| {
            original_key == key.as_str() && original_value == value.as_str()
        })
    }) {
        return value.to_string();
    }
    url.query_pairs_mut().clear().extend_pairs(pairs.iter());
    url.to_string()
}

fn redact_url_userinfo_occurrences(value: &str) -> String {
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
        let userinfo_end = authority.rfind('@');
        let has_password = userinfo_end
            .and_then(|end| authority[..end].find(':'))
            .is_some();
        if let Some(end) = userinfo_end.filter(|_| has_password) {
            output.push_str(&value[cursor..authority_start]);
            output.push_str("redacted:redacted@");
            output.push_str(&authority[end + 1..]);
        } else {
            output.push_str(&value[cursor..authority_end]);
        }
        cursor = authority_end;
    }
    output.push_str(&value[cursor..]);
    output
}

fn redact_query_like_secrets(value: &str) -> String {
    let mut redacted = value.to_string();
    for key in ["password", "token", "api_key", "apikey", "secret"] {
        redacted = redact_query_like_secret(&redacted, key);
    }
    redacted
}

fn redact_query_like_secret(value: &str, key: &str) -> String {
    let needle = format!("{key}=");
    let lower = value.to_ascii_lowercase();
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
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

fn parse_scan_results(
    data: &Value,
) -> Result<Vec<harborbeacon_local_agent::runtime::hub::HubScanResultItem>, String> {
    let value = data
        .pointer("/candidates")
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    serde_json::from_value(value)
        .map_err(|error| format!("failed to parse camera scan results from task response: {error}"))
}

fn parse_connected_device(data: &Value) -> Result<CameraDevice, String> {
    let value = data
        .pointer("/device")
        .cloned()
        .ok_or_else(|| "task response missing connected device payload".to_string())?;
    serde_json::from_value(value)
        .map_err(|error| format!("failed to parse connected camera from task response: {error}"))
}

fn parse_camera_snapshot_path(path: &str) -> Option<String> {
    let trimmed = path.strip_prefix("/api/cameras/")?;
    let device_id = trimmed.strip_suffix("/snapshot.jpg")?;
    if device_id.is_empty() {
        None
    } else {
        percent_decode_path_segment(device_id).ok()
    }
}

fn parse_camera_live_stream_path(path: &str) -> Option<String> {
    let trimmed = path.strip_prefix("/api/cameras/")?;
    let device_id = trimmed.strip_suffix("/live.mjpeg")?;
    if device_id.is_empty() {
        None
    } else {
        percent_decode_path_segment(device_id).ok()
    }
}

fn parse_camera_hls_live_start_path(path: &str) -> Option<String> {
    parse_camera_live_action_path(path, "/live/start")
}

fn parse_camera_hls_live_stop_path(path: &str) -> Option<String> {
    parse_camera_live_action_path(path, "/live/stop")
}

fn parse_camera_hls_live_status_path(path: &str) -> Option<String> {
    parse_camera_live_action_path(path, "/live/status")
}

fn parse_camera_live_action_path(path: &str, suffix: &str) -> Option<String> {
    let trimmed = path.strip_prefix("/api/cameras/")?;
    let device_id = trimmed.strip_suffix(suffix)?;
    if device_id.is_empty() {
        None
    } else {
        percent_decode_path_segment(device_id).ok()
    }
}

fn parse_camera_hls_live_asset_path(path: &str) -> Option<(String, String, String)> {
    let trimmed = path.strip_prefix("/api/cameras/")?;
    let (device_id, rest) = trimmed.split_once("/live/")?;
    let (session_id, asset_name) = rest.split_once('/')?;
    if device_id.is_empty() || session_id.is_empty() || !session_id.starts_with("live-") {
        return None;
    }
    let asset_name = percent_decode_path_segment(asset_name).ok()?;
    if !is_safe_live_asset_name(&asset_name) {
        return None;
    }
    Some((
        percent_decode_path_segment(device_id).ok()?,
        percent_decode_path_segment(session_id).ok()?,
        asset_name,
    ))
}

fn parse_camera_analyze_path(path: &str) -> Option<String> {
    let trimmed = path.strip_prefix("/api/cameras/")?;
    let device_id = trimmed.strip_suffix("/analyze")?;
    if device_id.is_empty() {
        None
    } else {
        percent_decode_path_segment(device_id).ok()
    }
}

fn parse_camera_task_snapshot_path(path: &str) -> Option<String> {
    let trimmed = path.strip_prefix("/api/cameras/")?;
    let device_id = trimmed.strip_suffix("/snapshot")?;
    if device_id.is_empty() {
        None
    } else {
        percent_decode_path_segment(device_id).ok()
    }
}

fn parse_camera_share_link_path(path: &str) -> Option<String> {
    let trimmed = path.strip_prefix("/api/cameras/")?;
    let device_id = trimmed.strip_suffix("/share-link")?;
    if device_id.is_empty() {
        None
    } else {
        percent_decode_path_segment(device_id).ok()
    }
}

fn parse_camera_recording_start_path(path: &str) -> Option<String> {
    parse_camera_recording_action_path(path, "/recordings/start")
}

fn parse_camera_recording_stop_path(path: &str) -> Option<String> {
    parse_camera_recording_action_path(path, "/recordings/stop")
}

fn parse_camera_recording_action_path(path: &str, suffix: &str) -> Option<String> {
    let trimmed = path.strip_prefix("/api/cameras/")?;
    let device_id = trimmed.strip_suffix(suffix)?;
    if device_id.is_empty() {
        None
    } else {
        percent_decode_path_segment(device_id).ok()
    }
}

fn parse_device_credentials_path(path: &str) -> Option<String> {
    parse_device_scoped_path(path, "/credentials")
}

fn parse_device_evidence_path(path: &str) -> Option<String> {
    parse_device_scoped_path(path, "/evidence")
}

fn parse_device_validation_run_path(path: &str) -> Option<String> {
    parse_device_scoped_path(path, "/validation/run")
}

fn parse_device_rtsp_check_path(path: &str) -> Option<String> {
    parse_device_scoped_path(path, "/rtsp-check")
}

fn parse_device_credential_status_path(path: &str) -> Option<String> {
    parse_device_scoped_path(path, "/credential-status")
}

fn parse_device_metadata_path(path: &str) -> Option<String> {
    let device_id = path.strip_prefix("/api/devices/")?.trim();
    if device_id.is_empty() || device_id.contains('/') {
        None
    } else {
        percent_decode_path_segment(device_id).ok()
    }
}

fn parse_device_metadata_patch_path(path: &str) -> Option<String> {
    parse_device_metadata_path(path)
}

fn parse_device_scoped_path(path: &str, suffix: &str) -> Option<String> {
    let trimmed = path.strip_prefix("/api/devices/")?;
    let device_id = trimmed.strip_suffix(suffix)?;
    if device_id.is_empty() || device_id.contains('/') {
        None
    } else {
        percent_decode_path_segment(device_id).ok()
    }
}

fn parse_share_link_revoke_path(path: &str) -> Option<String> {
    let trimmed = path.strip_prefix("/api/share-links/")?;
    let share_link_id = trimmed.strip_suffix("/revoke")?;
    if share_link_id.is_empty() {
        None
    } else {
        percent_decode_path_segment(share_link_id).ok()
    }
}

fn parse_camera_live_page_path(path: &str) -> Option<String> {
    let device_id = path.strip_prefix("/live/cameras/")?;
    if device_id.is_empty() {
        None
    } else {
        percent_decode_path_segment(device_id).ok()
    }
}

fn parse_shared_camera_live_page_path(path: &str) -> Option<String> {
    let token = path.strip_prefix("/shared/cameras/")?;
    if token.is_empty() || token.contains('/') {
        None
    } else {
        percent_decode_path_segment(token).ok()
    }
}

fn parse_shared_camera_live_stream_path(path: &str) -> Option<String> {
    let trimmed = path.strip_prefix("/shared/cameras/")?;
    let token = trimmed.strip_suffix("/live.mjpeg")?;
    if token.is_empty() || token.contains('/') {
        None
    } else {
        percent_decode_path_segment(token).ok()
    }
}

fn url_encode_path_segment(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(byte as char);
        } else {
            let _ = write!(&mut encoded, "%{byte:02X}");
        }
    }
    encoded
}

fn percent_decode_path_segment(value: &str) -> Result<String, String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err("incomplete percent escape".to_string());
            }
            let hi = decode_hex(bytes[index + 1]).ok_or_else(|| "invalid hex digit".to_string())?;
            let lo = decode_hex(bytes[index + 2]).ok_or_else(|| "invalid hex digit".to_string())?;
            decoded.push((hi << 4) | lo);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }

    String::from_utf8(decoded).map_err(|error| format!("invalid utf-8 path segment: {error}"))
}

fn decode_hex(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[derive(Debug, Default, Deserialize)]
struct LiveStopRequest {
    session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct HlsLiveSessionProjection {
    device_id: String,
    session_id: Option<String>,
    status: String,
    playlist_url: Option<String>,
    playlist_ready: bool,
    mode: String,
    codec: String,
    started_at: Option<String>,
    updated_at: String,
    message: String,
}

impl HlsLiveSessionProjection {
    fn stopped(device_id: &str, message: impl Into<String>) -> Self {
        Self {
            device_id: device_id.to_string(),
            session_id: None,
            status: "stopped".to_string(),
            playlist_url: None,
            playlist_ready: false,
            mode: "hls_fmp4".to_string(),
            codec: "h264_copy".to_string(),
            started_at: None,
            updated_at: current_unix_secs().to_string(),
            message: message.into(),
        }
    }

    fn to_response(&self, _public_origin: &str) -> Self {
        self.clone()
    }
}

struct HlsLiveSession {
    device_id: String,
    session_id: String,
    root: PathBuf,
    started_at: String,
    child: Child,
}

#[derive(Clone, Default)]
struct HlsLiveRuntime {
    inner: Arc<Mutex<HlsLiveRuntimeInner>>,
}

#[derive(Default)]
struct HlsLiveRuntimeInner {
    sessions: HashMap<String, HlsLiveSession>,
    device_sessions: HashMap<String, String>,
}

impl HlsLiveRuntime {
    fn start_session(
        &self,
        device_id: &str,
        stream_url: &str,
    ) -> Result<HlsLiveSessionProjection, String> {
        if stream_url.trim().is_empty() {
            return Err("camera RTSP stream is not configured".to_string());
        }
        let ffmpeg_bin = resolve_ffmpeg_bin()
            .ok_or_else(|| format!("当前机器缺少 ffmpeg，{}", ffmpeg_resolution_hint()))?;
        let session_id = format!("live-{}", Uuid::new_v4().as_simple());
        let root = hls_live_root()
            .join(safe_live_path_segment(device_id))
            .join(&session_id);
        if root.exists() {
            fs::remove_dir_all(&root)
                .map_err(|error| format!("failed to reset live session directory: {error}"))?;
        }
        fs::create_dir_all(&root)
            .map_err(|error| format!("failed to create live session directory: {error}"))?;

        let playlist_path = root.join("index.m3u8");
        let mut child = Command::new(&ffmpeg_bin)
            .current_dir(&root)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-rtsp_transport",
                "tcp",
                "-fflags",
                "nobuffer",
                "-flags",
                "low_delay",
                "-i",
                stream_url,
                "-map",
                "0:v:0",
                "-an",
                "-c:v",
                "copy",
                "-f",
                "hls",
                "-hls_time",
                "1",
                "-hls_list_size",
                "6",
                "-hls_flags",
                "delete_segments+omit_endlist+independent_segments",
                "-hls_segment_type",
                "fmp4",
                "-hls_fmp4_init_filename",
                "init.mp4",
                "-hls_segment_filename",
                "segment_%05d.m4s",
                "index.m3u8",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("启动 H.264 live remux ffmpeg 失败: {error}"))?;

        for _ in 0..20 {
            if playlist_path.exists() {
                break;
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    let _ = fs::remove_dir_all(&root);
                    return Err(format!(
                        "H.264 live remux exited before playlist was ready: {status}"
                    ));
                }
                Ok(None) => thread::sleep(Duration::from_millis(100)),
                Err(error) => {
                    let _ = fs::remove_dir_all(&root);
                    return Err(format!("failed to check live remux process: {error}"));
                }
            }
        }

        let session = HlsLiveSession {
            device_id: device_id.to_string(),
            session_id: session_id.clone(),
            root,
            started_at: current_unix_secs().to_string(),
            child,
        };

        let mut inner = self
            .inner
            .lock()
            .map_err(|_| "live session lock is poisoned".to_string())?;
        if let Some(existing_session_id) = inner.device_sessions.remove(device_id) {
            if let Some(mut existing) = inner.sessions.remove(&existing_session_id) {
                stop_hls_live_session(&mut existing);
            }
        }
        inner
            .device_sessions
            .insert(device_id.to_string(), session_id.clone());
        inner.sessions.insert(session_id.clone(), session);
        Ok(self.projection_locked(&mut inner, device_id, Some(&session_id)))
    }

    fn status(&self, device_id: &str, session_id: Option<&str>) -> HlsLiveSessionProjection {
        let Ok(mut inner) = self.inner.lock() else {
            return HlsLiveSessionProjection::stopped(
                device_id,
                "live session lock is unavailable",
            );
        };
        self.projection_locked(&mut inner, device_id, session_id)
    }

    fn stop_session(&self, device_id: &str, session_id: Option<&str>) -> HlsLiveSessionProjection {
        let Ok(mut inner) = self.inner.lock() else {
            return HlsLiveSessionProjection::stopped(
                device_id,
                "live session lock is unavailable",
            );
        };
        let Some(session_id) = session_id
            .map(str::to_string)
            .or_else(|| inner.device_sessions.get(device_id).cloned())
        else {
            return HlsLiveSessionProjection::stopped(device_id, "no live session is running");
        };
        let Some(mut session) = inner.sessions.remove(&session_id) else {
            inner.device_sessions.remove(device_id);
            return HlsLiveSessionProjection::stopped(
                device_id,
                "live session was already stopped",
            );
        };
        inner.device_sessions.remove(&session.device_id);
        stop_hls_live_session(&mut session);
        HlsLiveSessionProjection {
            device_id: session.device_id,
            session_id: Some(session.session_id),
            status: "stopped".to_string(),
            playlist_url: None,
            playlist_ready: false,
            mode: "hls_fmp4".to_string(),
            codec: "h264_copy".to_string(),
            started_at: Some(session.started_at),
            updated_at: current_unix_secs().to_string(),
            message: "live session stopped".to_string(),
        }
    }

    fn asset_path(
        &self,
        device_id: &str,
        session_id: &str,
        asset_name: &str,
    ) -> Result<PathBuf, String> {
        if !is_safe_live_asset_name(asset_name) {
            return Err("unsafe live asset path".to_string());
        }
        let inner = self
            .inner
            .lock()
            .map_err(|_| "live session lock is poisoned".to_string())?;
        let session = inner
            .sessions
            .get(session_id)
            .ok_or_else(|| "live session not found".to_string())?;
        if session.device_id != device_id {
            return Err("live session not found for camera".to_string());
        }
        let path = session.root.join(asset_name);
        if !path_is_same_or_inside(&path.to_string_lossy(), &session.root.to_string_lossy()) {
            return Err("unsafe live asset path".to_string());
        }
        Ok(path)
    }

    fn projection_locked(
        &self,
        inner: &mut HlsLiveRuntimeInner,
        device_id: &str,
        session_id: Option<&str>,
    ) -> HlsLiveSessionProjection {
        let Some(session_id) = session_id
            .map(str::to_string)
            .or_else(|| inner.device_sessions.get(device_id).cloned())
        else {
            return HlsLiveSessionProjection::stopped(device_id, "no live session is running");
        };

        let Some(session) = inner.sessions.get_mut(&session_id) else {
            inner.device_sessions.remove(device_id);
            return HlsLiveSessionProjection::stopped(device_id, "live session is not registered");
        };
        if session.device_id != device_id {
            return HlsLiveSessionProjection::stopped(
                device_id,
                "live session belongs to another camera",
            );
        }
        match session.child.try_wait() {
            Ok(Some(status)) => {
                let mut session = inner
                    .sessions
                    .remove(&session_id)
                    .expect("session exists after get_mut");
                inner.device_sessions.remove(&session.device_id);
                stop_hls_live_session(&mut session);
                return HlsLiveSessionProjection {
                    device_id: session.device_id,
                    session_id: Some(session.session_id),
                    status: "failed".to_string(),
                    playlist_url: None,
                    playlist_ready: false,
                    mode: "hls_fmp4".to_string(),
                    codec: "h264_copy".to_string(),
                    started_at: Some(session.started_at),
                    updated_at: current_unix_secs().to_string(),
                    message: format!("live remux process exited: {status}"),
                };
            }
            Ok(None) => {}
            Err(error) => {
                return HlsLiveSessionProjection {
                    device_id: device_id.to_string(),
                    session_id: Some(session_id),
                    status: "degraded".to_string(),
                    playlist_url: None,
                    playlist_ready: false,
                    mode: "hls_fmp4".to_string(),
                    codec: "h264_copy".to_string(),
                    started_at: Some(session.started_at.clone()),
                    updated_at: current_unix_secs().to_string(),
                    message: format!("failed to inspect live remux process: {error}"),
                };
            }
        }

        let playlist_ready = session.root.join("index.m3u8").exists();
        HlsLiveSessionProjection {
            device_id: session.device_id.clone(),
            session_id: Some(session.session_id.clone()),
            status: if playlist_ready {
                "running"
            } else {
                "starting"
            }
            .to_string(),
            playlist_url: Some(format!(
                "/api/beacon/cameras/{}/live/{}/index.m3u8",
                url_encode_path_segment(&session.device_id),
                url_encode_path_segment(&session.session_id)
            )),
            playlist_ready,
            mode: "hls_fmp4".to_string(),
            codec: "h264_copy".to_string(),
            started_at: Some(session.started_at.clone()),
            updated_at: current_unix_secs().to_string(),
            message: if playlist_ready {
                "H.264 live remux is running"
            } else {
                "H.264 live remux is starting"
            }
            .to_string(),
        }
    }
}

fn stop_hls_live_session(session: &mut HlsLiveSession) {
    let _ = session.child.kill();
    let _ = session.child.wait();
    let _ = fs::remove_dir_all(&session.root);
}

fn hls_live_root() -> PathBuf {
    env::var("HARBORNAVI_LIVE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/run/harbornavi/live"))
}

fn safe_live_path_segment(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            output.push(ch);
        } else {
            output.push('_');
        }
    }
    if output.is_empty() {
        "camera".to_string()
    } else {
        output
    }
}

fn is_safe_live_asset_name(value: &str) -> bool {
    !value.is_empty()
        && !value.contains('/')
        && !value.contains('\\')
        && !value.contains("..")
        && matches!(
            Path::new(value).extension().and_then(|ext| ext.to_str()),
            Some("m3u8" | "m4s" | "mp4" | "ts")
        )
}

fn live_asset_mime_type(value: &str) -> &'static str {
    match Path::new(value).extension().and_then(|ext| ext.to_str()) {
        Some("m3u8") => "application/vnd.apple.mpegurl",
        Some("m4s") => "video/iso.segment",
        Some("mp4") => "video/mp4",
        Some("ts") => "video/mp2t",
        _ => "application/octet-stream",
    }
}

fn current_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_secs()
}

struct FfmpegMjpegStream {
    child: Child,
    stdout: ChildStdout,
}

impl FfmpegMjpegStream {
    fn spawn(stream_url: &str) -> Result<Self, String> {
        let ffmpeg_bin = resolve_ffmpeg_bin()
            .ok_or_else(|| format!("当前机器缺少 ffmpeg，{}", ffmpeg_resolution_hint()))?;

        let mut child = Command::new(&ffmpeg_bin)
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-rtsp_transport",
                "tcp",
                "-fflags",
                "nobuffer",
                "-flags",
                "low_delay",
                "-i",
                stream_url,
                "-an",
                "-vf",
                "fps=5,scale=960:-2:flags=fast_bilinear",
                "-q:v",
                "6",
                "-f",
                "mpjpeg",
                "-boundary_tag",
                "ffmpeg",
                "pipe:1",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("启动实时转码 ffmpeg 失败: {error}"))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "无法获取 ffmpeg 输出管道".to_string())?;

        Ok(Self { child, stdout })
    }
}

impl Read for FfmpegMjpegStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.stdout.read(buf)
    }
}

impl Drop for FfmpegMjpegStream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::{
        apply_bridge_provider_binding_projection, authorize_gateway_service_request,
        build_admin_knowledge_search_request, build_default_notification_target_readiness,
        build_device_credential_status, build_feature_availability_response,
        build_files_browse_response, build_harboros_im_capability_map,
        build_harboros_status_response, build_hardware_readiness_response,
        build_home_assistant_operator_audit, build_inference_health_alias_response,
        build_knowledge_index_job, build_knowledge_index_status_response,
        build_local_model_catalog, build_local_vision_event_notification_blocked_response,
        build_model_capabilities_response, build_rag_readiness_response,
        build_redacted_diagnostics_bundle, build_release_readiness_response,
        build_rtsp_url_from_patch, camera_stream_url_with_credentials, current_epoch_secs,
        default_model_download_target_path, default_model_download_target_path_in_root,
        default_model_endpoints, embedding_warmup_timeout_stats, ensure_local_admin_access,
        ensure_local_camera_access, harbor_assistant_build_missing_response,
        hardware_class_for_probe, has_forwarding_headers,
        huggingface_download_should_fallback_to_plain_http, huggingface_resolve_url,
        identity_query_suffix, is_admin_surface_path, is_harbor_assistant_client_route,
        is_harbor_assistant_surface_path, is_safe_live_asset_name,
        knowledge_preview_mime_supported, latest_model_download_jobs,
        live_bridge_provider_from_setup_status, local_model_catalog_item,
        local_model_catalog_specs, mime_type_for_path, model_download_huggingface_endpoint,
        model_download_huggingface_endpoints, model_download_jobs_status,
        model_hardware_recommendation, model_snapshot_file_allowed, normalize_unified_admin_path,
        overlay_model_endpoints_with_runtime_truth, parse_approval_decision_path,
        parse_camera_analyze_path, parse_camera_hls_live_asset_path,
        parse_camera_hls_live_start_path, parse_camera_hls_live_status_path,
        parse_camera_hls_live_stop_path, parse_camera_live_page_path,
        parse_camera_live_stream_path, parse_camera_recording_start_path,
        parse_camera_recording_stop_path, parse_camera_share_link_path, parse_camera_snapshot_path,
        parse_camera_task_snapshot_path, parse_device_credential_status_path,
        parse_device_credentials_path, parse_device_evidence_path,
        parse_device_metadata_patch_path, parse_device_rtsp_check_path,
        parse_device_validation_run_path, parse_json_body_limited,
        parse_knowledge_index_job_cancel_path, parse_local_vision_event_notify_path,
        parse_member_default_delivery_surface_update_path, parse_member_role_update_path,
        parse_model_download_cancel_path, parse_model_download_job_path, parse_model_endpoint_path,
        parse_model_endpoint_test_path, parse_model_runtime_install_path,
        parse_notification_target_delete_path, parse_optional_unix_seconds,
        parse_share_link_revoke_path, parse_shared_camera_live_page_path,
        parse_shared_camera_live_stream_path, percent_decode_path_segment,
        probe_local_model_runtime, redact_account_management_snapshot, redact_admin_string,
        redact_bridge_provider_config, redact_camera_device_projection,
        redact_model_endpoint_response, redact_state_snapshot, redact_stream_url_credentials,
        redact_value_stream_credentials, redacted_general_message_nsp_route_summary,
        redacted_home_assistant_task_api_event_summary, release_item, request_identity_hints,
        resolve_harbor_assistant_asset_path, resolve_knowledge_preview_path,
        run_knowledge_index_jobs, run_model_download_job, run_model_download_transfer,
        scan_request_task_args, service_overloaded_response, url_encode_path_segment,
        validate_home_assistant_service_fields, validate_home_assistant_service_smoke, AdminApi,
        HomeAssistantServiceSmokeRequest, HomeGuardianEvaluationQueue, KnowledgeSearchApiRequest,
        LocalModelRuntimeProjection, ManualAddRequest, ModelRuntimeActivationRequest,
        ModelRuntimeActivationResult, VisionIngestLimiter, DEFAULT_HF_ENDPOINT,
        HOME_GUARDIAN_ACTION_COOLDOWN_SECONDS, HOME_GUARDIAN_AUTO_EVALUATION_MIN_INTERVAL_SECONDS,
        MAX_VISION_EVENT_INGEST_INFLIGHT, PRIVACY_GATEWAY_AUDIT_ACTION,
    };
    use harborbeacon_local_agent::control_plane::audit::{
        build_metadata_audit_record, AuditActorKind,
    };
    use harborbeacon_local_agent::control_plane::events::EventRecord;
    use harborbeacon_local_agent::control_plane::media::{
        MediaDeliveryMode, MediaSession, MediaSessionKind, MediaSessionStatus, ShareAccessScope,
        ShareLink,
    };
    use harborbeacon_local_agent::control_plane::models::{
        ModelEndpoint, ModelEndpointKind, ModelEndpointStatus, ModelKind, PrivacyLevel,
    };
    use harborbeacon_local_agent::control_plane::users::{MembershipStatus, RoleKind};
    use harborbeacon_local_agent::runtime::access_control::{
        AccessAction, AccessIdentityHints, AccessPrincipal,
    };
    use harborbeacon_local_agent::runtime::admin_console::{
        default_model_route_policies, AdminConsoleState, AdminConsoleStore, AdminModelCenterState,
        AutomationRuleReview, BridgeProviderConfig, DeviceCredentialSecret, DeviceEvidenceRecord,
        HomeAssistantAdminState, KnowledgeSettings, KnowledgeSourceRoot, ModelDownloadJobRecord,
        NotificationTargetRecord, RagResourceProfile, RemoteViewConfig,
    };
    use harborbeacon_local_agent::runtime::hub::CameraHubService;
    use harborbeacon_local_agent::runtime::knowledge_index::{
        KnowledgeIndexConfig, KnowledgeIndexService,
    };
    use harborbeacon_local_agent::runtime::privacy_gateway::PRIVACY_GATEWAY_POLICY_VERSION;
    use harborbeacon_local_agent::runtime::registry::{CameraDevice, DeviceRegistryStore};
    use harborbeacon_local_agent::runtime::remote_view;
    use harborbeacon_local_agent::runtime::task_api::TaskApiService;
    use harborbeacon_local_agent::runtime::task_session::TaskConversationStore;
    use harborbeacon_local_agent::runtime::vision_event::{
        ingest_local_vision_event, LocalVisionEvent, SnapshotArtifact, StoredLocalVisionEvent,
        VISION_EVENT_STORE_PATH_ENV,
    };
    use serde_json::{json, Value};
    use std::fs;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
    use std::path::{Path, PathBuf};
    use std::sync::mpsc::sync_channel;
    use std::sync::{Arc, Mutex, OnceLock};
    use std::thread;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    fn hf_endpoint_env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }
    use tiny_http::{Header, StatusCode};

    #[test]
    fn parse_json_body_limited_blocks_oversized_payload() {
        let payload = br#"{"ok":true}"#;
        let parsed: Value =
            parse_json_body_limited(payload, payload.len()).expect("valid small json");
        assert_eq!(parsed["ok"], json!(true));

        let error = parse_json_body_limited::<Value>(payload, payload.len() - 1)
            .expect_err("oversized payload must fail");

        assert!(error.contains("exceeds"));
    }

    fn unique_store_path(prefix: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{unique}.json"))
    }

    fn build_test_admin_api(prefix: &str) -> (AdminApi, Vec<PathBuf>) {
        let admin_path = unique_store_path(&format!("{prefix}-state"));
        let registry_path = unique_store_path(&format!("{prefix}-registry"));
        let conversation_path = unique_store_path(&format!("{prefix}-runtime"));
        let registry_store = DeviceRegistryStore::new(registry_path.clone());
        let admin_store = AdminConsoleStore::new(admin_path.clone(), registry_store);
        let conversation_store = TaskConversationStore::new(conversation_path.clone());
        let task_service = TaskApiService::new(admin_store.clone(), conversation_store);
        let api = AdminApi::new(
            admin_store,
            task_service,
            PathBuf::from("frontend/harbor-assistant/dist/harbor-assistant"),
            "http://harborbeacon.local:4174".to_string(),
        );
        (api, vec![admin_path, registry_path, conversation_path])
    }

    fn cleanup_test_paths(paths: &[PathBuf]) {
        for path in paths {
            let _ = fs::remove_file(path);
        }
    }

    fn response_json(
        response: tiny_http::Response<std::io::Cursor<Vec<u8>>>,
    ) -> (StatusCode, Value) {
        let status = response.status_code();
        let mut reader = response.into_reader();
        let mut text = String::new();
        reader.read_to_string(&mut text).expect("read response");
        let value = serde_json::from_str(&text).expect("json response");
        (status, value)
    }

    fn sample_home_guardian_review(status: &str) -> AutomationRuleReview {
        AutomationRuleReview {
            review_id: "guardian_review_shape_1".to_string(),
            workspace_id: "home-1".to_string(),
            source: "home_guardian".to_string(),
            source_channel: Some("admin-api-test".to_string()),
            source_conversation_id: Some("conv-shape".to_string()),
            original_prompt: "notify me when the front door sees a person".to_string(),
            status: status.to_string(),
            trigger_definition: Some(json!({
                "camera_id": "front-door",
                "event_type": "person_detected",
                "labels": ["person"],
                "min_confidence": 0.75,
            })),
            condition_definition: None,
            action_plan: Some(json!({
                "actions": [{"kind": "notify_default_target"}],
                "metadata_only": true
            })),
            device_refs: Vec::new(),
            risk_level: Some("low".to_string()),
            requires_approval: true,
            created_at: Some("1700000000".to_string()),
            updated_at: None,
            expires_at: None,
            rule_id: Some("guardian_rule_shape_1".to_string()),
            run_summaries: Vec::new(),
            metadata: Some(json!({"feature": "home_guardian", "home_guardian": true})),
        }
    }

    fn sample_local_vision_event(event_id: &str) -> LocalVisionEvent {
        let started_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_millis()
            .to_string();
        LocalVisionEvent {
            event_id: event_id.to_string(),
            camera_id: "front-door".to_string(),
            event_type: "person_detected".to_string(),
            confidence: 0.92,
            labels: vec!["person".to_string(), "door".to_string()],
            summary: "Person near front door".to_string(),
            snapshot_artifact: SnapshotArtifact {
                artifact_id: Some("artifact-shape-1".to_string()),
                path: None,
                mime_type: Some("image/jpeg".to_string()),
                byte_size: Some(1234),
                sha256: Some("abcd".to_string()),
                source: Some("fixture".to_string()),
            },
            started_at,
            analyzer: "fixture-yolo".to_string(),
            latency_ms: 64,
            metrics: json!({}),
            vlm: None,
        }
    }

    fn sample_stored_local_vision_event(event_id: &str) -> StoredLocalVisionEvent {
        StoredLocalVisionEvent {
            received_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_millis()
                .to_string(),
            event: sample_local_vision_event(event_id),
            audit_record: json!({"metadata_only": true}),
            ha_mqtt_payload: json!({"metadata_only": true}),
        }
    }

    #[test]
    fn home_guardian_evaluation_queue_drops_when_full() {
        let (sender, _receiver) = sync_channel(1);
        let queue = HomeGuardianEvaluationQueue::new(sender);

        assert_eq!(
            queue.enqueue(sample_stored_local_vision_event("queued-1")),
            "queued"
        );
        assert_eq!(
            queue.enqueue(sample_stored_local_vision_event("queued-2")),
            "dropped"
        );

        let snapshot = queue.snapshot();
        assert_eq!(snapshot["queued_total"], json!(1));
        assert_eq!(snapshot["dropped_total"], json!(1));
        assert_eq!(snapshot["bounded"], json!(true));
    }

    #[test]
    fn service_overloaded_response_is_metadata_only() {
        let (status, body) = response_json(service_overloaded_response());

        assert_eq!(status, StatusCode(503));
        assert_eq!(body["status"], json!("blocked"));
        assert_eq!(body["metadata_only"], json!(true));
        assert_eq!(body["secret_scan"], json!("clean"));
        let text = serde_json::to_string(&body).expect("serialize");
        assert!(!text.contains("rtsp://"));
        assert!(!text.contains("token"));
        assert!(!text.contains("route_key"));
    }

    #[test]
    fn vision_ingest_limiter_blocks_after_max_inflight() {
        let limiter = VisionIngestLimiter::new();
        let mut guards = Vec::new();
        for _ in 0..MAX_VISION_EVENT_INGEST_INFLIGHT {
            guards.push(limiter.try_acquire().expect("acquire below limit"));
        }

        assert!(limiter.try_acquire().is_none());
        assert_eq!(
            limiter.snapshot()["active_inflight"],
            json!(MAX_VISION_EVENT_INGEST_INFLIGHT)
        );
        assert_eq!(limiter.snapshot()["rejected_total"], json!(1));

        drop(guards);
        assert!(limiter.try_acquire().is_some());
    }

    #[test]
    fn home_guardian_rule_cache_tracks_active_reviews_only() {
        let (api, paths) = build_test_admin_api("guardian-rule-cache");
        api.admin_store
            .upsert_automation_review(sample_home_guardian_review("pending"))
            .expect("save pending");
        api.refresh_home_guardian_rule_cache()
            .expect("refresh pending");
        assert_eq!(
            api.guardian_rule_cache_snapshot()["active_rule_count"],
            json!(0)
        );

        api.admin_store
            .set_automation_review_status("guardian_review_shape_1", "active")
            .expect("activate");
        api.refresh_home_guardian_rule_cache()
            .expect("refresh active");
        assert_eq!(
            api.guardian_rule_cache_snapshot()["active_rule_count"],
            json!(1)
        );

        cleanup_test_paths(&paths);
    }

    #[test]
    fn home_guardian_auto_evaluation_does_not_persist_run_summaries() {
        let (api, paths) = build_test_admin_api("guardian-auto-no-hot-write");
        api.admin_store
            .upsert_automation_review(sample_home_guardian_review("active"))
            .expect("save active");
        api.refresh_home_guardian_rule_cache()
            .expect("refresh cache");

        let stored = sample_stored_local_vision_event("auto-no-hot-write-event");
        let response = api
            .evaluate_home_guardian_rules_for_event(&stored, None, true)
            .expect("auto evaluate");
        assert_eq!(response.metadata_only, true);

        let state = api.admin_store.load_state().expect("reload state");
        let review = state
            .automation_reviews
            .iter()
            .find(|review| review.review_id == "guardian_review_shape_1")
            .expect("review");
        assert_eq!(review.run_summaries.len(), 0);

        cleanup_test_paths(&paths);
    }

    #[test]
    fn home_guardian_action_cooldown_is_rule_camera_action_scoped() {
        let (api, paths) = build_test_admin_api("guardian-cooldown");
        let review = sample_home_guardian_review("active");
        let stored = sample_stored_local_vision_event("cooldown-event-1");
        let now = current_epoch_secs();

        assert!(!api.guardian_action_in_cooldown(&review, &stored, "notify_default_target", now));
        api.mark_guardian_action_cooldown(&review, &stored, "notify_default_target", now);

        assert!(api.guardian_action_in_cooldown(
            &review,
            &stored,
            "notify_default_target",
            now + 1
        ));
        assert!(!api.guardian_action_in_cooldown(
            &review,
            &stored,
            "home_assistant:light.turn_on",
            now + 1
        ));
        assert!(!api.guardian_action_in_cooldown(
            &review,
            &stored,
            "notify_default_target",
            now + HOME_GUARDIAN_ACTION_COOLDOWN_SECONDS + 1
        ));

        cleanup_test_paths(&paths);
    }

    #[test]
    fn home_guardian_auto_evaluation_coalesces_same_camera_event_type() {
        let (api, paths) = build_test_admin_api("guardian-auto-coalesce");
        let mut stored = sample_stored_local_vision_event("auto-event-1");
        let now = current_epoch_secs();

        assert!(api.guardian_auto_evaluation_due(&stored, now));
        assert!(!api.guardian_auto_evaluation_due(&stored, now + 1));
        assert!(api.guardian_auto_evaluation_due(
            &stored,
            now + HOME_GUARDIAN_AUTO_EVALUATION_MIN_INTERVAL_SECONDS + 1
        ));

        stored.event.event_type = "vehicle_detected".to_string();
        assert!(api.guardian_auto_evaluation_due(&stored, now + 2));

        cleanup_test_paths(&paths);
    }

    struct EnvGuard {
        key: String,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            unsafe {
                std::env::set_var(key, value);
            }
            Self {
                key: key.to_string(),
                original,
            }
        }

        fn remove(key: &str) -> Self {
            let original = std::env::var(key).ok();
            unsafe {
                std::env::remove_var(key);
            }
            Self {
                key: key.to_string(),
                original,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(value) => unsafe {
                    std::env::set_var(&self.key, value);
                },
                None => unsafe {
                    std::env::remove_var(&self.key);
                },
            }
        }
    }

    fn build_manual_add_request(ip: &str) -> ManualAddRequest {
        ManualAddRequest {
            name: "Test Camera".to_string(),
            room: None,
            ip: ip.to_string(),
            path: None,
            snapshot_url: None,
            username: None,
            password: None,
            port: None,
        }
    }

    #[test]
    fn home_assistant_service_smoke_blocks_unsafe_calls() {
        let mut state = HomeAssistantAdminState {
            enabled: true,
            base_url: "http://homeassistant.local:8123".to_string(),
            access_token: "secret-token".to_string(),
            exposed_domains: vec!["light".to_string(), "switch".to_string()],
            ..Default::default()
        };
        let allowed = HomeAssistantServiceSmokeRequest {
            entity_id: "light.kitchen".to_string(),
            domain: "light".to_string(),
            service: "turn_on".to_string(),
            fields: json!({}),
        };
        validate_home_assistant_service_smoke(&allowed, &state).expect("light turn_on is safe");

        let denied = HomeAssistantServiceSmokeRequest {
            entity_id: "homeassistant.restart".to_string(),
            domain: "homeassistant".to_string(),
            service: "restart".to_string(),
            fields: json!({}),
        };
        let error =
            validate_home_assistant_service_smoke(&denied, &state).expect_err("restart blocked");
        assert!(error.contains("allowlisted") || error.contains("scope"));

        state.exposed_domains = vec!["sensor".to_string()];
        let error =
            validate_home_assistant_service_smoke(&allowed, &state).expect_err("scope blocked");
        assert!(error.contains("sync scope"));
    }

    #[test]
    fn home_assistant_service_action_fields_reject_secret_like_values() {
        validate_home_assistant_service_fields(&json!({
            "brightness": 80,
            "transition": 1,
        }))
        .expect("low-risk fields are accepted");

        let error = validate_home_assistant_service_fields(&json!({
            "token": "secret",
        }))
        .expect_err("secret-like key blocked");
        assert!(error.contains("secret-like keys"));

        let error = validate_home_assistant_service_fields(&json!("not-object"))
            .expect_err("non-object fields blocked");
        assert!(error.contains("JSON object"));
    }

    #[test]
    fn local_vision_event_notify_path_decodes_event_id() {
        assert_eq!(
            parse_local_vision_event_notify_path("/api/vision/events/event%3A1/notify"),
            Some("event:1".to_string())
        );
        assert_eq!(
            parse_local_vision_event_notify_path("/api/vision/events/event/1/notify"),
            None
        );
    }

    #[test]
    fn notification_target_readiness_redacts_route_key() {
        let targets = vec![NotificationTargetRecord {
            target_id: "target-1".to_string(),
            label: "Family".to_string(),
            route_key: "gw_route_secret".to_string(),
            platform_hint: "weixin".to_string(),
            is_default: true,
        }];
        let readiness = build_default_notification_target_readiness(
            &targets,
            Some(&json!({
                "configured": true,
                "connected": true,
                "delivery_observability": { "record_count": 2 },
            })),
        );
        let text = serde_json::to_string(&readiness).expect("serialize readiness");

        assert_eq!(readiness["status"], json!("available"));
        assert_eq!(readiness["target_label"], json!("Family"));
        assert_eq!(readiness["route_key_redacted"], json!(true));
        assert!(!text.contains("gw_route_secret"));
    }

    #[test]
    fn family_timeline_admin_handlers_preserve_shape_and_redaction() {
        let (api, paths) = build_test_admin_api("harborbeacon-family-timeline-shape");
        let event_store_path = unique_store_path("harborbeacon-family-timeline-events");
        let _vision_store_guard = EnvGuard::set(
            VISION_EVENT_STORE_PATH_ENV,
            &event_store_path.to_string_lossy(),
        );
        ingest_local_vision_event(
            &event_store_path,
            sample_local_vision_event("event-family-1"),
        )
        .expect("ingest event");
        let hints = AccessIdentityHints::default();

        let (status, body) = response_json(api.handle_family_timeline(
            "/api/family/timeline?camera_id=front-door&label=person&limit=10&window_seconds=3600",
            &hints,
        ));
        assert_eq!(status, StatusCode(200));
        assert_eq!(body["metadata_only"], json!(true));
        assert_eq!(body["event_count"], json!(1));
        assert_eq!(body["events"][0]["event_id"], json!("event-family-1"));
        assert_eq!(
            body["events"][0]["artifact"]["local_path_redacted"],
            json!(true)
        );

        let (status, digest_body) = response_json(api.handle_family_timeline_digest(
            "/api/family/timeline/digest?window_seconds=3600",
            &hints,
        ));
        assert_eq!(status, StatusCode(200));
        assert_eq!(digest_body["metadata_only"], json!(true));
        assert_eq!(digest_body["secret_scan"], json!("clean"));
        assert!(digest_body["headline"]
            .as_str()
            .unwrap_or_default()
            .contains("1 小时"));
        assert!(api.last_family_timeline_digest().is_some());

        let text = serde_json::to_string(&(body, digest_body)).expect("serialize");
        assert!(!text.contains("/tmp/admin-family-shape.jpg"));
        assert!(!text.contains("rtsp://"));

        let _ = fs::remove_file(event_store_path);
        cleanup_test_paths(&paths);
    }

    #[test]
    fn home_guardian_admin_evaluate_latest_shape_keeps_pending_metadata_only() {
        let (api, paths) = build_test_admin_api("harborbeacon-guardian-evaluate-shape");
        api.admin_store
            .upsert_automation_review(sample_home_guardian_review("pending"))
            .expect("save review");
        let stored = sample_stored_local_vision_event("event-guardian-1");

        let response = api
            .evaluate_home_guardian_rules_for_event(&stored, Some("guardian_review_shape_1"), true)
            .expect("evaluate guardian");
        let body = serde_json::to_value(&response).expect("serialize response");

        assert_eq!(body["status"], json!("evaluated"));
        assert_eq!(body["metadata_only"], json!(true));
        assert_eq!(body["secret_scan"], json!("clean"));
        assert_eq!(body["results"][0]["status"], json!("skipped"));
        assert_eq!(body["results"][0]["matched"], json!(true));
        assert_eq!(body["results"][0]["executed"], json!(false));
        assert_eq!(body["results"][0]["actions"], json!([]));

        let state = api
            .admin_store
            .load_or_create_state()
            .expect("reload state");
        let review = state
            .automation_reviews
            .iter()
            .find(|review| review.review_id == "guardian_review_shape_1")
            .expect("review");
        assert_eq!(review.run_summaries.len(), 1);

        let text = serde_json::to_string(&body).expect("serialize body");
        assert!(!text.contains("/tmp/admin-family-shape.jpg"));
        assert!(!text.contains("token=secret"));

        cleanup_test_paths(&paths);
    }

    #[test]
    fn local_vision_notify_response_redacts_target_route_key() {
        let stored = sample_stored_local_vision_event("event-notify-1");
        let target = NotificationTargetRecord {
            target_id: "target-1".to_string(),
            label: "Family".to_string(),
            route_key: "gw_route_secret".to_string(),
            platform_hint: "weixin".to_string(),
            is_default: true,
        };
        let response = build_local_vision_event_notification_blocked_response(
            &stored,
            Some(&target),
            "blocked because token=secret and /tmp/admin-family-shape.jpg",
        );
        let text = serde_json::to_string(&response).expect("serialize response");

        assert_eq!(response.status, "blocked");
        assert_eq!(response.target_label.as_deref(), Some("Family"));
        assert_eq!(response.platform_hint.as_deref(), Some("weixin"));
        assert_eq!(response.audit_record["metadata_only"], json!(true));
        assert_eq!(response.audit_record["secret_scan"], json!("clean"));
        assert!(!text.contains("gw_route_secret"));
        assert!(!text.contains("token=secret"));
        assert!(!text.contains("/tmp/admin-family-shape.jpg"));
    }

    #[test]
    fn routing_status_admin_handler_keeps_routing_and_gate_boundary_separate() {
        let (api, paths) = build_test_admin_api("harborbeacon-routing-status-shape");
        let hints = AccessIdentityHints::default();

        let (status, body) = response_json(api.handle_routing_status(&hints));
        assert_eq!(status, StatusCode(200));
        assert_eq!(body["kind"], json!("harborbeacon.routing_status.v1"));
        assert_eq!(body["metadata_only"], json!(true));
        assert_eq!(body["scope"], json!("beacon_internal_orchestration"));
        assert!(body["execution_routes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|route| route["domain_id"] == json!("harboros_system")));
        assert!(body["boundaries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|boundary| boundary["boundary_id"] == json!("im_gateway_route_registry")));
        assert_eq!(
            normalize_unified_admin_path("/api/beacon/routing/status"),
            "/api/routing/status"
        );
        assert!(is_admin_surface_path("/api/routing/status"));

        let text = serde_json::to_string(&body).expect("serialize routing status");
        assert!(!text.contains("gw_route_"));
        assert!(!text.contains("args.resume_token"));

        cleanup_test_paths(&paths);
    }

    #[test]
    fn audit_records_admin_handler_filters_paginates_and_redacts() {
        let (api, paths) = build_test_admin_api("harborbeacon-audit-records-shape");
        api.admin_store
            .append_audit_record(build_metadata_audit_record(
                "home-1",
                "model_route_policy",
                "bulk",
                "model.route_policy.save",
                AuditActorKind::User,
                "local-owner",
                json!({
                    "route_key": "gw_route_secret",
                    "model_path": "C:\\models\\secret\\tokenizer.json",
                    "token": "secret",
                }),
                json!({"status": "saved"}),
            ))
            .expect("append audit");
        api.admin_store
            .append_audit_record(build_metadata_audit_record(
                "home-1",
                "vision_event",
                "event-audit-1",
                "vision_event.notify",
                AuditActorKind::User,
                "local-owner",
                json!({"event_id": "event-audit-1"}),
                json!({"status": "blocked"}),
            ))
            .expect("append audit");

        let hints = AccessIdentityHints::default();
        let (status, body) = response_json(api.handle_audit_records(
            "/api/audit/records?limit=1&action=model.route_policy.save",
            &hints,
        ));
        assert_eq!(status, StatusCode(200));
        assert_eq!(body["metadata_only"], json!(true));
        assert_eq!(body["secret_scan"], json!("clean"));
        assert_eq!(body["total"], json!(1));
        assert_eq!(
            body["records"][0]["action"],
            json!("model.route_policy.save")
        );
        assert!(is_admin_surface_path("/api/audit/records"));
        assert_eq!(
            normalize_unified_admin_path("/api/beacon/audit/records"),
            "/api/audit/records"
        );

        let text = serde_json::to_string(&body).expect("serialize audit page");
        assert!(!text.contains("gw_route_secret"));
        assert!(!text.contains("C:\\models"));
        assert!(!text.contains("tokenizer"));
        assert!(text.contains("metadata_only"));

        let (status, summary) =
            response_json(api.handle_audit_summary("/api/audit/summary?window=24h", &hints));
        assert_eq!(status, StatusCode(200));
        assert_eq!(summary["total"], json!(2));
        assert_eq!(summary["by_action"]["vision_event.notify"], json!(1));
        assert!(is_admin_surface_path("/api/audit/summary"));

        cleanup_test_paths(&paths);
    }

    #[test]
    fn audit_records_admin_handler_clamps_and_filters_entity_id_with_safe_parse_fallbacks() {
        let (api, paths) = build_test_admin_api("harborbeacon-audit-records-filter-console");
        for index in 0..3 {
            api.admin_store
                .append_audit_record(build_metadata_audit_record(
                    "home-1",
                    if index == 1 {
                        "home_guardian"
                    } else {
                        "vision_event"
                    },
                    if index == 1 {
                        "rule-front"
                    } else {
                        "event-front"
                    },
                    if index == 1 {
                        "home_guardian.evaluate_latest"
                    } else {
                        "vision_event.notify"
                    },
                    AuditActorKind::User,
                    "local-owner",
                    json!({
                        "entity_id": if index == 1 { "rule-front" } else { "event-front" },
                        "raw_path": "/mnt/pool/private/frame.jpg",
                        "route_key": "gw_route_should_not_escape",
                    }),
                    json!({"status": "recorded"}),
                ))
                .expect("append audit");
        }

        let hints = AccessIdentityHints::default();
        let (status, body) = response_json(api.handle_audit_records(
            "/api/audit/records?limit=999&cursor=bogus&entity_kind=home_guardian&entity_id=rule-front&action=home_guardian.evaluate_latest",
            &hints,
        ));
        assert_eq!(status, StatusCode(200));
        assert_eq!(body["limit"], json!(200));
        assert_eq!(body["cursor"], Value::Null);
        assert_eq!(body["total"], json!(1));
        assert_eq!(body["records"][0]["entity_id"], json!("rule-front"));
        assert_eq!(
            body["records"][0]["action"],
            json!("home_guardian.evaluate_latest")
        );

        let text = serde_json::to_string(&body).expect("serialize audit page");
        assert!(!text.contains("/mnt/pool/private"));
        assert!(!text.contains("gw_route_should_not_escape"));
        assert!(!text.contains("route_key"));
        assert!(text.contains("opaque_delivery_route_present"));

        let (status, body) = response_json(api.handle_audit_records(
            "/api/audit/records?limit=1&cursor=1&action=vision_event.notify",
            &hints,
        ));
        assert_eq!(status, StatusCode(200));
        assert_eq!(body["limit"], json!(1));
        assert_eq!(body["cursor"], json!("1"));
        assert_eq!(body["total"], json!(2));
        assert_eq!(body["records"].as_array().unwrap().len(), 1);
        assert_eq!(body["next_cursor"], Value::Null);

        cleanup_test_paths(&paths);
    }

    #[test]
    fn home_assistant_operator_audit_is_metadata_only() {
        let request = HomeAssistantServiceSmokeRequest {
            entity_id: "light.kitchen".to_string(),
            domain: "light".to_string(),
            service: "turn_on".to_string(),
            fields: json!({}),
        };

        let audit = build_home_assistant_operator_audit(
            "home_assistant.service_smoke_executed",
            "succeeded",
            true,
            true,
            "ok token=secret rtsp://admin:secret@camera.local/stream",
            &request,
        );
        let text = serde_json::to_string(&audit).expect("serialize audit");

        assert_eq!(audit["metadata_only"], json!(true));
        assert_eq!(audit["secret_scan"], json!("clean"));
        assert!(!text.contains("token=secret"));
        assert!(!text.contains("admin:secret"));
        assert!(text.contains("redacted"));
    }

    #[test]
    fn inference_health_alias_redacts_runtime_probe_error() {
        let response = build_inference_health_alias_response(&LocalModelRuntimeProjection {
            error: Some("failed token=secret api_key=abc".to_string()),
            ..Default::default()
        });
        let text = serde_json::to_string(&response).expect("serialize health");

        assert_eq!(response.status, "degraded");
        assert!(!text.contains("token=secret"));
        assert!(!text.contains("api_key=abc"));
    }

    #[test]
    fn redacted_diagnostics_bundle_excludes_secret_material() {
        let admin_path = unique_store_path("harborbeacon-diagnostics-state");
        let registry_path = unique_store_path("harborbeacon-diagnostics-registry");
        let registry_store = DeviceRegistryStore::new(registry_path.clone());
        let admin_store = AdminConsoleStore::new(admin_path.clone(), registry_store);
        let state = CameraHubService::new(admin_store)
            .state_snapshot(None)
            .expect("state snapshot");
        let home_assistant = super::HomeAssistantStatusResponse {
            configured: true,
            enabled: true,
            base_url: "http://homeassistant.local:8123".to_string(),
            token_configured: true,
            token_redacted: true,
            exposed_domains: vec!["light".to_string()],
            status: "connected".to_string(),
            last_error: Some("token=secret".to_string()),
            last_test_at: None,
            last_sync_at: None,
            entity_count: 1,
            service_count: 1,
            version: Some("2026.6".to_string()),
            location_name: Some("Home".to_string()),
        };
        let bundle = build_redacted_diagnostics_bundle(
            &state,
            &home_assistant,
            &LocalModelRuntimeProjection::default(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            json!({
                "queued_total": 0,
                "dropped_total": 0,
                "metadata_only": true,
            }),
            json!({
                "http": {"active_requests": 0, "metadata_only": true},
                "vision_ingest": {"active_inflight": 0, "metadata_only": true},
                "metadata_only": true,
            }),
            json!({
                "exists": true,
                "file_bytes": 0,
                "automation_review_count": 0,
                "audit_record_count": 0,
                "metadata_only": true,
            }),
            json!({
                "status": "not_run",
                "redacted": true,
            }),
            json!({
                "status": "not_run",
                "redacted": true,
            }),
            json!({
                "status": "not_run",
                "redacted": true,
            }),
            json!({
                "kind": "evt_readiness_v1",
                "profile": "k3-direct-72h-readiness",
                "status": "degraded",
                "blockers": [],
                "warnings": ["test"],
                "redacted": true,
            }),
            Some(json!({
                "kind": "evt_preflight_v1",
                "profile": "k3-direct-72h-readiness",
                "status": "degraded",
                "long_run_started": false,
                "short_run_started": false,
                "redacted": true,
            })),
            &[],
            None,
        );
        let text = serde_json::to_string(&bundle).expect("serialize bundle");

        assert_eq!(bundle.security["secret_scan"], json!("clean"));
        assert_eq!(
            bundle.workflow["general_message_nsp_route"]["status"],
            json!("not_run")
        );
        assert_eq!(
            bundle.workflow["evt_readiness"]["status"],
            json!("degraded")
        );
        assert_eq!(
            bundle.workflow["evt_preflight"]["long_run_started"],
            json!(false)
        );
        assert!(!text.contains("token=secret"));
        assert!(!text.contains("rtsp://"));
        assert!(!text.contains("api_key=abc"));

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
    }

    #[test]
    fn nsp_route_diagnostics_redacts_local_paths() {
        let step = harborbeacon_local_agent::control_plane::tasks::TaskStepRun {
            step_id: "step-1".to_string(),
            task_id: "turn-1".to_string(),
            domain: "general".to_string(),
            operation: "message".to_string(),
            status: harborbeacon_local_agent::control_plane::tasks::TaskStepRunStatus::Success,
            output_payload: json!({
                "data": {
                    "general_message_controller": {
                        "controller_stage": "nsp_router",
                        "nsp_route": {
                            "decision": null,
                            "confidence": null,
                            "schema_valid": false,
                            "local_only": true,
                            "fallback_reason": "missing /mnt/software/harborbeacon-agent-ci/model-store/runtimes/harbor-candle/bootstrap-llm/tokenizer.json and C:\\\\models\\\\secret\\\\tokenizer.json token=secret"
                        }
                    }
                }
            }),
            ..Default::default()
        };

        let summary = redacted_general_message_nsp_route_summary(&step).expect("summary");
        let text = serde_json::to_string(&summary).expect("serialize");

        assert_eq!(summary["stage"], json!("nsp_router"));
        assert!(text.contains("[redacted local path]"));
        assert!(!text.contains("/mnt/software"));
        assert!(!text.contains("C:\\\\models"));
        assert!(!text.contains("token=secret"));
    }

    #[test]
    fn admin_string_redaction_preserves_plain_http_origin() {
        let redacted = redact_admin_string("probe http://homeassistant.local:8123 ok");

        assert!(redacted.contains("http://homeassistant.local:8123"));
    }

    #[test]
    fn admin_string_redaction_preserves_public_shared_camera_route() {
        let redacted = redact_admin_string(
            "share=/shared/cameras/token local=/mnt/software/secret.bin win=C:\\models\\secret.bin",
        );

        assert!(redacted.contains("/shared/cameras/token"));
        assert!(!redacted.contains("/mnt/software"));
        assert!(!redacted.contains("C:\\models"));
        assert!(redacted.contains("[redacted local path]"));
    }

    #[test]
    fn home_assistant_task_api_workflow_summary_is_metadata_only() {
        let event = EventRecord {
            event_id: "event-1".to_string(),
            workspace_id: "home-1".to_string(),
            source_id: "task-1".to_string(),
            event_type: "home_assistant.service_action_clarification_required".to_string(),
            payload: json!({
                "status": "needs_input",
                "domain": "scene",
                "service": "turn_on",
                "candidate_count": 2,
                "reason": "ambiguous_entity",
                "message": "token=secret Bearer abc rtsp://camera.local/stream",
            }),
            occurred_at: Some("1710000000".to_string()),
            ..Default::default()
        };

        let summary =
            redacted_home_assistant_task_api_event_summary(&event).expect("summary event");
        let text = serde_json::to_string(&summary).expect("serialize summary");

        assert_eq!(summary["status"], json!("needs_input"));
        assert_eq!(summary["domain"], json!("scene"));
        assert_eq!(summary["service"], json!("turn_on"));
        assert_eq!(summary["candidate_count"], json!(2));
        assert_eq!(summary["redacted"], json!(true));
        assert!(!text.contains("token=secret"));
        assert!(!text.contains("Bearer abc"));
        assert!(!text.contains("rtsp://"));
    }

    #[test]
    fn owner_and_admin_manual_add_skip_camera_connect_approval_queue() {
        for (role_kind, user_id) in [
            (RoleKind::Owner, "local-owner"),
            (RoleKind::Admin, "admin-1"),
        ] {
            let admin_path = unique_store_path("harborbeacon-manual-add-state");
            let registry_path = unique_store_path("harborbeacon-manual-add-registry");
            let conversation_path = unique_store_path("harborbeacon-manual-add-runtime");
            let registry_store = DeviceRegistryStore::new(registry_path.clone());
            let admin_store = AdminConsoleStore::new(admin_path.clone(), registry_store);
            let conversation_store = TaskConversationStore::new(conversation_path.clone());
            let task_service = TaskApiService::new(admin_store.clone(), conversation_store.clone());
            let api = AdminApi::new(
                admin_store,
                task_service,
                PathBuf::from("frontend/harbor-assistant/dist/harbor-assistant"),
                "http://harborbeacon.local:4174".to_string(),
            );

            let error = api
                .manual_add(
                    &AccessPrincipal {
                        workspace_id: "home-1".to_string(),
                        user_id: user_id.to_string(),
                        display_name: user_id.to_string(),
                        role_kind,
                    },
                    build_manual_add_request(""),
                )
                .expect_err("owner/admin manual add should fail validation before approval");

            assert_eq!(error, "IP 地址不能为空");
            assert!(conversation_store
                .pending_approvals()
                .expect("load pending approvals")
                .is_empty());

            let _ = fs::remove_file(admin_path);
            let _ = fs::remove_file(registry_path);
            let _ = fs::remove_file(conversation_path);
        }
    }

    #[test]
    fn operator_manual_add_still_routes_into_camera_connect_approval_queue() {
        let admin_path = unique_store_path("harborbeacon-operator-add-state");
        let registry_path = unique_store_path("harborbeacon-operator-add-registry");
        let conversation_path = unique_store_path("harborbeacon-operator-add-runtime");
        let registry_store = DeviceRegistryStore::new(registry_path.clone());
        let admin_store = AdminConsoleStore::new(admin_path.clone(), registry_store);
        let conversation_store = TaskConversationStore::new(conversation_path.clone());
        let task_service = TaskApiService::new(admin_store.clone(), conversation_store.clone());
        let api = AdminApi::new(
            admin_store,
            task_service,
            PathBuf::from("frontend/harbor-assistant/dist/harbor-assistant"),
            "http://harborbeacon.local:4174".to_string(),
        );

        let error = api
            .manual_add(
                &AccessPrincipal {
                    workspace_id: "home-1".to_string(),
                    user_id: "operator-1".to_string(),
                    display_name: "operator-1".to_string(),
                    role_kind: RoleKind::Operator,
                },
                build_manual_add_request(""),
            )
            .expect_err("operator manual add should still require approval");

        assert!(error.contains("approval_token"));
        let approvals = conversation_store
            .pending_approvals()
            .expect("load pending approvals");
        assert_eq!(approvals.len(), 1);
        assert_eq!(approvals[0].policy_ref, "camera.connect");
        assert_eq!(approvals[0].requester_user_id, "operator-1");

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    #[test]
    fn operator_camera_workspace_authorization_reaches_manual_add_approval_path() {
        let admin_path = unique_store_path("harborbeacon-operator-http-state");
        let registry_path = unique_store_path("harborbeacon-operator-http-registry");
        let conversation_path = unique_store_path("harborbeacon-operator-http-runtime");
        let registry_store = DeviceRegistryStore::new(registry_path.clone());
        let admin_store = AdminConsoleStore::new(admin_path.clone(), registry_store);
        let conversation_store = TaskConversationStore::new(conversation_path.clone());
        let task_service = TaskApiService::new(admin_store.clone(), conversation_store.clone());

        let mut state = admin_store.load_or_create_state().expect("state");
        state.platform.users.push(
            harborbeacon_local_agent::control_plane::users::UserAccount {
                user_id: "operator-1".to_string(),
                display_name: "operator-1".to_string(),
                email: None,
                phone: None,
                status: harborbeacon_local_agent::control_plane::users::UserStatus::Active,
                default_workspace_id: Some("home-1".to_string()),
                preferences: json!({
                    "auth_source": "harbor_os",
                    "channel": "harbor_os",
                }),
            },
        );
        state.platform.memberships.push(
            harborbeacon_local_agent::control_plane::users::Membership {
                membership_id: "membership-operator-1".to_string(),
                workspace_id: "home-1".to_string(),
                user_id: "operator-1".to_string(),
                role_kind: RoleKind::Operator,
                status: MembershipStatus::Active,
                granted_by_user_id: Some("local-owner".to_string()),
                granted_at: None,
            },
        );
        fs::write(
            &admin_path,
            serde_json::to_vec_pretty(&state).expect("serialize state"),
        )
        .expect("write state");

        let api = AdminApi::new(
            admin_store,
            task_service,
            PathBuf::from("frontend/harbor-assistant/dist/harbor-assistant"),
            "http://harborbeacon.local:4174".to_string(),
        );
        let hints = AccessIdentityHints {
            user_id: Some("operator-1".to_string()),
            ..AccessIdentityHints::default()
        };

        let principal = api
            .authorize_workspace_camera_action(&hints)
            .expect("operator should be allowed to operate cameras");
        assert_eq!(principal.user_id, "operator-1");
        assert_eq!(principal.role_kind, RoleKind::Operator);
        assert!(api
            .authorize_admin_action(&hints, AccessAction::AdminManage)
            .is_err());

        let error = api
            .manual_add(&principal, build_manual_add_request(""))
            .expect_err("operator manual add should still require approval");
        assert!(error.contains("approval_token"));
        let approvals = conversation_store
            .pending_approvals()
            .expect("load pending approvals");
        assert_eq!(approvals.len(), 1);
        assert_eq!(approvals[0].policy_ref, "camera.connect");
        assert_eq!(approvals[0].requester_user_id, "operator-1");

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    #[test]
    fn camera_paths_decode_percent_encoded_device_ids() {
        let encoded = "camera%201%2Fleft";
        assert_eq!(
            parse_camera_snapshot_path(&format!("/api/cameras/{encoded}/snapshot.jpg")),
            Some("camera 1/left".to_string())
        );
        assert_eq!(
            parse_camera_task_snapshot_path(&format!("/api/cameras/{encoded}/snapshot")),
            Some("camera 1/left".to_string())
        );
        assert_eq!(
            parse_camera_share_link_path(&format!("/api/cameras/{encoded}/share-link")),
            Some("camera 1/left".to_string())
        );
        assert_eq!(
            parse_camera_analyze_path(&format!("/api/cameras/{encoded}/analyze")),
            Some("camera 1/left".to_string())
        );
        assert_eq!(
            parse_camera_live_stream_path(&format!("/api/cameras/{encoded}/live.mjpeg")),
            Some("camera 1/left".to_string())
        );
        assert_eq!(
            parse_camera_hls_live_start_path(&format!("/api/cameras/{encoded}/live/start")),
            Some("camera 1/left".to_string())
        );
        assert_eq!(
            parse_camera_hls_live_stop_path(&format!("/api/cameras/{encoded}/live/stop")),
            Some("camera 1/left".to_string())
        );
        assert_eq!(
            parse_camera_hls_live_status_path(&format!("/api/cameras/{encoded}/live/status")),
            Some("camera 1/left".to_string())
        );
        assert_eq!(
            parse_camera_hls_live_asset_path(&format!(
                "/api/cameras/{encoded}/live/live-abc123/index.m3u8"
            )),
            Some((
                "camera 1/left".to_string(),
                "live-abc123".to_string(),
                "index.m3u8".to_string()
            ))
        );
        assert_eq!(
            parse_camera_recording_start_path(&format!("/api/cameras/{encoded}/recordings/start")),
            Some("camera 1/left".to_string())
        );
        assert_eq!(
            parse_camera_recording_stop_path(&format!("/api/cameras/{encoded}/recordings/stop")),
            Some("camera 1/left".to_string())
        );
        assert_eq!(
            parse_camera_live_page_path(&format!("/live/cameras/{encoded}")),
            Some("camera 1/left".to_string())
        );
    }

    #[test]
    fn hls_live_asset_paths_reject_traversal() {
        assert!(is_safe_live_asset_name("index.m3u8"));
        assert!(is_safe_live_asset_name("segment_00001.m4s"));
        assert!(is_safe_live_asset_name("init.mp4"));
        assert!(!is_safe_live_asset_name("../index.m3u8"));
        assert!(!is_safe_live_asset_name("nested/index.m3u8"));
        assert!(!is_safe_live_asset_name("segment_00001.jpg"));
        assert_eq!(
            parse_camera_hls_live_asset_path(
                "/api/cameras/cam-1/live/live-abc123/..%2Fsecret.m3u8"
            ),
            None
        );
        assert_eq!(
            parse_camera_hls_live_asset_path(
                "/api/cameras/cam-1/live/live-abc123/%2e%2e%2Fsecret.m3u8"
            ),
            None
        );
        assert_eq!(
            parse_camera_hls_live_asset_path(
                "/api/cameras/cam-1/live/live-abc123/nested/index.m3u8"
            ),
            None
        );
    }

    #[test]
    fn shared_camera_paths_decode_tokens() {
        let encoded = "abc.def%2D123";
        assert_eq!(
            parse_shared_camera_live_page_path(&format!("/shared/cameras/{encoded}")),
            Some("abc.def-123".to_string())
        );
        assert_eq!(
            parse_shared_camera_live_stream_path(&format!("/shared/cameras/{encoded}/live.mjpeg")),
            Some("abc.def-123".to_string())
        );
        assert_eq!(
            parse_share_link_revoke_path("/api/share-links/share-link-1/revoke"),
            Some("share-link-1".to_string())
        );
    }

    #[test]
    fn device_admin_paths_decode_percent_encoded_device_ids() {
        let encoded = "camera%201%2Fleft";
        assert_eq!(
            parse_device_credentials_path(&format!("/api/devices/{encoded}/credentials")),
            Some("camera 1/left".to_string())
        );
        assert_eq!(
            parse_device_rtsp_check_path(&format!("/api/devices/{encoded}/rtsp-check")),
            Some("camera 1/left".to_string())
        );
        assert_eq!(
            parse_device_credential_status_path(&format!(
                "/api/devices/{encoded}/credential-status"
            )),
            Some("camera 1/left".to_string())
        );
    }

    #[test]
    fn device_credential_status_redacts_secret_projection() {
        let mut state = AdminConsoleState::default();
        state.device_credentials.push(DeviceCredentialSecret {
            device_id: "cam-1".to_string(),
            username: "admin".to_string(),
            password: "secret".to_string(),
            rtsp_port: Some(8554),
            rtsp_paths: vec!["/stream1".to_string(), "/stream2".to_string()],
            updated_at: Some("123".to_string()),
            last_verified_at: Some("456".to_string()),
        });
        let device = CameraDevice::new("cam-1", "Living Room", "rtsp://192.168.3.73/stream1");

        let status = build_device_credential_status(&state, &device);

        assert!(status.configured);
        assert!(status.redacted);
        assert_eq!(status.username.as_deref(), Some("admin"));
        assert_eq!(status.rtsp_port, Some(8554));
        assert_eq!(status.path_count, 2);
        assert_eq!(status.source, "device_rtsp");
    }

    #[test]
    fn camera_stream_url_prefers_device_credentials_for_runtime_use() {
        let mut state = AdminConsoleState::default();
        state.defaults.rtsp_username = "default".to_string();
        state.defaults.rtsp_password = "wrong".to_string();
        state.defaults.rtsp_port = 554;
        state.device_credentials.push(DeviceCredentialSecret {
            device_id: "cam-1".to_string(),
            username: "admin".to_string(),
            password: "secret".to_string(),
            rtsp_port: Some(8554),
            rtsp_paths: vec!["stream2".to_string(), "/stream1".to_string()],
            updated_at: Some("123".to_string()),
            last_verified_at: None,
        });
        let mut device = CameraDevice::new("cam-1", "Living Room", "rtsp://192.168.3.73/stream1");
        device.ip_address = Some("192.168.3.73".to_string());

        let url = camera_stream_url_with_credentials(&device, &state).expect("stream url");

        assert_eq!(url, "rtsp://admin:secret@192.168.3.73:8554/stream2");
    }

    #[test]
    fn scan_request_task_args_preserve_rtsp_credentials_for_worker() {
        let args = scan_request_task_args(&super::ScanRequest {
            cidr: Some("192.168.3.231/32".to_string()),
            protocol: Some("RTSP".to_string()),
            rtsp_port: Some(554),
            rtsp_username: Some("admin".to_string()),
            rtsp_password: Some("fresh-secret".to_string()),
        });

        assert_eq!(
            args.pointer("/rtsp_username").and_then(Value::as_str),
            Some("admin")
        );
        assert_eq!(
            args.pointer("/rtsp_password").and_then(Value::as_str),
            Some("fresh-secret")
        );
    }

    #[test]
    fn stream_url_redaction_removes_rtsp_credentials() {
        assert_eq!(
            redact_stream_url_credentials("rtsp://admin:secret@192.168.3.73:8554/stream1"),
            "rtsp://redacted:redacted@192.168.3.73:8554/stream1"
        );
        assert_eq!(
            redact_stream_url_credentials("rtsp://192.168.3.73/stream1"),
            "rtsp://192.168.3.73/stream1"
        );
    }

    #[test]
    fn stream_url_redaction_handles_embedded_urls() {
        assert_eq!(
            redact_stream_url_credentials(
                "primary=rtsp://admin:secret@192.168.3.73:8554/stream1 secondary=ok"
            ),
            "primary=rtsp://redacted:redacted@192.168.3.73:8554/stream1 secondary=ok"
        );
    }

    #[test]
    fn recursive_camera_task_redaction_removes_stream_credentials() {
        let mut value = json!({
            "camera_target": {
                "primary_stream": {
                    "url": "rtsp://admin:secret@192.168.3.73:8554/stream1"
                }
            },
            "share_link": {
                "url": "/shared/cameras/token"
            },
            "nested": [
                "rtsp://operator:password@camera.local/live",
                "plain text"
            ]
        });

        redact_value_stream_credentials(&mut value);

        assert_eq!(
            value["camera_target"]["primary_stream"]["url"],
            json!("rtsp://redacted:redacted@192.168.3.73:8554/stream1")
        );
        assert_eq!(value["share_link"]["url"], json!("/shared/cameras/token"));
        assert_eq!(
            value["nested"][0],
            json!("rtsp://redacted:redacted@camera.local/live")
        );
        assert_eq!(value["nested"][1], json!("plain text"));
    }

    #[test]
    fn camera_device_projection_redacts_stream_and_snapshot_urls() {
        let mut device = CameraDevice::new(
            "cam-secret",
            "Secret Camera",
            "rtsp://admin:secret@192.168.3.73:8554/stream1",
        );
        device.snapshot_url = Some("http://admin:secret@192.168.3.73/snapshot.jpg".to_string());

        redact_camera_device_projection(&mut device);

        assert_eq!(device.primary_stream.url, "__harbor_redacted_rtsp_url__");
        assert_eq!(
            device.snapshot_url.as_deref(),
            Some("http://redacted:redacted@192.168.3.73/snapshot.jpg")
        );
    }

    #[test]
    fn state_snapshot_redacts_device_stream_urls() {
        let registry_path = unique_store_path("harborbeacon-state-redaction-registry");
        let admin_path = unique_store_path("harborbeacon-state-redaction-state");
        let registry_store = DeviceRegistryStore::new(registry_path.clone());
        let admin_store = AdminConsoleStore::new(admin_path.clone(), registry_store.clone());
        let mut device = CameraDevice::new(
            "cam-secret",
            "Secret Camera",
            "rtsp://admin:secret@192.168.3.73:8554/stream1",
        );
        device.snapshot_url = Some("http://admin:secret@192.168.3.73/snapshot.jpg".to_string());
        registry_store
            .save_devices(&[device])
            .expect("save device registry");
        let state = CameraHubService::new(admin_store)
            .state_snapshot(Some("http://harborbeacon.local:4174"))
            .expect("state snapshot");

        let redacted = redact_state_snapshot(state);
        let payload = serde_json::to_string(&redacted).expect("serialize redacted state");

        assert!(!payload.contains("admin:secret"));
        assert!(!payload.contains("rtsp://"));
        assert!(payload.contains("__harbor_redacted_rtsp_url__"));
        assert!(payload.contains("http://redacted:redacted@192.168.3.73/snapshot.jpg"));

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
    }

    #[test]
    fn path_segment_round_trips_utf8_content() {
        let raw = "客厅 camera #1";
        let encoded = url_encode_path_segment(raw);
        assert_eq!(percent_decode_path_segment(&encoded).as_deref(), Ok(raw));
    }

    #[test]
    fn direct_camera_access_is_restricted_to_local_clients() {
        let local = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(192, 168, 3, 12), 4567));
        let remote = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(8, 8, 8, 8), 4567));
        assert!(ensure_local_camera_access(Some(local), &[]).is_ok());
        assert!(ensure_local_camera_access(Some(remote), &[]).is_err());
    }

    #[test]
    fn lan_forwarded_headers_allow_harboros_nginx_routes() {
        let forwarded = vec![
            Header::from_bytes(
                b"X-Forwarded-For".as_slice(),
                b"192.168.3.82, 127.0.0.1".as_slice(),
            )
            .expect("header"),
            Header::from_bytes(b"X-Forwarded-Host".as_slice(), b"192.168.3.82".as_slice())
                .expect("header"),
            Header::from_bytes(b"X-Forwarded-Proto".as_slice(), b"http".as_slice())
                .expect("header"),
        ];
        let local = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 4567));
        assert!(has_forwarding_headers(&forwarded));
        assert!(ensure_local_admin_access(Some(local), &forwarded).is_ok());
        assert!(ensure_local_camera_access(Some(local), &forwarded).is_ok());
    }

    #[test]
    fn forwarded_public_client_still_blocks_local_only_routes() {
        let forwarded = vec![Header::from_bytes(
            b"X-Forwarded-For".as_slice(),
            b"192.168.3.82, 198.51.100.10".as_slice(),
        )
        .expect("header")];
        let local = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 4567));
        assert!(ensure_local_admin_access(Some(local), &forwarded).is_err());
        assert!(ensure_local_camera_access(Some(local), &forwarded).is_err());
    }

    #[test]
    fn forwarded_for_header_accepts_private_rfc_syntax() {
        let forwarded = vec![Header::from_bytes(
            b"Forwarded".as_slice(),
            b"for=\"192.168.3.82\";proto=http;host=harboros".as_slice(),
        )
        .expect("header")];
        let local = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 4567));
        assert!(ensure_local_admin_access(Some(local), &forwarded).is_ok());
        assert!(ensure_local_camera_access(Some(local), &forwarded).is_ok());
    }

    #[test]
    fn bridge_provider_config_redacts_secret() {
        let redacted = redact_bridge_provider_config(
            harborbeacon_local_agent::runtime::admin_console::BridgeProviderConfig {
                configured: true,
                connected: true,
                platform: "feishu".to_string(),
                gateway_base_url: "http://gateway.local:4180".to_string(),
                app_id: "cli_xxx".to_string(),
                app_secret: "super-secret".to_string(),
                app_name: "HarborBeacon Bot".to_string(),
                bot_open_id: "ou_xxx".to_string(),
                status: "已连接".to_string(),
                last_checked_at: "2026-04-18T10:00:00Z".to_string(),
                capabilities:
                    harborbeacon_local_agent::runtime::admin_console::BridgeProviderCapabilities {
                        reply: true,
                        update: true,
                        attachments: true,
                    },
            },
        );
        assert_eq!(redacted.app_id, "");
        assert_eq!(redacted.app_secret, "");
        assert_eq!(redacted.bot_open_id, "");
    }

    #[test]
    fn live_bridge_provider_from_setup_status_prefers_connected_channel() {
        let payload = json!({
            "public_origin": "http://192.168.3.169:8787",
            "channels": [
                {
                    "platform": "webhook",
                    "enabled": true,
                    "connected": false,
                    "display_name": "Webhook",
                    "capabilities": {
                        "reply": true,
                        "update": false,
                        "attachments": true
                    }
                },
                {
                    "platform": "weixin",
                    "enabled": true,
                    "connected": true,
                    "display_name": "Weixin",
                    "capabilities": {
                        "reply": true,
                        "update": false,
                        "attachments": false
                    }
                }
            ]
        });

        let provider = live_bridge_provider_from_setup_status(&payload).expect("provider");
        assert!(provider.configured);
        assert!(provider.connected);
        assert_eq!(provider.platform, "weixin");
        assert_eq!(provider.app_name, "Weixin");
        assert_eq!(provider.status, "已连接");
    }

    #[test]
    fn bridge_provider_binding_projection_marks_connected_gateway() {
        let provider = harborbeacon_local_agent::runtime::admin_console::BridgeProviderConfig {
            configured: true,
            connected: true,
            platform: "weixin".to_string(),
            app_name: "Weixin".to_string(),
            ..Default::default()
        };
        let mut status = String::new();
        let mut metric = String::new();
        let mut bound_user = None;

        apply_bridge_provider_binding_projection(
            &mut status,
            &mut metric,
            &mut bound_user,
            &provider,
        );

        assert_eq!(status, "Gateway 已连接");
        assert_eq!(metric, "Gateway 在线");
        assert_eq!(bound_user, Some("Weixin".to_string()));
    }

    #[test]
    fn approval_decision_paths_decode_ids() {
        let encoded = "approval%2F1";
        assert_eq!(
            parse_approval_decision_path(
                &format!("/api/tasks/approvals/{encoded}/approve"),
                "approve"
            ),
            Some("approval/1".to_string())
        );
        assert_eq!(
            parse_approval_decision_path(
                &format!("/api/tasks/approvals/{encoded}/reject"),
                "reject"
            ),
            Some("approval/1".to_string())
        );
    }

    #[test]
    fn approval_routes_are_admin_surface_paths() {
        assert!(is_admin_surface_path("/api/tasks/approvals"));
        assert!(is_admin_surface_path("/api/access/members"));
        assert!(is_admin_surface_path("/api/account-management"));
        assert!(is_admin_surface_path("/api/gateway/status"));
        assert!(is_admin_surface_path("/api/knowledge/search"));
        assert!(is_admin_surface_path("/api/knowledge/preview"));
        assert!(is_admin_surface_path("/api/share-links"));
        assert!(is_admin_surface_path("/api/models/endpoints"));
        assert!(is_admin_surface_path("/api/models/policies"));
        assert!(is_admin_surface_path("/admin/models"));
        assert!(is_admin_surface_path("/api/models/endpoints/ocr-local"));
        assert!(is_admin_surface_path(
            "/api/models/endpoints/ocr-local/test"
        ));
        assert!(is_admin_surface_path("/api/access/members/user-1/role"));
        assert!(is_admin_surface_path(
            "/api/access/members/user-1/default-delivery-surface"
        ));
        assert!(is_admin_surface_path(
            "/api/tasks/approvals/approval-1/approve"
        ));
        assert!(is_admin_surface_path(
            "/api/tasks/approvals/approval-1/reject"
        ));
        assert!(is_admin_surface_path("/api/automation/reviews"));
        assert!(is_admin_surface_path(
            "/api/automation/reviews/review-1/discard"
        ));
        assert!(is_admin_surface_path("/api/cameras/camera-1/share-link"));
        assert!(is_admin_surface_path("/api/cameras/recording-settings"));
        assert!(is_admin_surface_path("/api/cameras/recordings/status"));
        assert!(is_admin_surface_path("/api/cameras/recordings/timeline"));
        assert!(is_admin_surface_path(
            "/api/cameras/camera-1/recordings/start"
        ));
        assert!(is_admin_surface_path(
            "/api/cameras/camera-1/recordings/stop"
        ));
        assert!(is_admin_surface_path(
            "/api/share-links/share-link-1/revoke"
        ));
        assert!(is_admin_surface_path("/api/cameras/camera-1/snapshot"));
        assert!(is_admin_surface_path("/api/cameras/camera-1/snapshot.jpg"));
        assert!(is_admin_surface_path("/api/cameras/camera-1/live.mjpeg"));
        assert!(is_admin_surface_path("/api/cameras/camera-1/analyze"));
        assert!(is_admin_surface_path("/api/home-assistant/status"));
        assert!(is_admin_surface_path("/api/home-assistant/config"));
        assert!(is_admin_surface_path(
            "/api/harboros/apps/home-assistant/install"
        ));
        assert!(is_admin_surface_path("/api/models/capabilities"));
    }

    #[test]
    fn beacon_api_prefix_normalizes_to_admin_api_routes() {
        assert_eq!(
            normalize_unified_admin_path("/api/beacon/cameras/recording-settings"),
            "/api/cameras/recording-settings"
        );
        assert_eq!(
            normalize_unified_admin_path("/api/beacon/cameras/camera-1/snapshot.jpg"),
            "/api/cameras/camera-1/snapshot.jpg"
        );
        assert_eq!(
            normalize_unified_admin_path("/api/beacon/models/capabilities"),
            "/api/models/capabilities"
        );
        assert_eq!(
            normalize_unified_admin_path("/api/beacon/home-assistant/status"),
            "/api/home-assistant/status"
        );
        assert_eq!(
            normalize_unified_admin_path("/api/beacon/harboros/apps/home-assistant/install"),
            "/api/harboros/apps/home-assistant/install"
        );
        assert_eq!(normalize_unified_admin_path("/api/beacon"), "/api/state");
    }

    #[test]
    fn harbor_beacon_api_prefix_maps_to_beacon_internal_admin_api() {
        assert_eq!(
            super::normalize_unified_admin_url("/api/harbor-beacon?refresh=1"),
            "/api/state?refresh=1"
        );
        assert_eq!(
            normalize_unified_admin_path("/api/harbor-beacon/state"),
            "/api/state"
        );
        assert_eq!(
            normalize_unified_admin_path("/api/harbor-beacon/home-assistant/status"),
            "/api/home-assistant/status"
        );
        assert_eq!(
            super::normalize_unified_admin_url("/api/harbor-beacon/knowledge/search?limit=10"),
            "/api/knowledge/search?limit=10"
        );
    }

    #[test]
    fn harbor_assistant_api_prefix_remains_deprecated_alias() {
        assert_eq!(
            normalize_unified_admin_path("/api/harbor-assistant/cameras/recording-settings"),
            "/api/cameras/recording-settings"
        );
        assert_eq!(
            normalize_unified_admin_path("/api/harbor-assistant/cameras/camera-1/snapshot.jpg"),
            "/api/cameras/camera-1/snapshot.jpg"
        );
        assert_eq!(
            normalize_unified_admin_path("/api/harbor-assistant/models/capabilities"),
            "/api/models/capabilities"
        );
        assert_eq!(
            normalize_unified_admin_path("/api/harbor-assistant"),
            "/api/state"
        );
        let removed_prefix = format!("/api/{}desk", "harbor");
        let removed_state = format!("{removed_prefix}/state");
        assert_eq!(
            normalize_unified_admin_path(removed_state.as_str()),
            removed_state
        );
        assert_eq!(
            normalize_unified_admin_path("/api/harbor-assistant-v2/state"),
            "/api/harbor-assistant-v2/state"
        );
    }

    #[test]
    fn harbor_assistant_client_routes_are_identified() {
        assert!(is_harbor_assistant_client_route("/"));
        assert!(is_harbor_assistant_client_route("/overview"));
        assert!(is_harbor_assistant_client_route("/models-policies"));
        assert!(is_harbor_assistant_surface_path("/assets/runtime.js"));
        assert!(is_harbor_assistant_surface_path("/main.js"));
        assert!(!is_harbor_assistant_surface_path("/api/state"));
        assert!(!is_harbor_assistant_surface_path("/setup/mobile"));
    }

    #[test]
    fn harbor_assistant_asset_paths_reject_parent_segments() {
        let root = PathBuf::from("C:/harbor-assistant-dist");
        assert_eq!(
            resolve_harbor_assistant_asset_path(&root, "/assets/main.js"),
            Some(root.join("assets").join("main.js"))
        );
        assert_eq!(
            resolve_harbor_assistant_asset_path(&root, "/../secret.txt"),
            None
        );
        assert_eq!(
            resolve_harbor_assistant_asset_path(&root, "/overview"),
            None
        );
    }

    #[test]
    fn static_file_helpers_set_expected_mime_types() {
        assert_eq!(
            mime_type_for_path(Path::new("C:/tmp/index.html")),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            mime_type_for_path(Path::new("C:/tmp/main.js")),
            "application/javascript; charset=utf-8"
        );
        assert_eq!(
            mime_type_for_path(Path::new("C:/tmp/icon.svg")),
            "image/svg+xml"
        );
        assert_eq!(
            mime_type_for_path(Path::new("C:/tmp/photo.jpeg")),
            "image/jpeg"
        );
        assert_eq!(
            mime_type_for_path(Path::new("C:/tmp/note.md")),
            "text/markdown; charset=utf-8"
        );
        assert_eq!(
            mime_type_for_path(Path::new("C:/tmp/clip.mp4")),
            "video/mp4"
        );
        assert_eq!(
            mime_type_for_path(Path::new("C:/tmp/clip.webm")),
            "video/webm"
        );
        assert!(knowledge_preview_mime_supported(Path::new(
            "C:/tmp/photo.webp"
        )));
        assert!(knowledge_preview_mime_supported(Path::new(
            "C:/tmp/clip.mp4"
        )));
        assert!(knowledge_preview_mime_supported(Path::new(
            "C:/tmp/note.txt"
        )));
        assert!(!knowledge_preview_mime_supported(Path::new(
            "C:/tmp/data.json"
        )));
    }

    #[test]
    fn harbor_assistant_build_missing_response_mentions_dist_path() {
        let response = harbor_assistant_build_missing_response(Path::new(
            "frontend/harbor-assistant/dist/harbor-assistant",
        ));
        assert_eq!(response.status_code(), StatusCode(503));
    }

    #[test]
    fn account_management_redaction_clears_gateway_credentials() {
        let registry_path = unique_store_path("harborbeacon-account-registry");
        let admin_path = unique_store_path("harborbeacon-account-state");
        let registry_store = DeviceRegistryStore::new(registry_path.clone());
        let admin_store = AdminConsoleStore::new(admin_path.clone(), registry_store);
        let state = admin_store.load_or_create_state().expect("state");
        let snapshot =
            harborbeacon_local_agent::runtime::admin_console::account_management_snapshot(
                &state,
                Some("http://harborbeacon.local:4174"),
            );
        let redacted = redact_account_management_snapshot(snapshot);

        assert_eq!(redacted.gateway.bridge_provider.app_id, "");
        assert_eq!(redacted.gateway.bridge_provider.app_secret, "");
        assert_eq!(redacted.gateway.bridge_provider.bot_open_id, "");

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
    }

    #[test]
    fn member_role_paths_decode_ids() {
        let encoded = "user%2F1";
        assert_eq!(
            parse_member_role_update_path(&format!("/api/access/members/{encoded}/role")),
            Some("user/1".to_string())
        );
    }

    #[test]
    fn member_default_delivery_surface_paths_decode_ids() {
        let encoded = "user%2F1";
        assert_eq!(
            parse_member_default_delivery_surface_update_path(&format!(
                "/api/access/members/{encoded}/default-delivery-surface"
            )),
            Some("user/1".to_string())
        );
    }

    #[test]
    fn notification_target_delete_paths_decode_ids() {
        let encoded = "target%2F1";
        assert_eq!(
            parse_notification_target_delete_path(&format!(
                "/api/admin/notification-targets/{encoded}"
            )),
            Some("target/1".to_string())
        );
    }

    #[test]
    fn model_endpoint_paths_decode_ids() {
        let encoded = "ocr%2Flocal";
        assert_eq!(
            parse_model_endpoint_path(&format!("/api/models/endpoints/{encoded}")),
            Some("ocr/local".to_string())
        );
        assert_eq!(
            parse_model_endpoint_test_path(&format!("/api/models/endpoints/{encoded}/test")),
            Some("ocr/local".to_string())
        );
        assert_eq!(
            parse_model_runtime_install_path("/api/models/runtimes/harbor-candle/install"),
            Some("harbor-candle".to_string())
        );
    }

    #[test]
    fn phase2_admin_paths_decode_ids() {
        let encoded_device = "camera%201%2Fleft";
        assert_eq!(
            parse_device_metadata_patch_path("/api/devices/camera%201"),
            Some("camera 1".to_string())
        );
        assert_eq!(
            parse_device_metadata_patch_path(&format!("/api/devices/{encoded_device}")),
            Some("camera 1/left".to_string())
        );

        let encoded_job = "model-download-1";
        assert_eq!(
            parse_model_download_job_path(&format!("/api/models/local-downloads/{encoded_job}")),
            Some("model-download-1".to_string())
        );
        assert_eq!(
            parse_model_download_cancel_path(&format!(
                "/api/models/local-downloads/{encoded_job}/cancel"
            )),
            Some("model-download-1".to_string())
        );
        assert_eq!(
            parse_device_evidence_path("/api/devices/camera%201%2Fleft/evidence"),
            Some("camera 1/left".to_string())
        );
        assert_eq!(
            parse_device_validation_run_path("/api/devices/camera%201%2Fleft/validation/run"),
            Some("camera 1/left".to_string())
        );
        assert_eq!(
            parse_device_evidence_path("/api/devices/camera/left/evidence"),
            None
        );
    }

    #[test]
    fn release_readiness_schema_groups_core_lanes() {
        let registry_path = unique_store_path("harborbeacon-readiness-registry");
        let admin_path = unique_store_path("harborbeacon-readiness-state");
        let registry_store = DeviceRegistryStore::new(registry_path.clone());
        let admin_store = AdminConsoleStore::new(admin_path.clone(), registry_store);
        let state = admin_store.load_or_create_state().expect("state");
        let account_management =
            harborbeacon_local_agent::runtime::admin_console::account_management_snapshot(
                &state,
                Some("http://192.168.3.182:4174"),
            );
        let runtime = LocalModelRuntimeProjection {
            ready: true,
            backend_ready: true,
            backend_kind: Some("candle".to_string()),
            chat_model: Some("/models/qwen".to_string()),
            embedding_model: Some("/models/jina".to_string()),
            ..Default::default()
        };
        let features = build_feature_availability_response(
            &state.models.endpoints,
            &state.models.route_policies,
            &account_management,
            None,
            &runtime,
        );
        let hardware = build_hardware_readiness_response();
        let harboros = build_harboros_status_response("http://192.168.3.182:4174");
        let rag = build_rag_readiness_response(
            &runtime,
            &state.knowledge,
            &state.models.endpoints,
            &state.knowledge_index_jobs,
            &state.audit_records,
        );

        let response = build_release_readiness_response(
            "http://192.168.3.182:4174",
            None,
            &account_management,
            &features,
            &hardware,
            &harboros,
            &rag,
            &runtime,
        );

        assert_eq!(response.harbor_desk.admin_port, 4174);
        assert_eq!(
            response.harbor_desk.harboros_webui,
            "http://192.168.3.182/ui/"
        );
        assert_eq!(response.status, response.overall_status);
        assert!(!response.checklist.is_empty());
        assert!(!response.status_cards.is_empty());
        for group_id in ["im", "models", "rag", "hardware", "harboros", "aiot"] {
            assert!(response
                .groups
                .iter()
                .any(|group| group.group_id == group_id));
        }
        assert!(response
            .groups
            .iter()
            .flat_map(|group| group.items.iter())
            .all(|item| !serde_json::to_string(item).unwrap().contains("secret")));

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
    }

    #[test]
    fn harboros_im_capability_map_keeps_risky_actions_approval_gated() {
        let map = build_harboros_im_capability_map();
        let service_restart = map
            .items
            .iter()
            .find(|item| item.capability_id == "services.restart")
            .expect("service restart capability");
        assert!(!service_restart.im_ready);
        assert!(service_restart.approval_required);
        assert_eq!(service_restart.risk_level, "high");
        assert!(map
            .items
            .iter()
            .any(|item| item.capability_id == "dashboard.status" && item.im_ready));
    }

    #[test]
    fn local_model_catalog_surfaces_download_jobs_without_auto_download() {
        let job = ModelDownloadJobRecord {
            job_id: "model-download-1".to_string(),
            model_id: "qwen2.5-1.5b-instruct".to_string(),
            display_name: "Qwen2.5 1.5B Instruct".to_string(),
            provider_key: "qwen".to_string(),
            status: "queued".to_string(),
            requested_at: "1".to_string(),
            updated_at: "1".to_string(),
            target_path: None,
            progress_percent: Some(0),
            bytes_downloaded: Some(0),
            total_bytes: None,
            started_at: None,
            completed_at: None,
            error_message: None,
            message: "download request registered".to_string(),
            metadata: json!({}),
        };
        let catalog = build_local_model_catalog(vec![job.clone()]);

        assert!(catalog
            .models
            .iter()
            .any(|item| item.model_id == "qwen2.5-1.5b-instruct"));
        assert_eq!(catalog.download_jobs, vec![job]);
    }

    #[test]
    fn model_download_status_uses_latest_job_per_model() {
        let job =
            |job_id: &str, model_id: &str, status: &str, requested_at: &str, updated_at: &str| {
                ModelDownloadJobRecord {
                    job_id: job_id.to_string(),
                    model_id: model_id.to_string(),
                    display_name: model_id.to_string(),
                    provider_key: "qwen".to_string(),
                    status: status.to_string(),
                    requested_at: requested_at.to_string(),
                    updated_at: updated_at.to_string(),
                    target_path: None,
                    progress_percent: None,
                    bytes_downloaded: None,
                    total_bytes: None,
                    started_at: None,
                    completed_at: None,
                    error_message: None,
                    message: String::new(),
                    metadata: json!({}),
                }
            };

        let failed_old = job("job-1", "Qwen/Qwen3.5-4B", "failed", "1", "1");
        let completed_new = job("job-2", "Qwen/Qwen3.5-4B", "completed", "2", "2");
        assert_eq!(
            model_download_jobs_status(&[failed_old.clone(), completed_new.clone()]),
            "ready"
        );

        let running_other = job("job-3", "Qwen/Qwen3.5-9B", "running", "3", "3");
        assert_eq!(
            model_download_jobs_status(
                &[failed_old.clone(), completed_new.clone(), running_other,]
            ),
            "running"
        );

        let failed_latest = job("job-4", "Qwen/Qwen3.5-9B", "failed", "4", "4");
        assert_eq!(
            model_download_jobs_status(&[failed_old, completed_new, failed_latest]),
            "blocked"
        );
    }

    #[test]
    fn latest_model_download_jobs_collapses_retries_by_model() {
        let job =
            |job_id: &str, model_id: &str, status: &str, requested_at: &str, updated_at: &str| {
                ModelDownloadJobRecord {
                    job_id: job_id.to_string(),
                    model_id: model_id.to_string(),
                    display_name: model_id.to_string(),
                    provider_key: "qwen".to_string(),
                    status: status.to_string(),
                    requested_at: requested_at.to_string(),
                    updated_at: updated_at.to_string(),
                    target_path: None,
                    progress_percent: None,
                    bytes_downloaded: None,
                    total_bytes: None,
                    started_at: None,
                    completed_at: None,
                    error_message: None,
                    message: String::new(),
                    metadata: json!({}),
                }
            };

        let jobs = latest_model_download_jobs(&[
            job("job-old", "Qwen/Qwen3.5-4B", "failed", "1", "1"),
            job("job-new", "Qwen/Qwen3.5-4B", "running", "2", "3"),
            job("job-other", "Qwen/Qwen3-Embedding-0.6B", "failed", "2", "2"),
        ]);

        assert_eq!(
            jobs.iter()
                .map(|job| job.job_id.as_str())
                .collect::<Vec<_>>(),
            vec!["job-new", "job-other"]
        );
    }

    #[test]
    fn model_download_retry_reuses_failed_job_record() {
        let admin_path = unique_store_path("harborbeacon-model-download-reuse-admin");
        let registry_path = unique_store_path("harborbeacon-model-download-reuse-registry");
        let store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        let original = store
            .create_model_download_job(
                "Qwen/Qwen3.5-4B",
                "Qwen3.5 4B",
                "qwen",
                Some("/tmp/qwen-old".to_string()),
                json!({"source_kind": "huggingface"}),
            )
            .expect("create job");
        let mut failed = original.clone();
        failed.status = "failed".to_string();
        failed.updated_at = "2".to_string();
        failed.progress_percent = Some(87);
        failed.error_message = Some("mirror failed".to_string());
        store
            .save_model_download_job(failed)
            .expect("save failed job");

        let retry = store
            .create_or_update_model_download_job(
                "Qwen/Qwen3.5-4B",
                "Qwen3.5 4B",
                "qwen",
                Some("/tmp/qwen-new".to_string()),
                json!({"source_kind": "huggingface", "hf_endpoint": "https://hf-mirror.com/"}),
            )
            .expect("retry job");

        assert!(retry.should_spawn_worker);
        assert_eq!(retry.job.job_id, original.job_id);
        assert_eq!(retry.job.status, "queued");
        assert_eq!(retry.job.progress_percent, Some(0));
        assert_eq!(retry.job.error_message, None);
        assert_eq!(retry.job.target_path.as_deref(), Some("/tmp/qwen-new"));
        assert_eq!(store.list_model_download_jobs().unwrap().len(), 1);

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
    }

    #[test]
    fn local_model_catalog_ignores_failed_huggingface_cache_only_directory() {
        let root = unique_store_path("model-cache-root").with_extension("");
        let target = root.join("qwen-qwen3.5-4b");
        fs::create_dir_all(target.join(".hf-cache")).expect("create hf cache only target");
        let job = ModelDownloadJobRecord {
            job_id: "model-download-failed".to_string(),
            model_id: "Qwen/Qwen3.5-4B".to_string(),
            display_name: "Qwen3.5 4B".to_string(),
            provider_key: "qwen".to_string(),
            status: "failed".to_string(),
            requested_at: "1".to_string(),
            updated_at: "2".to_string(),
            target_path: Some(target.display().to_string()),
            progress_percent: Some(100),
            bytes_downloaded: Some(0),
            total_bytes: None,
            started_at: Some("1".to_string()),
            completed_at: Some("2".to_string()),
            error_message: Some("dns failed".to_string()),
            message: "download failed".to_string(),
            metadata: json!({}),
        };
        let spec = local_model_catalog_specs()
            .into_iter()
            .find(|spec| spec.model_id == "Qwen/Qwen3.5-4B")
            .expect("qwen live-test catalog spec");

        let item = local_model_catalog_item(&[root.display().to_string()], &[job], spec);

        assert!(!item.installed);
        assert_eq!(item.status, "blocked");
        assert!(item.local_path.is_none());
        assert_eq!(item.size_bytes, None);
        assert!(item
            .evidence
            .iter()
            .any(|entry| entry.starts_with("ignored_incomplete_local_path=")));
        assert!(item
            .evidence
            .iter()
            .any(|entry| entry == "ignored_incomplete_size_bytes=0"));
        assert!(item
            .evidence
            .iter()
            .any(|entry| entry == "latest_download_status=failed"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_model_catalog_marks_nonempty_cache_directory_installed() {
        let root = unique_store_path("model-cache-root").with_extension("");
        let target = root.join("qwen-qwen3.5-4b");
        fs::create_dir_all(&target).expect("create model target");
        fs::write(target.join("config.json"), b"{}").expect("write model payload");
        let spec = local_model_catalog_specs()
            .into_iter()
            .find(|spec| spec.model_id == "Qwen/Qwen3.5-4B")
            .expect("qwen live-test catalog spec");

        let item = local_model_catalog_item(&[root.display().to_string()], &[], spec);

        assert!(item.installed);
        assert_eq!(item.status, "ready");
        let expected_path = target.display().to_string();
        assert_eq!(item.local_path.as_deref(), Some(expected_path.as_str()));
        assert_eq!(item.size_bytes, Some(2));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn local_model_catalog_marks_completed_download_target_installed() {
        let root = unique_store_path("model-cache-root").with_extension("");
        let target = root.join("qwen-completed-download");
        fs::create_dir_all(&target).expect("create model target");
        fs::write(target.join("config.json"), b"{}").expect("write model payload");
        let job = ModelDownloadJobRecord {
            job_id: "model-download-completed".to_string(),
            model_id: "Qwen/Qwen3.5-4B".to_string(),
            display_name: "Qwen3.5 4B".to_string(),
            provider_key: "qwen".to_string(),
            status: "completed".to_string(),
            requested_at: "1".to_string(),
            updated_at: "2".to_string(),
            target_path: Some(target.display().to_string()),
            progress_percent: Some(100),
            bytes_downloaded: Some(2),
            total_bytes: Some(2),
            started_at: Some("1".to_string()),
            completed_at: Some("2".to_string()),
            error_message: None,
            message: "download complete".to_string(),
            metadata: json!({}),
        };
        let spec = local_model_catalog_specs()
            .into_iter()
            .find(|spec| spec.model_id == "Qwen/Qwen3.5-4B")
            .expect("qwen live-test catalog spec");

        let item = local_model_catalog_item(&[root.display().to_string()], &[job], spec);

        assert!(item.installed);
        assert_eq!(item.status, "ready");
        let expected_path = target.display().to_string();
        assert_eq!(item.local_path.as_deref(), Some(expected_path.as_str()));
        assert_eq!(item.size_bytes, Some(2));
        assert_eq!(
            item.download_job_id.as_deref(),
            Some("model-download-completed")
        );
        assert!(item
            .evidence
            .iter()
            .any(|entry| entry == "latest_download_status=completed"));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn bootstrap_model_download_target_uses_candle_runtime_store() {
        let root = unique_store_path("bootstrap-model-store").with_extension("");
        let target = default_model_download_target_path_in_root(
            &root.display().to_string(),
            "Qwen/Qwen2.5-0.5B-Instruct",
        );
        assert!(target
            .replace('\\', "/")
            .ends_with("runtimes/harbor-candle/bootstrap-llm"));
    }

    #[test]
    fn local_model_catalog_surfaces_huggingface_qwen_live_test_models() {
        let catalog = build_local_model_catalog(Vec::new());

        let primary = catalog
            .models
            .iter()
            .find(|item| item.model_id == "Qwen/Qwen3.5-4B")
            .expect("primary qwen model");
        assert_eq!(primary.source_kind, "huggingface");
        assert_eq!(primary.repo_id.as_deref(), Some("Qwen/Qwen3.5-4B"));
        assert_eq!(primary.file_policy, "runtime_snapshot");
        assert!(primary
            .runtime_profiles
            .iter()
            .any(|profile| profile == "vllm-openai-compatible"));
        assert_eq!(
            primary.acceptance_note.as_deref(),
            Some("primary-live-test")
        );

        let stretch = catalog
            .models
            .iter()
            .find(|item| item.model_id == "Qwen/Qwen3.5-9B")
            .expect("stretch qwen model");
        assert_eq!(stretch.acceptance_note.as_deref(), Some("stretch-after-4b"));

        let not_today = catalog
            .models
            .iter()
            .find(|item| item.model_id == "Qwen/Qwen3.6-35B-A3B")
            .expect("not today qwen model");
        assert_eq!(
            not_today.acceptance_note.as_deref(),
            Some("not-today-acceptance")
        );

        let qwen25 = catalog
            .models
            .iter()
            .find(|item| item.model_id == "qwen2.5-1.5b-instruct")
            .expect("qwen2.5 catalog model");
        assert_eq!(qwen25.source_kind, "huggingface");
        assert_eq!(
            qwen25.repo_id.as_deref(),
            Some("Qwen/Qwen2.5-1.5B-Instruct")
        );
        assert!(qwen25.installable);

        let bootstrap = catalog
            .models
            .iter()
            .find(|item| item.model_id == "Qwen/Qwen2.5-0.5B-Instruct")
            .expect("bootstrap qwen model");
        assert_eq!(bootstrap.source_kind, "huggingface");
        assert_eq!(
            bootstrap.repo_id.as_deref(),
            Some("Qwen/Qwen2.5-0.5B-Instruct")
        );
        assert_eq!(bootstrap.acceptance_note.as_deref(), Some("iso-bootstrap"));
        assert!(bootstrap
            .expected_capabilities
            .iter()
            .any(|capability| capability == "assistant_input_parser"));
        assert!(bootstrap
            .runtime_profiles
            .iter()
            .any(|profile| profile == "harbor-candle"));

        let smolvlm = catalog
            .models
            .iter()
            .find(|item| item.model_id == "HuggingFaceTB/SmolVLM-256M-Instruct")
            .expect("vm cpu smolvlm catalog model");
        assert_eq!(smolvlm.source_kind, "huggingface");
        assert_eq!(
            smolvlm.repo_id.as_deref(),
            Some("HuggingFaceTB/SmolVLM-256M-Instruct")
        );
        assert_eq!(smolvlm.model_kind, "vlm");
        assert_eq!(smolvlm.file_policy, "runtime_snapshot");
        assert!(smolvlm
            .runtime_profiles
            .iter()
            .any(|profile| profile == "harbor-vlm-sidecar"));
        assert_eq!(smolvlm.acceptance_note.as_deref(), Some("vm-cpu-photo-rag"));

        let bge = catalog
            .models
            .iter()
            .find(|item| item.model_id == "bge-m3")
            .expect("bge catalog model");
        assert_eq!(bge.source_kind, "huggingface");
        assert_eq!(bge.repo_id.as_deref(), Some("BAAI/bge-m3"));
        assert_eq!(bge.file_policy, "runtime_snapshot");
        assert!(bge.installable);
        assert!(!bge.manual_only);
    }

    #[test]
    fn local_model_catalog_marks_manual_legacy_models_manual_only() {
        let catalog = build_local_model_catalog(Vec::new());

        let legacy = catalog
            .models
            .iter()
            .find(|item| item.model_id == "minicpm-v-2.6")
            .expect("legacy manual VLM catalog model");
        assert_eq!(legacy.source_kind, "manual_or_url");
        assert!(!legacy.installable);
        assert!(legacy.manual_only);
        assert!(legacy.repo_id.is_none());
    }

    #[test]
    fn hardware_profile_keeps_4b_models_out_of_tiny_cpu_recommendations() {
        assert_eq!(
            hardware_class_for_probe(4, Some(12_000), false, None),
            "tiny_cpu"
        );
        assert_eq!(
            hardware_class_for_probe(8, Some(32_000), true, Some(12_288)),
            "low_vram_gpu"
        );
        assert_eq!(
            hardware_class_for_probe(8, Some(32_000), true, Some(16_384)),
            "gpu_16gb"
        );

        let mut hardware = build_hardware_readiness_response();
        hardware.hardware_class = "tiny_cpu".to_string();
        let qwen35_4b = local_model_catalog_specs()
            .into_iter()
            .find(|spec| spec.model_id == "Qwen/Qwen3.5-4B")
            .expect("qwen3.5 4b spec");
        let recommendation = model_hardware_recommendation(&qwen35_4b, &hardware, true);
        assert_eq!(recommendation.hardware_fit, "not_recommended");
        assert_eq!(
            recommendation.recommendation_group,
            "installed_not_recommended"
        );

        let qwen25 = local_model_catalog_specs()
            .into_iter()
            .find(|spec| spec.model_id == "qwen2.5-1.5b-instruct")
            .expect("qwen2.5 1.5b spec");
        let recommendation = model_hardware_recommendation(&qwen25, &hardware, false);
        assert_eq!(recommendation.hardware_fit, "recommended");
        assert_eq!(recommendation.recommendation_group, "lightweight_local");

        hardware.hardware_class = "gpu_16gb".to_string();
        let recommendation = model_hardware_recommendation(&qwen35_4b, &hardware, true);
        assert_eq!(recommendation.hardware_fit, "recommended");
        assert_eq!(recommendation.recommendation_group, "current_recommended");
    }

    #[test]
    fn model_capabilities_hide_manual_only_models_from_installable_choices() {
        let runtime = LocalModelRuntimeProjection {
            error: Some("runtime offline".to_string()),
            ..Default::default()
        };
        let endpoints =
            overlay_model_endpoints_with_runtime_truth(&default_model_endpoints(), &runtime);
        let response = build_model_capabilities_response(
            &AdminModelCenterState::default(),
            &endpoints,
            &default_model_route_policies(),
            Vec::new(),
            &runtime,
        );

        let embedder = response
            .capabilities
            .iter()
            .find(|capability| capability.capability_id == "embedder")
            .expect("embedder capability");
        assert_ne!(embedder.status, "ready");
        assert!(embedder
            .installable_models
            .iter()
            .all(|model| { model.source_kind == "huggingface" && model.repo_id.is_some() }));

        let vision = response
            .capabilities
            .iter()
            .find(|capability| capability.capability_id == "vlm")
            .expect("vision capability");
        assert!(vision
            .installable_models
            .iter()
            .all(|model| model.model_id != "minicpm-v-2.6"));
    }

    #[test]
    fn model_capabilities_surface_default_candle_runtime_and_missing_model() {
        let runtime = LocalModelRuntimeProjection {
            error: Some("runtime offline".to_string()),
            ..Default::default()
        };
        let endpoints =
            overlay_model_endpoints_with_runtime_truth(&default_model_endpoints(), &runtime);
        let response = build_model_capabilities_response(
            &AdminModelCenterState::default(),
            &endpoints,
            &default_model_route_policies(),
            Vec::new(),
            &runtime,
        );

        assert_eq!(response.runtime_manager.status, "installed");
        let candle = response
            .runtime_manager
            .runtimes
            .iter()
            .find(|runtime| runtime.record.runtime_id == "harbor-candle")
            .expect("harbor candle runtime");
        assert!(candle.installed);
        assert!(candle.record.enabled);
        assert!(candle.record.installable);

        let router = response
            .capabilities
            .iter()
            .find(|capability| capability.capability_id == "semantic_router")
            .expect("router capability");
        assert_eq!(router.status, "needs_model");
        assert_eq!(
            router.required_runtime_profile.as_deref(),
            Some("harbor-candle")
        );
        assert!(router.runtime_installed);
        assert!(router.runtime_installable);
        assert_eq!(router.next_action, "选择或安装模型");
        assert!(router.current_model.is_none());
    }

    #[test]
    fn huggingface_snapshot_allow_patterns_keep_runtime_files_only() {
        let patterns = vec![
            "*.json".to_string(),
            "*.safetensors".to_string(),
            "tokenizer*".to_string(),
        ];

        assert!(model_snapshot_file_allowed("config.json", &patterns));
        assert!(model_snapshot_file_allowed(
            "model-00001-of-00002.safetensors",
            &patterns
        ));
        assert!(model_snapshot_file_allowed("tokenizer.model", &patterns));
        assert!(!model_snapshot_file_allowed("pytorch_model.bin", &patterns));
        assert!(!model_snapshot_file_allowed("../escape.json", &patterns));
    }

    #[test]
    fn huggingface_plain_http_fallback_handles_mirror_header_gaps() {
        assert!(huggingface_download_should_fallback_to_plain_http(
            "Header Content-Range is missing"
        ));
        assert!(huggingface_download_should_fallback_to_plain_http(
            "Header etag is missing"
        ));
        assert!(!huggingface_download_should_fallback_to_plain_http(
            "HTTP status client error"
        ));
    }

    #[test]
    fn huggingface_endpoint_fallback_covers_common_mirror_failures() {
        assert!(super::huggingface_download_should_try_endpoint_fallback(
            "status code 404"
        ));
        assert!(super::huggingface_download_should_try_endpoint_fallback(
            "operation timed out"
        ));
        assert!(!super::huggingface_download_should_try_endpoint_fallback(
            "checksum mismatch"
        ));
    }

    #[test]
    fn huggingface_resolve_url_uses_endpoint_repo_revision_and_file_path() {
        let url = huggingface_resolve_url(
            "https://hf-mirror.com/",
            "Qwen/Qwen3.5-4B",
            "main",
            "nested/config.json",
        )
        .expect("resolve url");

        assert_eq!(
            url.as_str(),
            "https://hf-mirror.com/Qwen/Qwen3.5-4B/resolve/main/nested/config.json"
        );
    }

    #[test]
    fn huggingface_endpoint_prefers_job_metadata_then_env_then_default_mirror() {
        let _guard = hf_endpoint_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let env_guard = EnvGuard::set("HF_ENDPOINT", "https://env-mirror.example/");

        assert_eq!(
            model_download_huggingface_endpoint(&json!({
                "hf_endpoint": "https://user-mirror.example///"
            })),
            "https://user-mirror.example"
        );
        assert_eq!(
            model_download_huggingface_endpoint(&json!({})),
            "https://env-mirror.example"
        );

        drop(env_guard);
        let remove_guard = EnvGuard::remove("HF_ENDPOINT");
        assert_eq!(
            model_download_huggingface_endpoint(&json!({})),
            DEFAULT_HF_ENDPOINT
        );
        drop(remove_guard);
    }

    #[test]
    fn huggingface_endpoint_candidates_dedupe_metadata_env_and_builtin_fallbacks() {
        let _guard = hf_endpoint_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _env_guard = EnvGuard::set(
            "HF_ENDPOINTS",
            "https://env-one.example/; https://hf-mirror.com/",
        );
        let _single_env_guard = EnvGuard::set("HF_ENDPOINT", "https://env-two.example/");

        assert_eq!(
            model_download_huggingface_endpoints(&json!({
                "hf_endpoints": ["https://meta-one.example/", "https://meta-one.example///"],
                "hf_endpoint": "https://meta-two.example/"
            })),
            vec![
                "https://meta-one.example".to_string(),
                "https://meta-two.example".to_string(),
                "https://env-one.example".to_string(),
                "https://hf-mirror.com".to_string(),
                "https://env-two.example".to_string(),
                "https://huggingface.co".to_string(),
            ]
        );
    }

    #[test]
    fn huggingface_endpoint_does_not_generate_double_slash_repo_info_base() {
        let endpoint = model_download_huggingface_endpoint(&json!({
            "hf_endpoint": "https://hf-mirror.com/"
        }));
        let repo_info_url = format!(
            "{}/api/models/{}/revision/{}",
            endpoint, "Qwen/Qwen3.5-4B", "main"
        );

        assert_eq!(
            repo_info_url,
            "https://hf-mirror.com/api/models/Qwen/Qwen3.5-4B/revision/main"
        );
        assert!(!repo_info_url.contains(".com//api/"));
    }

    #[test]
    fn default_model_download_target_path_is_snapshot_directory() {
        let target = default_model_download_target_path("Qwen/Qwen3.5-4B");

        assert!(target.ends_with("qwen-qwen3.5-4b"));
        assert!(!target.ends_with("model.bin"));
    }

    #[test]
    fn model_download_transfer_copies_explicit_source_to_target() {
        let source_path = unique_store_path("harborbeacon-model-source");
        let target_path = unique_store_path("harborbeacon-model-target");
        fs::write(&source_path, b"model-bytes").expect("write source");
        let _ = fs::remove_file(&target_path);

        let job = harborbeacon_local_agent::runtime::admin_console::ModelDownloadJobRecord {
            job_id: "model-download-copy".to_string(),
            model_id: "demo-model".to_string(),
            display_name: "Demo model".to_string(),
            provider_key: "local".to_string(),
            status: "running".to_string(),
            requested_at: "1".to_string(),
            updated_at: "1".to_string(),
            target_path: Some(target_path.display().to_string()),
            progress_percent: Some(0),
            bytes_downloaded: Some(0),
            total_bytes: None,
            started_at: Some("1".to_string()),
            completed_at: None,
            error_message: None,
            message: String::new(),
            metadata: json!({
                "source_url": source_path.display().to_string(),
                "api_key": "should-not-be-used"
            }),
        };

        let stats = run_model_download_transfer(&job).expect("download transfer");

        assert_eq!(stats.bytes_written, 11);
        assert_eq!(fs::read(&target_path).expect("read target"), b"model-bytes");
        let _ = fs::remove_file(source_path);
        let _ = fs::remove_file(target_path);
    }

    #[test]
    fn model_download_failed_job_preserves_last_progress() {
        let admin_path = unique_store_path("harborbeacon-model-download-failed-admin");
        let registry_path = unique_store_path("harborbeacon-model-download-failed-registry");
        let store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        let job = store
            .create_model_download_job(
                "missing-source-model",
                "Missing source model",
                "local",
                Some(
                    unique_store_path("harborbeacon-model-download-target")
                        .display()
                        .to_string(),
                ),
                json!({}),
            )
            .expect("create download job");

        run_model_download_job(store.clone(), job.clone());

        let saved = store
            .model_download_job(&job.job_id)
            .expect("load job")
            .expect("job exists");
        assert_eq!(saved.status, "failed");
        assert_eq!(saved.progress_percent, Some(0));
        assert!(saved
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("download source_url is required"));
        assert!(saved.message.contains("download job failed"));

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
    }

    #[test]
    fn concurrent_model_download_workers_preserve_job_records() {
        let admin_path = unique_store_path("harborbeacon-model-download-concurrent-admin");
        let registry_path = unique_store_path("harborbeacon-model-download-concurrent-registry");
        let source_path = unique_store_path("harborbeacon-model-download-concurrent-source");
        let target_path = unique_store_path("harborbeacon-model-download-concurrent-target");
        fs::write(&source_path, b"model-bytes").expect("write source");
        let store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        let copied = store
            .create_model_download_job(
                "concurrent-copy-model",
                "Concurrent copy model",
                "local",
                Some(target_path.display().to_string()),
                json!({"source_url": source_path.display().to_string()}),
            )
            .expect("create copy job");
        let missing = store
            .create_model_download_job(
                "concurrent-missing-source-model",
                "Concurrent missing source model",
                "local",
                Some(
                    unique_store_path("harborbeacon-model-download-concurrent-missing-target")
                        .display()
                        .to_string(),
                ),
                json!({}),
            )
            .expect("create missing job");

        let copy_store = store.clone();
        let copy_thread = thread::spawn(move || run_model_download_job(copy_store, copied));
        let missing_store = store.clone();
        let missing_thread = thread::spawn(move || run_model_download_job(missing_store, missing));
        copy_thread.join().expect("copy worker");
        missing_thread.join().expect("missing worker");

        let jobs = store.list_model_download_jobs().expect("list jobs");
        let copied = jobs
            .iter()
            .find(|job| job.model_id == "concurrent-copy-model")
            .expect("copy job preserved");
        let missing = jobs
            .iter()
            .find(|job| job.model_id == "concurrent-missing-source-model")
            .expect("missing job preserved");
        assert_eq!(copied.status, "completed");
        assert_eq!(missing.status, "failed");
        assert!(missing
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("download source_url is required"));

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(source_path);
        let _ = fs::remove_file(target_path);
    }

    #[test]
    fn rag_readiness_schema_reports_required_release_fields() {
        let index_root = std::env::temp_dir().join("harborbeacon-rag-index-test");
        let source_root = std::env::temp_dir().join("harborbeacon-rag-source-test");
        fs::create_dir_all(&source_root).expect("create rag source root");
        let settings = KnowledgeSettings {
            source_roots: vec![KnowledgeSourceRoot {
                root_id: "test-root".to_string(),
                label: "Test root".to_string(),
                path: source_root.to_string_lossy().into_owned(),
                enabled: true,
                include: Vec::new(),
                exclude: Vec::new(),
                last_indexed_at: None,
            }],
            index_root: index_root.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let response = build_rag_readiness_response(
            &LocalModelRuntimeProjection {
                ready: true,
                backend_ready: true,
                embedding_model: Some("jina".to_string()),
                ..Default::default()
            },
            &settings,
            &default_model_endpoints(),
            &[],
            &[],
        );

        assert!(!response.generated_at.is_empty());
        assert!(!response.status.is_empty());
        assert_eq!(response.source_roots.status, "ready");
        assert!(response
            .evidence
            .iter()
            .any(|entry| entry.contains("embedding_model=jina")));
        assert_eq!(
            response.privacy_gateway.policy_version,
            PRIVACY_GATEWAY_POLICY_VERSION
        );
        assert_eq!(
            response.privacy_gateway.covered_routes,
            vec!["retrieval.answer".to_string()]
        );
        assert_eq!(response.privacy_gateway.secret_scan, "clean");
        let _ = fs::remove_dir_all(source_root);
    }

    #[test]
    fn rag_readiness_privacy_gateway_reports_metadata_only_audit_stats() {
        let source_root = unique_store_path("harborbeacon-rag-source-privacy-gateway");
        let index_root = unique_store_path("harborbeacon-rag-index-privacy-gateway");
        fs::create_dir_all(&source_root).expect("create rag source root");
        let settings = KnowledgeSettings {
            source_roots: vec![KnowledgeSourceRoot {
                root_id: "test-root".to_string(),
                label: "Test root".to_string(),
                path: source_root.to_string_lossy().into_owned(),
                enabled: true,
                include: Vec::new(),
                exclude: Vec::new(),
                last_indexed_at: None,
            }],
            index_root: index_root.to_string_lossy().into_owned(),
            privacy_level: PrivacyLevel::AllowRedactedCloud,
            default_resource_profile: RagResourceProfile::CloudAllowed,
            ..Default::default()
        };
        let audit = build_metadata_audit_record(
            "home-1",
            "privacy_transform",
            "privacy-transform-abc123",
            PRIVACY_GATEWAY_AUDIT_ACTION,
            AuditActorKind::System,
            "system",
            json!({
                "task_id": "task-1",
                "trace_id": "trace-1",
                "metadata_only": true,
            }),
            json!({
                "decision": "allow_redacted_cloud",
                "risk_level": "medium",
                "privacy_transform_id": "privacy-transform-abc123",
                "warnings": ["redacted cloud capsule used"],
                "metadata_only": true,
            }),
        );

        let response = build_rag_readiness_response(
            &LocalModelRuntimeProjection {
                ready: true,
                backend_ready: true,
                embedding_model: Some("jina".to_string()),
                ..Default::default()
            },
            &settings,
            &default_model_endpoints(),
            &[],
            &[audit],
        );

        assert_eq!(response.privacy_gateway.status, "ready");
        assert_eq!(response.privacy_gateway.recent_audit_count, 1);
        assert_eq!(
            response
                .privacy_gateway
                .decision_counts
                .get("allow_redacted_cloud"),
            Some(&1)
        );
        assert_eq!(response.privacy_gateway.risk_counts.get("medium"), Some(&1));
        assert_eq!(response.privacy_gateway.warning_count, 1);
        assert_eq!(
            response
                .privacy_gateway
                .latest_privacy_transform_id
                .as_deref(),
            Some("privacy-transform-abc123")
        );
        let text = serde_json::to_string(&response.privacy_gateway).expect("serialize gateway");
        assert!(!text.contains("C:\\Users"));
        assert!(!text.contains("source_path"));
        assert!(!text.contains("https://"));

        let _ = fs::remove_dir_all(source_root);
    }

    #[test]
    fn rag_readiness_top_level_status_includes_degraded_model_cards() {
        let source_root = unique_store_path("harborbeacon-rag-source-model-blocker");
        let index_root = unique_store_path("harborbeacon-rag-index-model-blocker");
        fs::create_dir_all(&source_root).expect("create rag source root");
        let settings = KnowledgeSettings {
            source_roots: vec![KnowledgeSourceRoot {
                root_id: "test-root".to_string(),
                label: "Test root".to_string(),
                path: source_root.to_string_lossy().into_owned(),
                enabled: true,
                include: Vec::new(),
                exclude: Vec::new(),
                last_indexed_at: None,
            }],
            index_root: index_root.to_string_lossy().into_owned(),
            ..Default::default()
        };

        let response = build_rag_readiness_response(
            &LocalModelRuntimeProjection {
                ready: true,
                backend_ready: true,
                embedding_model: Some("jina".to_string()),
                ..Default::default()
            },
            &settings,
            &[],
            &[],
            &[],
        );

        assert_ne!(response.status, "ready");
        assert!(response
            .blockers
            .iter()
            .any(|item| item.contains("OCR endpoint is not configured")));
        assert!(response
            .model_readiness
            .iter()
            .any(|card| card.label == "OCR" && card.status == "needs-config"));
        let _ = fs::remove_dir_all(source_root);
    }

    #[test]
    fn rag_readiness_embedder_card_follows_runtime_truth() {
        let source_root = unique_store_path("harborbeacon-rag-source-runtime-embedder");
        let index_root = unique_store_path("harborbeacon-rag-index-runtime-embedder");
        fs::create_dir_all(&source_root).expect("create rag source root");
        let settings = KnowledgeSettings {
            source_roots: vec![KnowledgeSourceRoot {
                root_id: "test-root".to_string(),
                label: "Test root".to_string(),
                path: source_root.to_string_lossy().into_owned(),
                enabled: true,
                include: Vec::new(),
                exclude: Vec::new(),
                last_indexed_at: None,
            }],
            index_root: index_root.to_string_lossy().into_owned(),
            ..Default::default()
        };

        let response = build_rag_readiness_response(
            &LocalModelRuntimeProjection {
                ready: false,
                backend_ready: false,
                embedding_model: None,
                error: Some("runtime offline".to_string()),
                ..Default::default()
            },
            &settings,
            &[ModelEndpoint {
                model_endpoint_id: "embed-local-openai-compatible".to_string(),
                workspace_id: Some("home-1".to_string()),
                provider_account_id: None,
                model_kind: ModelKind::Embedder,
                endpoint_kind: ModelEndpointKind::Local,
                provider_key: "openai_compatible".to_string(),
                model_name: "Qwen/Qwen3-Embedding-0.6B".to_string(),
                capability_tags: Vec::new(),
                cost_policy: json!({}),
                status: ModelEndpointStatus::Active,
                metadata: json!({}),
            }],
            &[],
            &[],
        );

        let embedder = response
            .model_readiness
            .iter()
            .find(|card| card.model_kind == "embedder")
            .expect("embedder readiness card");
        assert_eq!(embedder.status, "needs-config");
        assert!(embedder
            .blocker
            .as_deref()
            .is_some_and(|blocker| blocker.contains("degraded")));
        assert!(response
            .blockers
            .iter()
            .any(|blocker| blocker.contains("Embedding model is not ready")));
        let _ = fs::remove_dir_all(source_root);
    }

    #[test]
    fn rag_readiness_image_capability_is_not_ready_without_vlm_content_index() {
        let source_root = unique_store_path("harborbeacon-rag-source-image-content");
        let index_root = unique_store_path("harborbeacon-rag-index-image-content");
        fs::create_dir_all(&source_root).expect("create rag source root");
        fs::create_dir_all(&index_root).expect("create rag index root");
        fs::write(
            index_root.join("root-a.json"),
            serde_json::to_string(&json!({
                "schema_version": 1,
                "root": source_root.to_string_lossy(),
                "root_signature": {
                    "modified_unix_millis": 0,
                    "size_bytes": 0
                },
                "generated_at": "200",
                "directories": [],
                "entries": [{
                    "modality": "image",
                    "path": source_root.join("spring.jpg").to_string_lossy(),
                    "title": "spring.jpg",
                    "searchable_text": "user sidecar only",
                    "text_sources": [{
                        "source_kind": "sidecar",
                        "text": "user sidecar only"
                    }],
                    "file_signature": {
                        "modified_unix_millis": 0,
                        "size_bytes": 9
                    }
                }]
            }))
            .expect("serialize manifest"),
        )
        .expect("write manifest");
        let settings = KnowledgeSettings {
            source_roots: vec![KnowledgeSourceRoot {
                root_id: "test-root".to_string(),
                label: "Test root".to_string(),
                path: source_root.to_string_lossy().into_owned(),
                enabled: true,
                include: Vec::new(),
                exclude: Vec::new(),
                last_indexed_at: None,
            }],
            index_root: index_root.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let endpoint =
            |model_kind: ModelKind, model_endpoint_id: &str, model_name: &str| ModelEndpoint {
                model_endpoint_id: model_endpoint_id.to_string(),
                workspace_id: Some("home-1".to_string()),
                provider_account_id: None,
                model_kind,
                endpoint_kind: ModelEndpointKind::Local,
                provider_key: "openai_compatible".to_string(),
                model_name: model_name.to_string(),
                capability_tags: Vec::new(),
                cost_policy: json!({}),
                status: ModelEndpointStatus::Active,
                metadata: json!({}),
            };
        let response = build_rag_readiness_response(
            &LocalModelRuntimeProjection {
                ready: true,
                backend_ready: true,
                embedding_model: Some("jina".to_string()),
                ..Default::default()
            },
            &settings,
            &[
                endpoint(ModelKind::Embedder, "embedder-local", "jina"),
                endpoint(ModelKind::Vlm, "vlm-local", "cpu-vlm"),
                endpoint(ModelKind::Ocr, "ocr-local", "tesseract"),
            ],
            &[],
            &[],
        );

        let image_rag = response
            .capability_profiles
            .iter()
            .find(|profile| profile.capability_id == "image_rag")
            .expect("image rag profile");
        assert_eq!(image_rag.status, "degraded");
        assert!(image_rag
            .evidence
            .iter()
            .any(|entry| entry == "vlm_indexed_image_count=0"));
        assert!(response
            .warnings
            .iter()
            .any(|warning| warning.contains("Image RAG is degraded")));

        let _ = fs::remove_dir_all(source_root);
        let _ = fs::remove_dir_all(index_root);
    }

    #[test]
    fn knowledge_index_status_counts_manifest_cache_and_storage() {
        let index_root = unique_store_path("harborbeacon-knowledge-index-status");
        fs::create_dir_all(&index_root).expect("create index root");
        fs::write(
            index_root.join("root-a.json"),
            serde_json::to_string(&json!({
                "schema_version": 1,
                "root": "/tmp/root-a",
                "root_signature": {
                    "modified_unix_millis": 0,
                    "size_bytes": 0
                },
                "generated_at": "200",
                "directories": [],
                "entries": [{
                    "modality": "document",
                    "path": "/tmp/root-a/doc.md",
                    "title": "doc.md",
                    "searchable_text": "hello",
                    "file_signature": {
                        "modified_unix_millis": 0,
                        "size_bytes": 5
                    }
                }, {
                    "modality": "image",
                    "path": "/tmp/root-a/spring.jpg",
                    "title": "spring.jpg",
                    "searchable_text": "春天的公园",
                    "text_sources": [{
                        "source_kind": "vlm",
                        "provider_key": "mock-vlm",
                        "text": "春天的公园"
                    }],
                    "file_signature": {
                        "modified_unix_millis": 0,
                        "size_bytes": 9
                    }
                }, {
                    "modality": "image",
                    "path": "/tmp/root-a/no-content.jpg",
                    "title": "no-content.jpg",
                    "searchable_text": "",
                    "file_signature": {
                        "modified_unix_millis": 0,
                        "size_bytes": 9
                    }
                }]
            }))
            .expect("serialize manifest"),
        )
        .expect("write manifest");
        fs::write(
            index_root.join("root-a.embeddings.json"),
            serde_json::to_string(&json!({
                "schema_version": 1,
                "root": "/tmp/root-a",
                "entries": [{
                    "key": "chunk-1",
                    "path": "/tmp/root-a/doc.md",
                    "text_hash": "abc",
                    "vector": [0.1, 0.2]
                }]
            }))
            .expect("serialize embedding store"),
        )
        .expect("write embedding store");

        let response = build_knowledge_index_status_response(KnowledgeSettings {
            index_root: index_root.to_string_lossy().into_owned(),
            ..Default::default()
        });

        assert_eq!(response.manifest_count, 1);
        assert_eq!(response.manifest_entry_count, 3);
        assert_eq!(response.document_count, 1);
        assert_eq!(response.image_count, 2);
        assert_eq!(response.content_indexed_image_count, 1);
        assert_eq!(response.vlm_indexed_image_count, 1);
        assert_eq!(response.ocr_indexed_image_count, 0);
        assert_eq!(response.image_content_missing_count, 1);
        assert_eq!(response.image_text_source_counts.get("vlm"), Some(&1));
        assert_eq!(response.embedding_cache_count, 1);
        assert_eq!(response.embedding_entry_count, 1);
        assert!(response.storage_usage_bytes > 0);
        assert_eq!(response.last_indexed_at.as_deref(), Some("200"));
        let _ = fs::remove_dir_all(index_root);
    }

    #[test]
    fn admin_knowledge_search_request_uses_enabled_roots_and_clamps_limit() {
        let settings = KnowledgeSettings {
            source_roots: vec![
                KnowledgeSourceRoot {
                    root_id: "enabled".to_string(),
                    label: "Enabled".to_string(),
                    path: "/mnt/source-a".to_string(),
                    enabled: true,
                    include: Vec::new(),
                    exclude: Vec::new(),
                    last_indexed_at: None,
                },
                KnowledgeSourceRoot {
                    root_id: "disabled".to_string(),
                    label: "Disabled".to_string(),
                    path: "/mnt/source-b".to_string(),
                    enabled: false,
                    include: Vec::new(),
                    exclude: Vec::new(),
                    last_indexed_at: None,
                },
            ],
            index_root: "/mnt/index".to_string(),
            ..Default::default()
        };

        let request = build_admin_knowledge_search_request(
            KnowledgeSearchApiRequest {
                query: " 找到春天照片 ".to_string(),
                limit: Some(500),
                include_documents: Some(false),
                include_images: None,
                include_videos: Some(true),
                source_scope: None,
                camera_id: None,
                from: None,
                to: None,
            },
            &settings,
            vec!["/mnt/source-a/camera-main/segment.mp4".to_string()],
            Vec::new(),
        )
        .expect("search request");

        assert_eq!(request.query, "找到春天照片");
        assert_eq!(request.limit, 50);
        assert_eq!(request.configured_roots, vec!["/mnt/source-a".to_string()]);
        assert_eq!(request.roots, vec!["/mnt/source-a".to_string()]);
        assert!(!request.include_documents);
        assert!(request.include_images);
        assert!(request.include_videos);
        assert_eq!(request.index_root.as_deref(), Some("/mnt/index"));
        assert_eq!(
            request.focus_paths,
            vec!["/mnt/source-a/camera-main/segment.mp4"]
        );
    }

    #[test]
    fn admin_search_request_detects_and_validates_dvr_focus_fields() {
        let payload = KnowledgeSearchApiRequest {
            query: "谁在倒饮料".to_string(),
            limit: None,
            include_documents: None,
            include_images: None,
            include_videos: Some(true),
            source_scope: None,
            camera_id: Some(" camera-main ".to_string()),
            from: Some("1714600000".to_string()),
            to: Some("1714600300".to_string()),
        };

        assert!(payload.has_dvr_focus());
        assert_eq!(
            parse_optional_unix_seconds(payload.from.as_deref(), "from").unwrap(),
            Some(1_714_600_000)
        );
        assert_eq!(
            parse_optional_unix_seconds(payload.to.as_deref(), "to").unwrap(),
            Some(1_714_600_300)
        );
        assert!(parse_optional_unix_seconds(Some("today"), "from")
            .unwrap_err()
            .contains("Unix seconds"));
    }

    #[test]
    fn knowledge_preview_allows_indexed_files_under_enabled_source_root() {
        let source_root = unique_store_path("harbor-assistant-search-preview-source");
        let index_root = unique_store_path("harbor-assistant-search-preview-index");
        fs::create_dir_all(&source_root).expect("create source root");
        fs::create_dir_all(&index_root).expect("create index root");
        let indexed_path = source_root.join("indexed.md");
        fs::write(&indexed_path, "Harbor Assistant Search indexed preview")
            .expect("write indexed file");
        let service = KnowledgeIndexService::from_config(
            KnowledgeIndexConfig::new(index_root.clone()).unwrap(),
        )
        .expect("index service");
        service
            .load_or_refresh(&source_root)
            .expect("write index manifest");
        let settings = KnowledgeSettings {
            source_roots: vec![KnowledgeSourceRoot {
                root_id: "source".to_string(),
                label: "Source".to_string(),
                path: source_root.to_string_lossy().into_owned(),
                enabled: true,
                include: Vec::new(),
                exclude: Vec::new(),
                last_indexed_at: Some("1".to_string()),
            }],
            index_root: index_root.to_string_lossy().into_owned(),
            ..Default::default()
        };

        let resolved = resolve_knowledge_preview_path(&indexed_path.to_string_lossy(), &settings)
            .expect("preview path");

        assert_eq!(
            resolved,
            indexed_path.canonicalize().expect("canonical path")
        );
        let _ = fs::remove_dir_all(source_root);
        let _ = fs::remove_dir_all(index_root);
    }

    #[test]
    fn knowledge_preview_rejects_outside_unindexed_directory_and_unsupported_files() {
        let source_root = unique_store_path("harbor-assistant-search-preview-guard-source");
        let outside_root = unique_store_path("harbor-assistant-search-preview-guard-outside");
        let index_root = unique_store_path("harbor-assistant-search-preview-guard-index");
        fs::create_dir_all(&source_root).expect("create source root");
        fs::create_dir_all(&outside_root).expect("create outside root");
        fs::create_dir_all(&index_root).expect("create index root");
        let indexed_path = source_root.join("indexed.md");
        fs::write(&indexed_path, "indexed").expect("write indexed file");
        let service = KnowledgeIndexService::from_config(
            KnowledgeIndexConfig::new(index_root.clone()).unwrap(),
        )
        .expect("index service");
        service
            .load_or_refresh(&source_root)
            .expect("write index manifest");
        let unindexed_path = source_root.join("unindexed.md");
        fs::write(&unindexed_path, "not indexed").expect("write unindexed file");
        let unsupported_path = source_root.join("payload.bin");
        fs::write(&unsupported_path, "not previewable").expect("write unsupported file");
        let outside_path = outside_root.join("outside.md");
        fs::write(&outside_path, "outside").expect("write outside file");
        let settings = KnowledgeSettings {
            source_roots: vec![KnowledgeSourceRoot {
                root_id: "source".to_string(),
                label: "Source".to_string(),
                path: source_root.to_string_lossy().into_owned(),
                enabled: true,
                include: Vec::new(),
                exclude: Vec::new(),
                last_indexed_at: Some("1".to_string()),
            }],
            index_root: index_root.to_string_lossy().into_owned(),
            ..Default::default()
        };

        let outside_error =
            resolve_knowledge_preview_path(&outside_path.to_string_lossy(), &settings)
                .expect_err("outside root rejected");
        assert_eq!(outside_error.status, StatusCode(403));

        let unindexed_error =
            resolve_knowledge_preview_path(&unindexed_path.to_string_lossy(), &settings)
                .expect_err("unindexed path rejected");
        assert_eq!(unindexed_error.status, StatusCode(404));

        let directory_error =
            resolve_knowledge_preview_path(&source_root.to_string_lossy(), &settings)
                .expect_err("directory rejected");
        assert_eq!(directory_error.status, StatusCode(400));

        let unsupported_error =
            resolve_knowledge_preview_path(&unsupported_path.to_string_lossy(), &settings)
                .expect_err("unsupported mime rejected");
        assert_eq!(unsupported_error.status, StatusCode(415));

        let _ = fs::remove_dir_all(source_root);
        let _ = fs::remove_dir_all(outside_root);
        let _ = fs::remove_dir_all(index_root);
    }

    #[test]
    fn parse_knowledge_index_job_cancel_route_extracts_job_id() {
        assert_eq!(
            parse_knowledge_index_job_cancel_path("/api/knowledge/index/jobs/job-123/cancel"),
            Some("job-123".to_string())
        );
        assert_eq!(
            parse_knowledge_index_job_cancel_path("/api/knowledge/index/jobs//cancel"),
            None
        );
        assert_eq!(
            parse_knowledge_index_job_cancel_path("/api/knowledge/index/jobs/job-123"),
            None
        );
    }

    #[test]
    fn embedding_warmup_timeout_stats_complete_job_as_degraded() {
        let stats = embedding_warmup_timeout_stats(2, Duration::from_millis(25));

        assert_eq!(stats.total, 2);
        assert_eq!(stats.completed, 0);
        assert_eq!(stats.skipped, 0);
        assert_eq!(stats.failed, 2);
        assert!(stats.degraded);
        assert!(stats.persist_error.is_none());
        assert!(stats
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("exceeded 25 ms"));
    }

    #[test]
    fn knowledge_index_worker_completes_job_and_updates_source_root() {
        let admin_path = unique_store_path("harborbeacon-knowledge-index-worker-admin");
        let registry_path = unique_store_path("harborbeacon-knowledge-index-worker-registry");
        let source_root = unique_store_path("harborbeacon-knowledge-index-worker-root");
        let index_root = unique_store_path("harborbeacon-knowledge-index-worker-index");
        fs::create_dir_all(&source_root).expect("create source root");
        fs::create_dir_all(&index_root).expect("create index root");
        fs::write(source_root.join("note.md"), "worker indexed note").expect("write source doc");

        let store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        let settings = KnowledgeSettings {
            source_roots: vec![KnowledgeSourceRoot {
                root_id: "worker-root".to_string(),
                label: "Worker Root".to_string(),
                path: source_root.to_string_lossy().into_owned(),
                enabled: true,
                include: Vec::new(),
                exclude: Vec::new(),
                last_indexed_at: None,
            }],
            index_root: index_root.to_string_lossy().into_owned(),
            ..Default::default()
        };
        store
            .save_knowledge_settings(settings.clone())
            .expect("save settings");
        let job = build_knowledge_index_job(
            &settings.source_roots[0],
            "100",
            settings.default_resource_profile,
        );
        store
            .save_knowledge_index_job(job.clone())
            .expect("save job");

        let worker_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        run_knowledge_index_jobs(worker_store, settings, vec![job]);

        let jobs = store
            .list_knowledge_index_jobs()
            .expect("list knowledge index jobs");
        assert_eq!(jobs[0].status, "completed");
        assert_eq!(jobs[0].progress_percent, Some(100));
        assert_eq!(jobs[0].checkpoint["phase"], "completed");
        assert_eq!(jobs[0].checkpoint["embedding_total"], 1);
        assert!(jobs[0].checkpoint["embedding_completed"].is_number());
        assert!(jobs[0].checkpoint["embedding_skipped"].is_number());
        assert!(jobs[0].checkpoint["embedding_failed"].is_number());
        assert!(jobs[0].checkpoint["embedding_warmup_degraded"].is_boolean());
        assert!(jobs[0].checkpoint["embedding_warmup_timeout_ms"].is_number());
        let updated_settings = store.knowledge_settings().expect("load settings");
        assert!(updated_settings.source_roots[0].last_indexed_at.is_some());
        assert!(index_root
            .read_dir()
            .expect("list index root")
            .flatten()
            .any(
                |entry| entry.path().extension().and_then(|value| value.to_str()) == Some("json")
            ));

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_dir_all(source_root);
        let _ = fs::remove_dir_all(index_root);
    }

    #[test]
    fn knowledge_index_worker_honors_queued_cancel() {
        let admin_path = unique_store_path("harborbeacon-knowledge-index-cancel-admin");
        let registry_path = unique_store_path("harborbeacon-knowledge-index-cancel-registry");
        let source_root = unique_store_path("harborbeacon-knowledge-index-cancel-root");
        let index_root = unique_store_path("harborbeacon-knowledge-index-cancel-index");
        fs::create_dir_all(&source_root).expect("create source root");
        fs::create_dir_all(&index_root).expect("create index root");
        fs::write(source_root.join("note.md"), "canceled note").expect("write source doc");

        let store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        let settings = KnowledgeSettings {
            source_roots: vec![KnowledgeSourceRoot {
                root_id: "cancel-root".to_string(),
                label: "Cancel Root".to_string(),
                path: source_root.to_string_lossy().into_owned(),
                enabled: true,
                include: Vec::new(),
                exclude: Vec::new(),
                last_indexed_at: None,
            }],
            index_root: index_root.to_string_lossy().into_owned(),
            ..Default::default()
        };
        store
            .save_knowledge_settings(settings.clone())
            .expect("save settings");
        let job = build_knowledge_index_job(
            &settings.source_roots[0],
            "100",
            settings.default_resource_profile,
        );
        store
            .save_knowledge_index_job(job.clone())
            .expect("save job");
        store
            .cancel_knowledge_index_job(&job.job_id, "101".to_string())
            .expect("cancel job");

        let worker_store = AdminConsoleStore::new(
            admin_path.clone(),
            DeviceRegistryStore::new(registry_path.clone()),
        );
        run_knowledge_index_jobs(worker_store, settings, vec![job]);

        let jobs = store
            .list_knowledge_index_jobs()
            .expect("list knowledge index jobs");
        assert_eq!(jobs[0].status, "canceled");
        assert_eq!(jobs[0].checkpoint["phase"], "canceled_before_start");
        let updated_settings = store.knowledge_settings().expect("load settings");
        assert!(updated_settings.source_roots[0].last_indexed_at.is_none());

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_dir_all(source_root);
        let _ = fs::remove_dir_all(index_root);
    }

    #[test]
    fn files_browse_lists_configured_root_without_writes() {
        let source_root = std::env::temp_dir().join(format!(
            "harborbeacon-files-browse-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let nested = source_root.join("MM-test");
        fs::create_dir_all(&nested).expect("create nested directory");
        fs::write(source_root.join("note.txt"), "sample").expect("write sample file");
        let settings = KnowledgeSettings {
            source_roots: vec![KnowledgeSourceRoot {
                root_id: "sample".to_string(),
                label: "Sample".to_string(),
                path: source_root.to_string_lossy().into_owned(),
                enabled: true,
                include: Vec::new(),
                exclude: Vec::new(),
                last_indexed_at: None,
            }],
            index_root: source_root
                .join("index-root")
                .to_string_lossy()
                .into_owned(),
            ..Default::default()
        };

        let response = build_files_browse_response(Some(&source_root.to_string_lossy()), &settings)
            .expect("browse configured root");

        assert!(response.readonly);
        assert!(response
            .entries
            .iter()
            .any(|entry| entry.name == "MM-test" && entry.is_dir));
        assert!(response
            .entries
            .iter()
            .any(|entry| entry.name == "note.txt" && !entry.is_dir));

        let outside = std::env::temp_dir().join("harborbeacon-outside-browse");
        let denied = build_files_browse_response(Some(&outside.to_string_lossy()), &settings);
        assert!(denied.is_err());

        let _ = fs::remove_dir_all(source_root);
    }

    #[test]
    fn readiness_evidence_records_redact_stream_credentials() {
        let item = release_item(
            "camera-check",
            "Camera check",
            "harbor-aiot",
            "blocked",
            "blocked",
            "rtsp://admin:secret@192.168.1.20:554/stream1 failed",
            "POST /api/devices/cam/rtsp-check",
            "/devices-aiot",
            vec!["stream=rtsp://admin:secret@192.168.1.20:554/stream1".to_string()],
        );
        let text = serde_json::to_string(&item).expect("json");

        assert!(!text.contains("admin:secret"));
        assert!(!text.contains("rtsp://admin"));
        assert!(text.contains("redacted"));
    }

    #[test]
    fn rtsp_metadata_patch_url_does_not_inject_credentials() {
        let mut device = CameraDevice::new(
            "cam-1",
            "Front Door",
            "rtsp://admin:secret@192.168.1.10:554/old",
        );
        device.ip_address = Some("192.168.1.10".to_string());

        let url =
            build_rtsp_url_from_patch(&device, Some("stream1"), Some(8554)).expect("rtsp url");

        assert_eq!(url, "rtsp://192.168.1.10:8554/stream1");
        assert!(!url.contains("secret"));
        assert!(!url.contains("admin:"));
    }

    #[test]
    fn device_evidence_response_exposes_recent_checks_without_secrets() {
        let registry_path = unique_store_path("harborbeacon-device-evidence-registry");
        let admin_path = unique_store_path("harborbeacon-device-evidence-state");
        let conversation_path = unique_store_path("harborbeacon-device-evidence-conversations");
        let registry_store = DeviceRegistryStore::new(registry_path.clone());
        let admin_store = AdminConsoleStore::new(admin_path.clone(), registry_store.clone());
        let conversation_store = TaskConversationStore::new(conversation_path.clone());

        let mut device = CameraDevice::new(
            "cam-secret",
            "Secret Camera",
            "rtsp://admin:secret@192.168.1.10:554/stream1",
        );
        device.snapshot_url =
            Some("http://admin:secret@192.168.1.10/snapshot.jpg?token=abc".to_string());
        registry_store
            .save_devices(&[device.clone()])
            .expect("save device");
        admin_store
            .save_device_credential(DeviceCredentialSecret {
                device_id: "cam-secret".to_string(),
                username: "admin".to_string(),
                password: "secret".to_string(),
                rtsp_port: Some(554),
                rtsp_paths: vec!["/stream1".to_string()],
                updated_at: Some("100".to_string()),
                last_verified_at: Some("101".to_string()),
            })
            .expect("save credential");
        admin_store
            .record_device_evidence(DeviceEvidenceRecord {
                evidence_id: "rtsp-evidence".to_string(),
                device_id: "cam-secret".to_string(),
                evidence_kind: "rtsp_check".to_string(),
                status: "passed".to_string(),
                observed_at: "200".to_string(),
                summary: "rtsp://admin:secret@192.168.1.10:554/stream1 ok".to_string(),
                details: json!({
                    "stream_url": "rtsp://admin:secret@192.168.1.10:554/stream1",
                    "api_token": "raw-token"
                }),
            })
            .expect("record rtsp evidence");
        admin_store
            .record_device_evidence(DeviceEvidenceRecord {
                evidence_id: "snapshot-evidence".to_string(),
                device_id: "cam-secret".to_string(),
                evidence_kind: "snapshot_check".to_string(),
                status: "passed".to_string(),
                observed_at: "201".to_string(),
                summary: "snapshot ok".to_string(),
                details: json!({
                    "snapshot_url": "http://admin:secret@192.168.1.10/snapshot.jpg?token=abc"
                }),
            })
            .expect("record snapshot evidence");

        conversation_store
            .save_share_link_bundle(
                &MediaSession {
                    media_session_id: "media-session-active".to_string(),
                    device_id: "cam-secret".to_string(),
                    stream_profile_id: "stream-cam-secret-primary".to_string(),
                    session_kind: MediaSessionKind::Share,
                    delivery_mode: MediaDeliveryMode::Hls,
                    opened_by_user_id: Some("local-owner".to_string()),
                    status: MediaSessionStatus::Active,
                    share_link_id: Some("share-link-active".to_string()),
                    started_at: Some("202".to_string()),
                    ended_at: None,
                    metadata: json!({
                        "stream_url": "rtsp://admin:secret@192.168.1.10:554/stream1"
                    }),
                },
                &ShareLink {
                    share_link_id: "share-link-active".to_string(),
                    media_session_id: "media-session-active".to_string(),
                    token_hash: "token-hash".to_string(),
                    access_scope: ShareAccessScope::PublicLink,
                    expires_at: Some("9999999999".to_string()),
                    revoked_at: None,
                },
            )
            .expect("save share link");

        let task_service = TaskApiService::new(admin_store.clone(), conversation_store);
        let api = AdminApi::new(
            admin_store,
            task_service,
            PathBuf::from("frontend/harbor-assistant/dist/harbor-assistant"),
            "http://harborbeacon.local:4174".to_string(),
        );
        let response = api
            .build_device_evidence_response(&device)
            .expect("evidence response");
        let payload = serde_json::to_string(&response).expect("serialize response");

        assert!(response.credential_status.configured);
        assert!(response.credential_status.redacted);
        assert_eq!(
            response
                .recent_rtsp_check
                .as_ref()
                .map(|record| record.status.as_str()),
            Some("passed")
        );
        assert_eq!(
            response
                .recent_snapshot_check
                .as_ref()
                .map(|record| record.status.as_str()),
            Some("passed")
        );
        assert_eq!(response.share_links.len(), 1);
        assert_eq!(response.share_links[0].status, "active");
        assert!(!payload.contains("admin:secret"));
        assert!(!payload.contains("raw-token"));
        assert!(!payload.contains("token=abc"));
        assert!(payload.contains("redacted:redacted@192.168.1.10"));

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    #[test]
    fn model_endpoint_response_redacts_secret_metadata() {
        let payload = redact_model_endpoint_response(&[ModelEndpoint {
            model_endpoint_id: "llm-cloud".to_string(),
            workspace_id: Some("home-1".to_string()),
            provider_account_id: None,
            model_kind: ModelKind::Llm,
            endpoint_kind: ModelEndpointKind::Cloud,
            provider_key: "custom".to_string(),
            model_name: "demo".to_string(),
            capability_tags: vec!["chat".to_string()],
            cost_policy: json!({}),
            status: ModelEndpointStatus::Active,
            metadata: json!({
                "base_url": "https://api.example.com/v1",
                "api_key": "super-secret",
                "nested": {
                    "token": "hidden-token"
                }
            }),
        }]);

        assert_eq!(payload.endpoints.len(), 1);
        assert_eq!(payload.endpoints[0].metadata["api_key"], json!(""));
        assert_eq!(
            payload.endpoints[0].metadata["api_key_configured"],
            json!(true)
        );
        assert_eq!(payload.endpoints[0].metadata["nested"]["token"], json!(""));
        assert_eq!(
            payload.endpoints[0].metadata["nested"]["token_configured"],
            json!(true)
        );
    }

    #[test]
    fn installed_model_selection_activates_runtime_and_updates_endpoint() {
        let registry_path = unique_store_path("harborbeacon-model-select-registry");
        let admin_path = unique_store_path("harborbeacon-model-select-state");
        let conversation_path = unique_store_path("harborbeacon-model-select-conversations");
        let model_store = unique_store_path("harborbeacon-model-select-store");
        let model_dir = model_store.join("qwen2.5-1.5b-instruct");
        fs::create_dir_all(&model_dir).expect("create model dir");
        fs::write(model_dir.join("config.json"), "{}").expect("write model file");

        let registry_store = DeviceRegistryStore::new(registry_path.clone());
        let admin_store = AdminConsoleStore::new(admin_path.clone(), registry_store);
        admin_store
            .save_model_store_root(&model_store.to_string_lossy())
            .expect("save model store root");
        admin_store
            .install_model_runtime("harbor-candle")
            .expect("enable candle runtime");
        let conversation_store = TaskConversationStore::new(conversation_path.clone());
        let task_service = TaskApiService::new(admin_store.clone(), conversation_store);
        let seen_requests: Arc<Mutex<Vec<ModelRuntimeActivationRequest>>> =
            Arc::new(Mutex::new(Vec::new()));
        let seen_for_handler = seen_requests.clone();
        let api = AdminApi::new(
            admin_store.clone(),
            task_service,
            PathBuf::from("frontend/harbor-assistant/dist/harbor-assistant"),
            "http://harborbeacon.local:4174".to_string(),
        )
        .with_model_runtime_activation_handler(Arc::new(move |request| {
            seen_for_handler
                .lock()
                .expect("request lock")
                .push(request.clone());
            Ok(ModelRuntimeActivationResult {
                activated: true,
                status: "activated".to_string(),
                message: "runtime switched".to_string(),
                runtime_model_id: Some(request.model_id.clone()),
            })
        }));
        api.admin_store
            .save_model_capability_binding("semantic_router", "qwen2.5-1.5b-instruct")
            .expect("save binding");

        api.activate_selected_model_capability("semantic_router", "qwen2.5-1.5b-instruct")
            .expect("activate selected model");

        let requests = seen_requests.lock().expect("request lock");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].model_kind, ModelKind::Llm);
        assert_eq!(requests[0].model_id, "qwen2.5-1.5b-instruct");
        assert_eq!(
            requests[0].local_path.as_deref(),
            Some(model_dir.to_string_lossy().as_ref())
        );
        assert_eq!(
            requests[0].runtime_profiles,
            vec![
                "harbor-candle".to_string(),
                "harbor-model-api-candle".to_string()
            ]
        );
        drop(requests);

        let state = admin_store.load_state().expect("load state");
        let endpoint = state
            .models
            .endpoints
            .iter()
            .find(|endpoint| endpoint.model_endpoint_id == "llm-local-openai-compatible")
            .expect("llm endpoint");
        assert_eq!(endpoint.status, ModelEndpointStatus::Active);
        assert_eq!(endpoint.model_name, "qwen2.5-1.5b-instruct");
        assert_eq!(endpoint.metadata["runtime_auto_activation"], json!(true));
        assert_eq!(endpoint.metadata["activation_status"], json!("activated"));
        assert_eq!(
            endpoint.metadata["catalog_model_id"],
            json!("qwen2.5-1.5b-instruct")
        );

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
        let _ = fs::remove_dir_all(model_store);
    }

    #[test]
    fn external_openai_compatible_catalog_model_is_not_auto_started() {
        let registry_path = unique_store_path("harborbeacon-model-external-registry");
        let admin_path = unique_store_path("harborbeacon-model-external-state");
        let conversation_path = unique_store_path("harborbeacon-model-external-conversations");
        let model_store = unique_store_path("harborbeacon-model-external-store");
        let model_dir = model_store.join("qwen-qwen3.5-4b");
        fs::create_dir_all(&model_dir).expect("create model dir");
        fs::write(model_dir.join("config.json"), "{}").expect("write model file");

        let registry_store = DeviceRegistryStore::new(registry_path.clone());
        let admin_store = AdminConsoleStore::new(admin_path.clone(), registry_store);
        admin_store
            .save_model_store_root(&model_store.to_string_lossy())
            .expect("save model store root");
        let conversation_store = TaskConversationStore::new(conversation_path.clone());
        let task_service = TaskApiService::new(admin_store.clone(), conversation_store);
        let seen_requests: Arc<Mutex<Vec<ModelRuntimeActivationRequest>>> =
            Arc::new(Mutex::new(Vec::new()));
        let seen_for_handler = seen_requests.clone();
        let api = AdminApi::new(
            admin_store,
            task_service,
            PathBuf::from("frontend/harbor-assistant/dist/harbor-assistant"),
            "http://harborbeacon.local:4174".to_string(),
        )
        .with_model_runtime_activation_handler(Arc::new(move |request| {
            seen_for_handler
                .lock()
                .expect("request lock")
                .push(request.clone());
            Ok(ModelRuntimeActivationResult {
                activated: true,
                status: "activated".to_string(),
                message: "runtime switched".to_string(),
                runtime_model_id: Some(request.model_id.clone()),
            })
        }));

        let error = api
            .activate_selected_model_capability("semantic_router", "Qwen/Qwen3.5-4B")
            .expect_err("external runtime model should not be auto-started");

        assert!(error.contains("OpenAI-compatible runtime"));
        assert!(seen_requests.lock().expect("request lock").is_empty());

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
        let _ = fs::remove_dir_all(model_store);
    }

    #[test]
    fn runtime_overlay_promotes_live_local_llm_and_embedder_rows() {
        let mut endpoints =
            harborbeacon_local_agent::runtime::admin_console::default_model_endpoints();
        for endpoint in &mut endpoints {
            if matches!(
                endpoint.model_kind,
                ModelKind::Llm | ModelKind::Embedder | ModelKind::Vlm
            ) {
                endpoint.status = ModelEndpointStatus::Disabled;
                endpoint.metadata = json!({
                    "builtin": true,
                    "base_url": "",
                    "healthz_url": "",
                    "api_key": "",
                    "api_key_configured": false,
                });
            }
        }

        let overlayed = overlay_model_endpoints_with_runtime_truth(
            &endpoints,
            &LocalModelRuntimeProjection {
                ready: true,
                backend_ready: true,
                backend_kind: Some("candle".to_string()),
                chat_model: Some("/models/qwen".to_string()),
                embedding_model: Some("/models/jina".to_string()),
                ..Default::default()
            },
        );

        let llm = overlayed
            .iter()
            .find(|endpoint| endpoint.model_endpoint_id == "llm-local-openai-compatible")
            .expect("llm endpoint");
        assert_eq!(llm.status, ModelEndpointStatus::Active);
        assert_eq!(llm.metadata["projection_mismatch"], json!(true));
        assert_ne!(llm.metadata["base_url"], json!(""));
        assert_eq!(llm.metadata["runtime_backend_kind"], json!("candle"));

        let embedder = overlayed
            .iter()
            .find(|endpoint| endpoint.model_endpoint_id == "embed-local-openai-compatible")
            .expect("embedder endpoint");
        assert_eq!(embedder.status, ModelEndpointStatus::Active);
        assert_eq!(embedder.metadata["api_key_configured"], json!(true));

        let vlm = overlayed
            .iter()
            .find(|endpoint| endpoint.model_endpoint_id == "vlm-local-openai-compatible")
            .expect("vlm endpoint");
        assert_eq!(vlm.status, ModelEndpointStatus::Disabled);
    }

    #[test]
    fn runtime_probe_falls_back_to_builtin_local_endpoint_urls() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listener");
        let addr = listener.local_addr().expect("local addr");
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut buffer = [0_u8; 1024];
            let _ = stream.read(&mut buffer);
            let body = json!({
                "service": "harbor-model-api",
                "status": "ok",
                "backend": {
                    "kind": "candle",
                    "ready": true
                },
                "chat_model": "/models/qwen",
                "embedding_model": "/models/jina",
                "ready": true
            })
            .to_string();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        });

        let _base_url = EnvGuard::set("HARBOR_MODEL_API_BASE_URL", &format!("http://{addr}/v1"));
        let mut endpoints =
            harborbeacon_local_agent::runtime::admin_console::default_model_endpoints();
        for endpoint in &mut endpoints {
            if matches!(endpoint.model_kind, ModelKind::Llm | ModelKind::Embedder) {
                endpoint.metadata = json!({
                    "builtin": true,
                    "base_url": "",
                    "healthz_url": "",
                    "api_key": "",
                    "api_key_configured": false
                });
            }
        }

        let projection = probe_local_model_runtime(&endpoints);

        assert!(projection.ready);
        assert!(projection.backend_ready);
        assert_eq!(projection.backend_kind.as_deref(), Some("candle"));
        assert_eq!(projection.chat_model.as_deref(), Some("/models/qwen"));
        assert_eq!(projection.embedding_model.as_deref(), Some("/models/jina"));
        assert!(projection.api_key_configured);
        assert_eq!(projection.healthz_url, format!("http://{addr}/healthz"));

        server.join().expect("server join");
    }

    #[test]
    fn feature_availability_prefers_runtime_truth_for_embed_and_answer() {
        let registry_path = unique_store_path("harborbeacon-feature-runtime-registry");
        let admin_path = unique_store_path("harborbeacon-feature-runtime-state");
        let registry_store = DeviceRegistryStore::new(registry_path.clone());
        let admin_store = AdminConsoleStore::new(admin_path.clone(), registry_store);
        let mut state = admin_store.load_or_create_state().expect("state");
        for endpoint in &mut state.models.endpoints {
            if matches!(endpoint.model_kind, ModelKind::Llm | ModelKind::Embedder) {
                endpoint.status = ModelEndpointStatus::Disabled;
                endpoint.metadata = json!({
                    "builtin": true,
                    "base_url": "",
                    "healthz_url": "",
                    "api_key": "",
                    "api_key_configured": false,
                });
            }
        }
        let account_management =
            harborbeacon_local_agent::runtime::admin_console::account_management_snapshot(
                &state,
                Some("http://harborbeacon.local:4174"),
            );
        let overlayed = overlay_model_endpoints_with_runtime_truth(
            &state.models.endpoints,
            &LocalModelRuntimeProjection {
                ready: true,
                backend_ready: true,
                backend_kind: Some("candle".to_string()),
                chat_model: Some("/models/qwen".to_string()),
                embedding_model: Some("/models/jina".to_string()),
                ..Default::default()
            },
        );

        let response = build_feature_availability_response(
            &overlayed,
            &state.models.route_policies,
            &account_management,
            None,
            &LocalModelRuntimeProjection {
                ready: true,
                backend_ready: true,
                backend_kind: Some("candle".to_string()),
                chat_model: Some("/models/qwen".to_string()),
                embedding_model: Some("/models/jina".to_string()),
                ..Default::default()
            },
        );

        let embed = response
            .groups
            .iter()
            .flat_map(|group| group.items.iter())
            .find(|item| item.feature_id == "retrieval.embed")
            .expect("embed feature");
        assert_eq!(embed.status, "available");
        assert!(embed
            .evidence
            .iter()
            .any(|entry| entry.contains("projection_mismatch")));

        let answer = response
            .groups
            .iter()
            .flat_map(|group| group.items.iter())
            .find(|item| item.feature_id == "retrieval.answer")
            .expect("answer feature");
        assert_eq!(answer.status, "available");
        assert!(answer
            .evidence
            .iter()
            .any(|entry| entry.contains("local_inference.backend.kind=candle")));

        let vision = response
            .groups
            .iter()
            .flat_map(|group| group.items.iter())
            .find(|item| item.feature_id == "retrieval.vision_summary")
            .expect("vision feature");
        assert_ne!(vision.status, "available");

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
    }

    #[test]
    fn feature_availability_surfaces_weixin_blocker_without_secret_material() {
        let registry_path = unique_store_path("harborbeacon-feature-weixin-registry");
        let admin_path = unique_store_path("harborbeacon-feature-weixin-state");
        let registry_store = DeviceRegistryStore::new(registry_path.clone());
        let admin_store = AdminConsoleStore::new(admin_path.clone(), registry_store);
        let state = admin_store.load_or_create_state().expect("state");
        let account_management =
            harborbeacon_local_agent::runtime::admin_console::account_management_snapshot(
                &state,
                Some("http://harborbeacon.local:4174"),
            );
        let gateway_payload = json!({
            "ok": true,
            "channels": [
                {
                    "platform": "weixin",
                    "connected": false,
                    "transport": {
                        "status": "error"
                    }
                }
            ],
            "weixin": {
                "blocker_category": "weixin_dns_resolution",
                "ingress_blocker_category": "getupdates",
                "status": "error",
                "poll": {
                    "status": "error",
                    "error": "<urlopen error [Errno 11001] getaddrinfo failed>"
                },
                "delivery_observability": {
                    "last_send_status": ""
                },
                "app_secret": "should-not-leak"
            },
            "release_v1": {
                "weixin_blocker_category": "getupdates"
            },
            "delivery_observability": {
                "record_count": 0
            }
        });

        let response = build_feature_availability_response(
            &state.models.endpoints,
            &state.models.route_policies,
            &account_management,
            Some(&gateway_payload),
            &LocalModelRuntimeProjection::default(),
        );

        let proactive = response
            .groups
            .iter()
            .flat_map(|group| group.items.iter())
            .find(|item| item.feature_id == "proactive_delivery")
            .expect("proactive delivery");
        assert_eq!(proactive.status, "blocked");
        assert_eq!(proactive.blocker, "weixin_dns_resolution");
        assert!(proactive
            .evidence
            .iter()
            .any(|entry| entry.contains("delivery_observability.record_count=0")));
        assert!(!proactive.blocker.contains("should-not-leak"));

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
    }

    #[test]
    fn feature_availability_keeps_runtime_features_available_while_weixin_blocker_isolated() {
        let registry_path = unique_store_path("harborbeacon-feature-isolated-runtime-registry");
        let admin_path = unique_store_path("harborbeacon-feature-isolated-runtime-state");
        let registry_store = DeviceRegistryStore::new(registry_path.clone());
        let admin_store = AdminConsoleStore::new(admin_path.clone(), registry_store);
        let mut state = admin_store.load_or_create_state().expect("state");
        for endpoint in &mut state.models.endpoints {
            if matches!(endpoint.model_kind, ModelKind::Llm | ModelKind::Embedder) {
                endpoint.status = ModelEndpointStatus::Disabled;
                endpoint.metadata = json!({
                    "builtin": true,
                    "base_url": "",
                    "healthz_url": "",
                    "api_key": "",
                    "api_key_configured": false,
                });
            }
        }
        let account_management =
            harborbeacon_local_agent::runtime::admin_console::account_management_snapshot(
                &state,
                Some("http://harborbeacon.local:4174"),
            );
        let runtime = LocalModelRuntimeProjection {
            ready: true,
            backend_ready: true,
            backend_kind: Some("candle".to_string()),
            chat_model: Some("/models/qwen".to_string()),
            embedding_model: Some("/models/jina".to_string()),
            ..Default::default()
        };
        let overlayed =
            overlay_model_endpoints_with_runtime_truth(&state.models.endpoints, &runtime);
        let gateway_payload = json!({
            "ok": true,
            "channels": [
                {
                    "platform": "weixin",
                    "connected": false,
                    "transport": {
                        "status": "error"
                    }
                }
            ],
            "weixin": {
                "blocker_category": "weixin_dns_resolution",
                "ingress_blocker_category": "getupdates",
                "status": "error",
                "poll": {
                    "status": "error",
                    "error": "<urlopen error [Errno 11001] getaddrinfo failed>"
                }
            },
            "release_v1": {
                "weixin_blocker_category": "getupdates"
            },
            "delivery_observability": {
                "record_count": 0
            }
        });

        let response = build_feature_availability_response(
            &overlayed,
            &state.models.route_policies,
            &account_management,
            Some(&gateway_payload),
            &runtime,
        );

        let answer = response
            .groups
            .iter()
            .flat_map(|group| group.items.iter())
            .find(|item| item.feature_id == "retrieval.answer")
            .expect("answer feature");
        assert_eq!(answer.status, "available");
        assert!(answer.blocker.is_empty());
        assert!(answer
            .evidence
            .iter()
            .any(|entry| entry.contains("local_inference.backend.kind=candle")));

        let proactive = response
            .groups
            .iter()
            .flat_map(|group| group.items.iter())
            .find(|item| item.feature_id == "proactive_delivery")
            .expect("proactive delivery");
        assert_eq!(proactive.status, "blocked");
        assert_eq!(proactive.blocker, "weixin_dns_resolution");

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
    }

    #[test]
    fn feature_availability_falls_back_to_release_v1_weixin_blocker_category() {
        let registry_path = unique_store_path("harborbeacon-feature-release-v1-weixin-registry");
        let admin_path = unique_store_path("harborbeacon-feature-release-v1-weixin-state");
        let registry_store = DeviceRegistryStore::new(registry_path.clone());
        let admin_store = AdminConsoleStore::new(admin_path.clone(), registry_store);
        let state = admin_store.load_or_create_state().expect("state");
        let account_management =
            harborbeacon_local_agent::runtime::admin_console::account_management_snapshot(
                &state,
                Some("http://harborbeacon.local:4174"),
            );
        let gateway_payload = json!({
            "ok": true,
            "channels": [
                {
                    "platform": "weixin",
                    "connected": false,
                    "transport": {
                        "status": "error"
                    }
                }
            ],
            "release_v1": {
                "weixin_blocker_category": "getupdates"
            },
            "delivery_observability": {
                "record_count": 0
            }
        });

        let response = build_feature_availability_response(
            &state.models.endpoints,
            &state.models.route_policies,
            &account_management,
            Some(&gateway_payload),
            &LocalModelRuntimeProjection::default(),
        );

        let proactive = response
            .groups
            .iter()
            .flat_map(|group| group.items.iter())
            .find(|item| item.feature_id == "proactive_delivery")
            .expect("proactive delivery");
        assert_eq!(proactive.status, "blocked");
        assert_eq!(proactive.blocker, "getupdates");

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
    }

    #[test]
    fn request_identity_hints_prefer_headers_then_query() {
        let headers = vec![
            Header::from_bytes(b"X-Harbor-Open-Id".as_slice(), b"ou_header".as_slice())
                .expect("header"),
            Header::from_bytes(b"X-Harbor-User-Id".as_slice(), b"user-header".as_slice())
                .expect("header"),
            Header::from_bytes(b"X-HarborOS-User".as_slice(), b"harbor".as_slice())
                .expect("header"),
        ];

        let hints = request_identity_hints(
            "/live/cameras/cam-1?open_id=ou_query&user_id=user-query&harboros_user=harbor-query",
            &headers,
        );
        assert_eq!(hints.open_id.as_deref(), Some("ou_header"));
        assert_eq!(hints.user_id.as_deref(), Some("user-header"));
        assert_eq!(hints.harboros_user_id.as_deref(), Some("harbor"));
    }

    #[test]
    fn deprecated_binding_routes_return_gone() {
        let admin_path = unique_store_path("harborbeacon-binding-gone-state");
        let registry_path = unique_store_path("harborbeacon-binding-gone-registry");
        let conversation_path = unique_store_path("harborbeacon-binding-gone-runtime");
        let registry_store = DeviceRegistryStore::new(registry_path.clone());
        let admin_store = AdminConsoleStore::new(admin_path.clone(), registry_store);
        admin_store
            .save_bridge_provider_status(BridgeProviderConfig {
                gateway_base_url: "http://gateway.local:8787".to_string(),
                ..Default::default()
            })
            .expect("save bridge provider");
        let task_service = TaskApiService::new(
            admin_store.clone(),
            TaskConversationStore::new(conversation_path.clone()),
        );
        let api = AdminApi::new(
            admin_store,
            task_service,
            PathBuf::from("frontend/harbor-assistant/dist/harbor-assistant"),
            "http://harborbeacon.local:4174".to_string(),
        );

        let qr_response = api.handle_binding_qr_svg(&AccessIdentityHints::default());
        let page_response =
            api.handle_mobile_setup_page("/setup/mobile", &AccessIdentityHints::default());

        assert_eq!(qr_response.status_code(), StatusCode(410));
        assert_eq!(page_response.status_code(), StatusCode(410));

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    #[test]
    fn gateway_service_request_requires_matching_bearer_token() {
        let headers = vec![Header::from_bytes(
            b"Authorization".as_slice(),
            b"Bearer shared-token".as_slice(),
        )
        .expect("header")];
        let _guard = EnvGuard::set("HARBORGATE_BEARER_TOKEN", "shared-token");

        assert!(authorize_gateway_service_request(&headers).is_ok());
        assert!(authorize_gateway_service_request(&[]).is_err());
    }

    #[test]
    fn identity_query_suffix_keeps_open_id_and_user_id() {
        assert_eq!(
            identity_query_suffix("/live/cameras/cam-1?open_id=ou_demo&user_id=u_demo"),
            "?open_id=ou_demo&user_id=u_demo"
        );
        assert!(identity_query_suffix("/live/cameras/cam-1").is_empty());
    }

    #[test]
    fn verify_shared_camera_token_requires_persisted_active_share_link() {
        let admin_path = unique_store_path("harborbeacon-admin-state");
        let registry_path = unique_store_path("harborbeacon-device-registry");
        let conversation_path = unique_store_path("harborbeacon-task-runtime");
        let registry_store = DeviceRegistryStore::new(registry_path.clone());
        let admin_store = AdminConsoleStore::new(admin_path.clone(), registry_store);
        admin_store
            .save_remote_view_config(RemoteViewConfig {
                share_secret: "platform-share-secret".to_string(),
                share_link_ttl_minutes: 45,
            })
            .expect("save remote view");
        let conversation_store = TaskConversationStore::new(conversation_path.clone());
        let task_service = TaskApiService::new(admin_store.clone(), conversation_store.clone());
        let api = AdminApi::new(
            admin_store,
            task_service,
            PathBuf::from("frontend/harbor-assistant/dist/harbor-assistant"),
            "http://harborbeacon.local:4174".to_string(),
        );

        let issued = remote_view::issue_camera_share_token("platform-share-secret", "cam-1", 15)
            .expect("issue token");
        let media_session = MediaSession {
            media_session_id: "media-session-1".to_string(),
            device_id: "cam-1".to_string(),
            stream_profile_id: "cam-1::stream::primary".to_string(),
            session_kind: MediaSessionKind::Share,
            delivery_mode: MediaDeliveryMode::Hls,
            opened_by_user_id: Some("user-1".to_string()),
            status: MediaSessionStatus::Active,
            share_link_id: Some("share-link-1".to_string()),
            started_at: Some(remote_view::now_unix_secs().to_string()),
            ended_at: None,
            metadata: json!({
                "task_id": "task-1",
            }),
        };
        let share_link = ShareLink {
            share_link_id: "share-link-1".to_string(),
            media_session_id: media_session.media_session_id.clone(),
            token_hash: remote_view::camera_share_token_hash(&issued.token),
            access_scope: ShareAccessScope::PublicLink,
            expires_at: Some(issued.expires_at_unix_secs.to_string()),
            revoked_at: None,
        };
        conversation_store
            .save_share_link_bundle(&media_session, &share_link)
            .expect("save share bundle");

        let claims = api
            .verify_shared_camera_token(&issued.token)
            .expect("claims");
        assert_eq!(claims.device_id, "cam-1");

        conversation_store
            .revoke_share_link(
                "share-link-1",
                Some(remote_view::now_unix_secs().to_string()),
            )
            .expect("revoke");
        assert!(api.verify_shared_camera_token(&issued.token).is_err());

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }

    #[test]
    fn list_share_links_surfaces_registered_status() {
        let admin_path = unique_store_path("harborbeacon-admin-state");
        let registry_path = unique_store_path("harborbeacon-device-registry");
        let conversation_path = unique_store_path("harborbeacon-task-runtime");
        let registry_store = DeviceRegistryStore::new(registry_path.clone());
        let admin_store = AdminConsoleStore::new(admin_path.clone(), registry_store);
        let conversation_store = TaskConversationStore::new(conversation_path.clone());
        let task_service = TaskApiService::new(admin_store.clone(), conversation_store.clone());
        let api = AdminApi::new(
            admin_store,
            task_service,
            PathBuf::from("frontend/harbor-assistant/dist/harbor-assistant"),
            "http://harborbeacon.local:4174".to_string(),
        );

        conversation_store
            .save_share_link_bundle(
                &MediaSession {
                    media_session_id: "media-session-active".to_string(),
                    device_id: "cam-1".to_string(),
                    stream_profile_id: "cam-1::stream::primary".to_string(),
                    session_kind: MediaSessionKind::Share,
                    delivery_mode: MediaDeliveryMode::Hls,
                    opened_by_user_id: Some("user-1".to_string()),
                    status: MediaSessionStatus::Active,
                    share_link_id: Some("share-link-active".to_string()),
                    started_at: Some(remote_view::now_unix_secs().to_string()),
                    ended_at: None,
                    metadata: json!({}),
                },
                &ShareLink {
                    share_link_id: "share-link-active".to_string(),
                    media_session_id: "media-session-active".to_string(),
                    token_hash: "hash-active".to_string(),
                    access_scope: ShareAccessScope::PublicLink,
                    expires_at: Some((remote_view::now_unix_secs() + 600).to_string()),
                    revoked_at: None,
                },
            )
            .expect("save active share link");
        conversation_store
            .save_share_link_bundle(
                &MediaSession {
                    media_session_id: "media-session-revoked".to_string(),
                    device_id: "cam-1".to_string(),
                    stream_profile_id: "cam-1::stream::primary".to_string(),
                    session_kind: MediaSessionKind::Share,
                    delivery_mode: MediaDeliveryMode::Hls,
                    opened_by_user_id: Some("user-1".to_string()),
                    status: MediaSessionStatus::Closed,
                    share_link_id: Some("share-link-revoked".to_string()),
                    started_at: Some((remote_view::now_unix_secs() - 300).to_string()),
                    ended_at: Some(remote_view::now_unix_secs().to_string()),
                    metadata: json!({}),
                },
                &ShareLink {
                    share_link_id: "share-link-revoked".to_string(),
                    media_session_id: "media-session-revoked".to_string(),
                    token_hash: "hash-revoked".to_string(),
                    access_scope: ShareAccessScope::PublicLink,
                    expires_at: Some((remote_view::now_unix_secs() + 600).to_string()),
                    revoked_at: Some(remote_view::now_unix_secs().to_string()),
                },
            )
            .expect("save revoked share link");
        conversation_store
            .save_share_link_bundle(
                &MediaSession {
                    media_session_id: "media-session-expired".to_string(),
                    device_id: "cam-2".to_string(),
                    stream_profile_id: "cam-2::stream::primary".to_string(),
                    session_kind: MediaSessionKind::Share,
                    delivery_mode: MediaDeliveryMode::Hls,
                    opened_by_user_id: Some("user-2".to_string()),
                    status: MediaSessionStatus::Active,
                    share_link_id: Some("share-link-expired".to_string()),
                    started_at: Some((remote_view::now_unix_secs() - 1200).to_string()),
                    ended_at: None,
                    metadata: json!({}),
                },
                &ShareLink {
                    share_link_id: "share-link-expired".to_string(),
                    media_session_id: "media-session-expired".to_string(),
                    token_hash: "hash-expired".to_string(),
                    access_scope: ShareAccessScope::PublicLink,
                    expires_at: Some((remote_view::now_unix_secs() - 30).to_string()),
                    revoked_at: None,
                },
            )
            .expect("save expired share link");

        let all_links = api.list_share_links(None).expect("list share links");
        assert_eq!(all_links.len(), 3);
        assert!(all_links
            .iter()
            .any(|link| link.share_link_id == "share-link-active" && link.status == "active"));
        assert!(all_links
            .iter()
            .any(|link| link.share_link_id == "share-link-revoked" && link.status == "revoked"));
        assert!(all_links
            .iter()
            .any(|link| link.share_link_id == "share-link-expired" && link.status == "expired"));

        let cam1_links = api.list_share_links(Some("cam-1")).expect("filter links");
        assert_eq!(cam1_links.len(), 2);
        assert!(cam1_links.iter().all(|link| link.device_id == "cam-1"));

        let _ = fs::remove_file(admin_path);
        let _ = fs::remove_file(registry_path);
        let _ = fs::remove_file(conversation_path);
    }
}
