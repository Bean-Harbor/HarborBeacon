//! Model-center helpers for admin redaction, endpoint tests, OCR routing, and
//! VLM summary execution.

use base64::Engine as _;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use reqwest::blocking::Client;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::connectors::ai_provider::{
    EmbeddingRequest, OpenAiCompatibleConfig, OpenAiCompatibleEmbeddingClient,
    OpenAiCompatibleTextClient, OpenAiCompatibleVisionClient, RerankCompatibleClient,
    RerankCompatibleConfig, RerankRequest, RerankScore, TextCompletionRequest,
    VisionSummaryRequest,
};
use crate::control_plane::models::{
    ModelEndpoint, ModelEndpointKind, ModelEndpointStatus, ModelKind, PrivacyLevel,
};
use crate::runtime::admin_console::{
    default_model_endpoints, sanitize_model_center_state, AdminConsoleState, AdminModelCenterState,
};

pub const ADMIN_STATE_PATH_ENV: &str = "HARBOR_ADMIN_STATE_PATH";
pub const OCR_TESSERACT_PATH_ENV: &str = "HARBOR_OCR_TESSERACT_PATH";
pub const OCR_TESSERACT_LANGS_ENV: &str = "HARBOR_OCR_LANGS";
const OCR_POLICY_ID: &str = "retrieval.ocr";
const EMBED_POLICY_ID: &str = "retrieval.embed";
pub const RERANK_POLICY_ID: &str = "retrieval.rerank";
const LLM_POLICY_ID: &str = "retrieval.answer";
const SEMANTIC_ROUTER_POLICY_ID: &str = "semantic.router";
const VLM_POLICY_ID: &str = "retrieval.vision_summary";
const DEFAULT_ADMIN_STATE_PATH: &str = ".harborbeacon/admin-console.json";
const DEFAULT_TESSERACT_LANGS: &str = "chi_sim+eng";
const SEMANTIC_ROUTER_BASE_URL_ENV: &str = "HARBOR_SEMANTIC_ROUTER_BASE_URL";
const SEMANTIC_ROUTER_HEALTHZ_URL_ENV: &str = "HARBOR_SEMANTIC_ROUTER_HEALTHZ_URL";
const SEMANTIC_ROUTER_TOKEN_ENV: &str = "HARBOR_SEMANTIC_ROUTER_TOKEN";
const MODEL_API_TOKEN_ENV: &str = "HARBOR_MODEL_API_TOKEN";
const DEFAULT_SEMANTIC_ROUTER_BASE_URL: &str = "http://127.0.0.1:4176/v1";
const DEFAULT_SEMANTIC_ROUTER_MODEL: &str = "Qwen/Qwen2.5-0.5B-Instruct";
static VLM_EXECUTION_BUSY: AtomicBool = AtomicBool::new(false);
static VLM_EXECUTION_STARTED_TOTAL: AtomicU64 = AtomicU64::new(0);
static VLM_EXECUTION_BUSY_TOTAL: AtomicU64 = AtomicU64::new(0);
static VLM_EXECUTION_COMPLETED_TOTAL: AtomicU64 = AtomicU64::new(0);
static VLM_EXECUTION_FAILED_TOTAL: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelEndpointTestResult {
    pub ok: bool,
    pub status: String,
    pub summary: String,
    pub endpoint: ModelEndpoint,
    #[serde(default)]
    pub details: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct OcrExecution {
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub provider_key: String,
    #[serde(default)]
    pub model_endpoint_id: Option<String>,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub details: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct VlmSummaryExecution {
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub provider_key: String,
    #[serde(default)]
    pub model_endpoint_id: Option<String>,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub details: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct LlmTextExecution {
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub provider_key: String,
    #[serde(default)]
    pub model_endpoint_id: Option<String>,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub details: Value,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct LlmTextOptions {
    pub purpose: Option<String>,
    pub system_prompt: Option<String>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub timeout: Option<Duration>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct EmbeddingExecution {
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub provider_key: String,
    #[serde(default)]
    pub model_endpoint_id: Option<String>,
    #[serde(default)]
    pub model_name: Option<String>,
    #[serde(default)]
    pub vector: Vec<f32>,
    #[serde(default)]
    pub details: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RerankExecution {
    #[serde(default)]
    pub available: bool,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub provider_key: String,
    #[serde(default)]
    pub model_endpoint_id: Option<String>,
    #[serde(default)]
    pub model_name: Option<String>,
    #[serde(default)]
    pub scores: Vec<RerankDocumentScore>,
    #[serde(default)]
    pub details: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RerankDocumentScore {
    pub index: usize,
    pub score: f32,
}

pub fn default_admin_state_path() -> PathBuf {
    std::env::var(ADMIN_STATE_PATH_ENV)
        .ok()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_ADMIN_STATE_PATH))
}

pub fn load_model_center_state() -> AdminModelCenterState {
    load_model_center_state_from_path(&default_admin_state_path())
}

pub fn load_model_center_state_from_path(path: &Path) -> AdminModelCenterState {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => return AdminModelCenterState::default(),
    };
    let state = match serde_json::from_str::<AdminConsoleState>(&text) {
        Ok(state) => state,
        Err(_) => return AdminModelCenterState::default(),
    };
    sanitize_model_center_state(state.models)
}

pub fn redact_model_center_state(state: &AdminModelCenterState) -> AdminModelCenterState {
    AdminModelCenterState {
        endpoints: state.endpoints.iter().map(redact_model_endpoint).collect(),
        route_policies: state.route_policies.clone(),
        model_store_root: state.model_store_root.clone(),
        capability_bindings: state.capability_bindings.clone(),
        runtimes: state.runtimes.clone(),
    }
}

pub fn redact_model_endpoint(endpoint: &ModelEndpoint) -> ModelEndpoint {
    let mut redacted = endpoint.clone();
    redact_secret_value(&mut redacted.metadata);
    redacted
}

pub fn test_model_endpoint(endpoint: &ModelEndpoint) -> ModelEndpointTestResult {
    if let Some(mock_text) = metadata_string(&endpoint.metadata, "mock_text") {
        return ModelEndpointTestResult {
            ok: !mock_text.trim().is_empty(),
            status: "active".to_string(),
            summary: "Mock model endpoint is configured for local tests.".to_string(),
            endpoint: redact_model_endpoint(endpoint),
            details: json!({
                "mock_text_length": mock_text.chars().count(),
            }),
        };
    }

    if endpoint.model_kind == ModelKind::Ocr
        && endpoint.provider_key.eq_ignore_ascii_case("tesseract")
    {
        return test_tesseract_endpoint(&endpoint);
    }

    test_http_endpoint(&endpoint)
}

pub fn run_ocr(image_path: &Path) -> OcrExecution {
    let state = load_model_center_state();
    run_ocr_with_state(image_path, &state)
}

pub fn run_ocr_with_state(image_path: &Path, state: &AdminModelCenterState) -> OcrExecution {
    let Some(endpoint) = resolve_endpoint(state, ModelKind::Ocr, OCR_POLICY_ID) else {
        return OcrExecution {
            available: false,
            status: "disabled".to_string(),
            summary: "No OCR endpoint is enabled.".to_string(),
            provider_key: String::new(),
            model_endpoint_id: None,
            text: String::new(),
            details: json!({}),
        };
    };

    if let Some(mock_text) = metadata_string(&endpoint.metadata, "mock_text") {
        return OcrExecution {
            available: !mock_text.trim().is_empty(),
            status: "active".to_string(),
            summary: "Mock OCR endpoint resolved.".to_string(),
            provider_key: endpoint.provider_key.clone(),
            model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
            text: mock_text,
            details: json!({
                "endpoint_kind": endpoint.endpoint_kind.as_str(),
            }),
        };
    }

    if !endpoint.provider_key.eq_ignore_ascii_case("tesseract") {
        return OcrExecution {
            available: false,
            status: "degraded".to_string(),
            summary: format!(
                "OCR endpoint {} is configured, but provider {} is not implemented yet.",
                endpoint.model_endpoint_id, endpoint.provider_key
            ),
            provider_key: endpoint.provider_key.clone(),
            model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
            text: String::new(),
            details: json!({
                "endpoint_kind": endpoint.endpoint_kind.as_str(),
                "local_only": true,
                "fallback_allowed": false,
            }),
        };
    }

    let Some(binary_path) = resolve_tesseract_binary(&endpoint) else {
        return OcrExecution {
            available: false,
            status: "degraded".to_string(),
            summary: "Tesseract is not available on this host.".to_string(),
            provider_key: endpoint.provider_key.clone(),
            model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
            text: String::new(),
            details: json!({
                "languages": resolve_tesseract_languages(&endpoint),
            }),
        };
    };

    let output = Command::new(&binary_path)
        .arg(image_path)
        .arg("stdout")
        .arg("-l")
        .arg(resolve_tesseract_languages(&endpoint))
        .arg("--psm")
        .arg("3")
        .output();

    match output {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if text.is_empty() {
                OcrExecution {
                    available: false,
                    status: "degraded".to_string(),
                    summary: "OCR completed, but no text was extracted.".to_string(),
                    provider_key: endpoint.provider_key.clone(),
                    model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
                    text,
                    details: json!({
                        "binary_path": binary_path.to_string_lossy(),
                    }),
                }
            } else {
                OcrExecution {
                    available: true,
                    status: "active".to_string(),
                    summary: "OCR text extracted from image.".to_string(),
                    provider_key: endpoint.provider_key.clone(),
                    model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
                    text,
                    details: json!({
                        "binary_path": binary_path.to_string_lossy(),
                        "languages": resolve_tesseract_languages(&endpoint),
                    }),
                }
            }
        }
        Ok(output) => OcrExecution {
            available: false,
            status: "degraded".to_string(),
            summary: "Tesseract command failed.".to_string(),
            provider_key: endpoint.provider_key.clone(),
            model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
            text: String::new(),
            details: json!({
                "binary_path": binary_path.to_string_lossy(),
                "stderr": String::from_utf8_lossy(&output.stderr).trim(),
            }),
        },
        Err(error) => OcrExecution {
            available: false,
            status: "degraded".to_string(),
            summary: format!("Failed to start tesseract: {error}"),
            provider_key: endpoint.provider_key.clone(),
            model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
            text: String::new(),
            details: json!({
                "binary_path": binary_path.to_string_lossy(),
            }),
        },
    }
}

pub fn run_vlm_summary(image_path: &Path) -> VlmSummaryExecution {
    let state = load_model_center_state();
    run_vlm_summary_with_state(image_path, &state)
}

pub fn run_vlm_summary_with_state(
    image_path: &Path,
    state: &AdminModelCenterState,
) -> VlmSummaryExecution {
    let Some(_guard) = VlmExecutionGuard::try_acquire() else {
        return VlmSummaryExecution {
            available: false,
            status: "busy".to_string(),
            summary: "VLM queue is busy; retry after the current local request completes."
                .to_string(),
            provider_key: String::new(),
            model_endpoint_id: None,
            text: String::new(),
            details: json!({
                "queue_result": "busy",
                "local_only": true,
                "fallback_allowed": false,
            }),
        };
    };
    let Some(endpoint) = resolve_endpoint(state, ModelKind::Vlm, VLM_POLICY_ID) else {
        return VlmSummaryExecution {
            available: false,
            status: "disabled".to_string(),
            summary: "No VLM endpoint is enabled.".to_string(),
            provider_key: String::new(),
            model_endpoint_id: None,
            text: String::new(),
            details: json!({}),
        };
    };

    if let Some(reason) = vlm_endpoint_local_only_blocker(&endpoint) {
        return VlmSummaryExecution {
            available: false,
            status: "blocked".to_string(),
            summary: reason,
            provider_key: endpoint.provider_key.clone(),
            model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
            text: String::new(),
            details: json!({
                "endpoint_kind": endpoint.endpoint_kind.as_str(),
                "local_only": false,
                "fallback_allowed": false,
            }),
        };
    }

    if let Some(mock_text) = metadata_string(&endpoint.metadata, "mock_text") {
        return VlmSummaryExecution {
            available: !mock_text.trim().is_empty(),
            status: "active".to_string(),
            summary: "Mock VLM endpoint resolved.".to_string(),
            provider_key: endpoint.provider_key.clone(),
            model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
            text: mock_text,
            details: json!({
                "endpoint_kind": endpoint.endpoint_kind.as_str(),
                "local_only": true,
                "fallback_allowed": false,
            }),
        };
    }

    if !endpoint
        .provider_key
        .eq_ignore_ascii_case("openai_compatible")
    {
        return VlmSummaryExecution {
            available: false,
            status: "degraded".to_string(),
            summary: format!(
                "VLM endpoint {} is configured, but provider {} is not implemented yet.",
                endpoint.model_endpoint_id, endpoint.provider_key
            ),
            provider_key: endpoint.provider_key.clone(),
            model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
            text: String::new(),
            details: json!({
                "endpoint_kind": endpoint.endpoint_kind.as_str(),
            }),
        };
    }

    let Some(config) = openai_compatible_config_from_endpoint(&endpoint) else {
        return VlmSummaryExecution {
            available: false,
            status: "degraded".to_string(),
            summary: "VLM endpoint base_url / api_key / model_name are not configured.".to_string(),
            provider_key: endpoint.provider_key.clone(),
            model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
            text: String::new(),
            details: json!({
                "endpoint_kind": endpoint.endpoint_kind.as_str(),
                "local_only": true,
                "fallback_allowed": false,
            }),
        };
    };

    let image_data_url = match build_image_data_url(image_path) {
        Ok(value) => value,
        Err(error) => {
            return VlmSummaryExecution {
                available: false,
                status: "degraded".to_string(),
                summary: error,
                provider_key: endpoint.provider_key.clone(),
                model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
                text: String::new(),
                details: json!({
                    "image_path_redacted": true,
                    "local_only": true,
                    "fallback_allowed": false,
                }),
            };
        }
    };

    let prompt = metadata_string(&endpoint.metadata, "prompt").or_else(|| {
        Some(
            "请用中文概括这张图片、截图或摄像头静帧的主要内容，提取主体、场景、可检索文本线索和需要关注的信号，保持在 80 个汉字以内。"
                .to_string(),
        )
    });

    let client = match OpenAiCompatibleVisionClient::new(config) {
        Ok(client) => client,
        Err(error) => {
            return VlmSummaryExecution {
                available: false,
                status: "degraded".to_string(),
                summary: format!("Failed to build VLM client: {error}"),
                provider_key: endpoint.provider_key.clone(),
                model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
                text: String::new(),
                details: json!({
                    "image_path_redacted": true,
                    "local_only": true,
                    "fallback_allowed": false,
                }),
            };
        }
    };

    match client.describe_frame(&VisionSummaryRequest {
        image_data_url,
        detection_summary: "No detector summary is attached for retrieval-side still images."
            .to_string(),
        user_prompt: prompt,
    }) {
        Ok(response) => VlmSummaryExecution {
            available: true,
            status: "active".to_string(),
            summary: "VLM summary extracted from image.".to_string(),
            provider_key: endpoint.provider_key.clone(),
            model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
            text: response.summary,
            details: json!({
                "raw_response": response.raw_response,
                "local_only": true,
                "fallback_allowed": false,
            }),
        },
        Err(error) => VlmSummaryExecution {
            available: false,
            status: "degraded".to_string(),
            summary: format!("VLM request failed: {error}"),
            provider_key: endpoint.provider_key.clone(),
            model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
            text: String::new(),
            details: json!({
                "image_path_redacted": true,
                "local_only": true,
                "fallback_allowed": false,
            }),
        },
    }
}

pub fn vlm_execution_runtime_snapshot() -> Value {
    json!({
        "busy": VLM_EXECUTION_BUSY.load(Ordering::Relaxed),
        "mode": "global_serial_try_lock",
        "started_total": VLM_EXECUTION_STARTED_TOTAL.load(Ordering::Relaxed),
        "busy_total": VLM_EXECUTION_BUSY_TOTAL.load(Ordering::Relaxed),
        "completed_total": VLM_EXECUTION_COMPLETED_TOTAL.load(Ordering::Relaxed),
        "failed_total": VLM_EXECUTION_FAILED_TOTAL.load(Ordering::Relaxed),
        "metadata_only": true,
        "secret_scan": "clean",
    })
}

pub fn vlm_endpoint_readiness(state: &AdminModelCenterState) -> Value {
    let endpoint = resolve_endpoint(state, ModelKind::Vlm, VLM_POLICY_ID);
    let Some(endpoint) = endpoint.as_ref() else {
        return json!({
            "status": "not_configured",
            "endpoint_ready": false,
            "local_only": true,
            "fallback_allowed": false,
            "endpoint_bound": false,
            "queue": vlm_execution_runtime_snapshot(),
            "metadata_only": true,
            "secret_scan": "clean",
        });
    };
    let blocker = vlm_endpoint_local_only_blocker(endpoint);
    let configured = endpoint.status != ModelEndpointStatus::Disabled;
    let endpoint_ready = configured && blocker.is_none();
    json!({
        "status": if endpoint_ready {
            "available"
        } else if configured {
            "blocked"
        } else {
            "not_configured"
        },
        "endpoint_ready": endpoint_ready,
        "local_only": blocker.is_none(),
        "fallback_allowed": false,
        "endpoint_bound": true,
        "endpoint": {
            "model_endpoint_id": endpoint.model_endpoint_id,
            "provider_key": endpoint.provider_key,
            "endpoint_kind": endpoint.endpoint_kind.as_str(),
            "status": endpoint.status.as_str(),
            "model_name": endpoint.model_name,
            "base_url_redacted": metadata_string(&endpoint.metadata, "base_url").is_some(),
        },
        "blocker": blocker,
        "queue": vlm_execution_runtime_snapshot(),
        "metadata_only": true,
        "secret_scan": "clean",
    })
}

struct VlmExecutionGuard {
    completed: bool,
}

impl VlmExecutionGuard {
    fn try_acquire() -> Option<Self> {
        match VLM_EXECUTION_BUSY.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        {
            Ok(_) => {
                VLM_EXECUTION_STARTED_TOTAL.fetch_add(1, Ordering::Relaxed);
                Some(Self { completed: false })
            }
            Err(_) => {
                VLM_EXECUTION_BUSY_TOTAL.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }
}

impl Drop for VlmExecutionGuard {
    fn drop(&mut self) {
        if self.completed {
            VLM_EXECUTION_COMPLETED_TOTAL.fetch_add(1, Ordering::Relaxed);
        } else if !std::thread::panicking() {
            VLM_EXECUTION_COMPLETED_TOTAL.fetch_add(1, Ordering::Relaxed);
        } else {
            VLM_EXECUTION_FAILED_TOTAL.fetch_add(1, Ordering::Relaxed);
        }
        VLM_EXECUTION_BUSY.store(false, Ordering::Release);
    }
}

fn vlm_endpoint_local_only_blocker(endpoint: &ModelEndpoint) -> Option<String> {
    if endpoint.endpoint_kind != ModelEndpointKind::Local {
        return Some(
            "VLM endpoint must be local-only; cloud or remote fallback is blocked.".to_string(),
        );
    }
    let Some(base_url) = metadata_string(&endpoint.metadata, "base_url") else {
        return None;
    };
    if !endpoint
        .provider_key
        .eq_ignore_ascii_case("openai_compatible")
    {
        return None;
    }
    if !url_is_loopback(&base_url) {
        return Some(
            "VLM endpoint base_url must bind to loopback for K3 local-only execution.".to_string(),
        );
    }
    None
}

fn url_is_loopback(raw_url: &str) -> bool {
    let Ok(url) = Url::parse(raw_url.trim()) else {
        return false;
    };
    match url
        .host_str()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "localhost" => true,
        "::1" => true,
        value if value.starts_with("127.") => true,
        _ => false,
    }
}

pub fn run_llm_text(prompt: &str) -> LlmTextExecution {
    let state = load_model_center_state();
    run_llm_text_with_state(prompt, &state)
}

pub fn run_embedding(text: &str) -> EmbeddingExecution {
    let state = load_model_center_state();
    run_embedding_with_state(text, &state)
}

pub fn run_rerank(query: &str, documents: &[String], top_n: usize) -> RerankExecution {
    let state = load_model_center_state();
    run_rerank_with_state(query, documents, top_n, &state)
}

pub fn run_rerank_with_state(
    query: &str,
    documents: &[String],
    top_n: usize,
    state: &AdminModelCenterState,
) -> RerankExecution {
    let query = query.trim();
    if query.is_empty() {
        return RerankExecution {
            available: false,
            status: "disabled".to_string(),
            summary: "Rerank query is empty.".to_string(),
            provider_key: String::new(),
            model_endpoint_id: None,
            model_name: None,
            scores: Vec::new(),
            details: json!({}),
        };
    }
    let documents = documents
        .iter()
        .map(|document| document.trim().to_string())
        .filter(|document| !document.is_empty())
        .collect::<Vec<_>>();
    if documents.is_empty() {
        return RerankExecution {
            available: false,
            status: "disabled".to_string(),
            summary: "Rerank documents are empty.".to_string(),
            provider_key: String::new(),
            model_endpoint_id: None,
            model_name: None,
            scores: Vec::new(),
            details: json!({}),
        };
    }

    let Some(endpoint) = resolve_endpoint(state, ModelKind::Reranker, RERANK_POLICY_ID) else {
        return RerankExecution {
            available: false,
            status: "disabled".to_string(),
            summary: "No local rerank endpoint is enabled.".to_string(),
            provider_key: String::new(),
            model_endpoint_id: None,
            model_name: None,
            scores: Vec::new(),
            details: json!({
                "route_policy_id": RERANK_POLICY_ID,
                "local_only": true,
                "fallback_allowed": false,
            }),
        };
    };

    if endpoint.endpoint_kind == ModelEndpointKind::Cloud {
        return RerankExecution {
            available: false,
            status: "blocked".to_string(),
            summary: "Cloud reranker endpoints are not allowed for retrieval.rerank.".to_string(),
            provider_key: endpoint.provider_key.clone(),
            model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
            model_name: Some(endpoint.model_name.clone()),
            scores: Vec::new(),
            details: json!({
                "route_policy_id": RERANK_POLICY_ID,
                "endpoint_kind": endpoint.endpoint_kind.as_str(),
                "local_only": true,
                "fallback_allowed": false,
            }),
        };
    }

    if let Some(scores) = mock_rerank_scores_from_endpoint(&endpoint, documents.len()) {
        return RerankExecution {
            available: !scores.is_empty(),
            status: "active".to_string(),
            summary: "Mock rerank endpoint resolved.".to_string(),
            provider_key: endpoint.provider_key.clone(),
            model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
            model_name: Some(endpoint.model_name.clone()),
            scores,
            details: json!({
                "route_policy_id": RERANK_POLICY_ID,
                "endpoint_kind": endpoint.endpoint_kind.as_str(),
                "local_only": true,
                "fallback_allowed": false,
            }),
        };
    }

    if !endpoint
        .provider_key
        .eq_ignore_ascii_case("rerank_compatible")
    {
        return RerankExecution {
            available: false,
            status: "degraded".to_string(),
            summary: format!(
                "Rerank endpoint {} is configured, but provider {} is not implemented yet.",
                endpoint.model_endpoint_id, endpoint.provider_key
            ),
            provider_key: endpoint.provider_key.clone(),
            model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
            model_name: Some(endpoint.model_name.clone()),
            scores: Vec::new(),
            details: json!({
                "route_policy_id": RERANK_POLICY_ID,
                "endpoint_kind": endpoint.endpoint_kind.as_str(),
                "local_only": true,
                "fallback_allowed": false,
            }),
        };
    }

    let Some(config) = rerank_compatible_config_from_endpoint(&endpoint) else {
        return RerankExecution {
            available: false,
            status: "degraded".to_string(),
            summary: "Rerank endpoint base_url / model_name are not configured.".to_string(),
            provider_key: endpoint.provider_key.clone(),
            model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
            model_name: Some(endpoint.model_name.clone()),
            scores: Vec::new(),
            details: json!({
                "route_policy_id": RERANK_POLICY_ID,
                "endpoint_kind": endpoint.endpoint_kind.as_str(),
                "local_only": true,
                "fallback_allowed": false,
            }),
        };
    };

    let client = match RerankCompatibleClient::new(config) {
        Ok(client) => client,
        Err(error) => {
            return RerankExecution {
                available: false,
                status: "degraded".to_string(),
                summary: format!("Failed to build rerank client: {error}"),
                provider_key: endpoint.provider_key.clone(),
                model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
                model_name: Some(endpoint.model_name.clone()),
                scores: Vec::new(),
                details: json!({
                    "route_policy_id": RERANK_POLICY_ID,
                    "local_only": true,
                    "fallback_allowed": false,
                }),
            };
        }
    };

    let document_count = documents.len();
    match client.rerank(&RerankRequest {
        query: query.to_string(),
        documents,
        top_n: top_n.max(1),
    }) {
        Ok(response) => {
            let scores = response
                .scores
                .into_iter()
                .filter(|score| score.index < document_count)
                .map(rerank_score_to_document_score)
                .collect::<Vec<_>>();
            RerankExecution {
                available: !scores.is_empty(),
                status: "active".to_string(),
                summary: "Rerank request completed.".to_string(),
                provider_key: endpoint.provider_key.clone(),
                model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
                model_name: Some(endpoint.model_name.clone()),
                scores,
                details: json!({
                    "route_policy_id": RERANK_POLICY_ID,
                    "raw_response": response.raw_response,
                    "local_only": true,
                    "fallback_allowed": false,
                }),
            }
        }
        Err(error) => RerankExecution {
            available: false,
            status: "degraded".to_string(),
            summary: format!("Rerank request failed: {error}"),
            provider_key: endpoint.provider_key.clone(),
            model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
            model_name: Some(endpoint.model_name.clone()),
            scores: Vec::new(),
            details: json!({
                "route_policy_id": RERANK_POLICY_ID,
                "local_only": true,
                "fallback_allowed": false,
            }),
        },
    }
}

pub fn run_llm_text_with_state(prompt: &str, state: &AdminModelCenterState) -> LlmTextExecution {
    run_llm_text_with_state_and_options(prompt, state, &LlmTextOptions::default())
}

pub fn run_llm_text_with_state_and_options(
    prompt: &str,
    state: &AdminModelCenterState,
    options: &LlmTextOptions,
) -> LlmTextExecution {
    let route_policy_id = llm_route_policy_id(options);
    let local_only_state;
    let effective_state = if route_policy_id == SEMANTIC_ROUTER_POLICY_ID {
        local_only_state = semantic_router_local_only_model_state(state);
        &local_only_state
    } else {
        state
    };
    let candidates = resolve_endpoint_candidates(effective_state, ModelKind::Llm, route_policy_id);
    if candidates.is_empty() {
        return LlmTextExecution {
            available: false,
            status: "disabled".to_string(),
            summary: "No LLM endpoint is enabled.".to_string(),
            provider_key: String::new(),
            model_endpoint_id: None,
            text: String::new(),
            details: json!({
                "route_policy_id": route_policy_id,
                "attempted_endpoints": [],
            }),
        };
    };

    let mut attempted_endpoints = Vec::new();
    let mut attempt_summaries = Vec::new();
    let mut fallback_reason = None;
    let first_endpoint_id = candidates
        .first()
        .map(|endpoint| endpoint.model_endpoint_id.clone());
    let mut last_result = None;

    for endpoint in candidates {
        attempted_endpoints.push(endpoint.model_endpoint_id.clone());
        let mut result = run_llm_text_on_endpoint(prompt, &endpoint, options);
        attempt_summaries.push(json!({
            "endpoint": endpoint.model_endpoint_id,
            "endpoint_kind": endpoint.endpoint_kind.as_str(),
            "status": result.status,
            "available": result.available,
            "summary": result.summary,
        }));
        if result.available {
            let selected_endpoint_id = result.model_endpoint_id.clone();
            let selected_endpoint_kind = endpoint.endpoint_kind.as_str();
            let fallback_used = selected_endpoint_id.as_ref() != first_endpoint_id.as_ref()
                || attempted_endpoints.len() > 1;
            merge_llm_execution_details(
                &mut result,
                route_policy_id,
                &attempted_endpoints,
                fallback_reason.as_deref(),
                fallback_used,
                selected_endpoint_kind,
                attempt_summaries,
            );
            return result;
        }
        if fallback_reason.is_none() {
            fallback_reason = Some(result.summary.clone());
        }
        last_result = Some(result);
    }

    let mut result = last_result.unwrap_or_default();
    result.available = false;
    result.status = if result.status.trim().is_empty() {
        "degraded".to_string()
    } else {
        result.status
    };
    result.summary = format!(
        "All LLM endpoints failed for route_policy={route_policy_id}; last error: {}",
        result.summary
    );
    merge_llm_execution_details(
        &mut result,
        route_policy_id,
        &attempted_endpoints,
        fallback_reason.as_deref(),
        attempted_endpoints.len() > 1,
        "",
        attempt_summaries,
    );
    result
}

fn llm_route_policy_id(options: &LlmTextOptions) -> &'static str {
    match options.purpose.as_deref().map(str::trim) {
        Some("router") | Some("semantic.router") => SEMANTIC_ROUTER_POLICY_ID,
        _ => LLM_POLICY_ID,
    }
}

fn merge_llm_execution_details(
    result: &mut LlmTextExecution,
    route_policy_id: &str,
    attempted_endpoints: &[String],
    fallback_reason: Option<&str>,
    fallback_used: bool,
    selected_endpoint_kind: &str,
    attempt_summaries: Vec<Value>,
) {
    let mut details = match result.details.clone() {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    details.insert("route_policy_id".to_string(), json!(route_policy_id));
    details.insert(
        "attempted_endpoints".to_string(),
        json!(attempted_endpoints),
    );
    details.insert("fallback_used".to_string(), json!(fallback_used));
    if route_policy_id == SEMANTIC_ROUTER_POLICY_ID {
        details.insert("local_only".to_string(), json!(true));
    }
    details.insert(
        "attempt_summaries".to_string(),
        Value::Array(attempt_summaries),
    );
    if let Some(reason) = fallback_reason.filter(|value| !value.trim().is_empty()) {
        details.insert("fallback_reason".to_string(), json!(reason));
    }
    if let Some(endpoint_id) = result.model_endpoint_id.as_ref() {
        details.insert("selected_endpoint".to_string(), json!(endpoint_id));
    }
    if !selected_endpoint_kind.trim().is_empty() {
        details.insert(
            "selected_endpoint_kind".to_string(),
            json!(selected_endpoint_kind),
        );
    }
    result.details = Value::Object(details);
}

fn semantic_router_local_only_model_state(state: &AdminModelCenterState) -> AdminModelCenterState {
    let mut local_state = state.clone();
    local_state
        .endpoints
        .retain(|endpoint| endpoint.endpoint_kind != ModelEndpointKind::Cloud);
    wire_semantic_router_resident_endpoint(&mut local_state);
    for policy in &mut local_state.route_policies {
        if policy.route_policy_id == SEMANTIC_ROUTER_POLICY_ID {
            policy.privacy_level = PrivacyLevel::StrictLocal;
            policy
                .fallback_order
                .retain(|kind| !kind.eq_ignore_ascii_case("cloud"));
            if policy.fallback_order.is_empty() {
                policy.fallback_order = vec!["local".to_string(), "sidecar".to_string()];
            }
            if let Some(metadata) = policy.metadata.as_object_mut() {
                metadata.insert("local_only".to_string(), json!(true));
                metadata.insert("cloud_fallback_allowed".to_string(), json!(false));
            }
        }
    }
    local_state
}

pub(crate) fn wire_semantic_router_resident_endpoint(state: &mut AdminModelCenterState) {
    let base_url = env_trimmed(SEMANTIC_ROUTER_BASE_URL_ENV)
        .map(|value| value.trim_end_matches('/').to_string())
        .unwrap_or_else(|| DEFAULT_SEMANTIC_ROUTER_BASE_URL.to_string());
    let healthz_url = env_trimmed(SEMANTIC_ROUTER_HEALTHZ_URL_ENV)
        .unwrap_or_else(|| infer_healthz_url(&base_url));
    let api_key = env_trimmed(SEMANTIC_ROUTER_TOKEN_ENV)
        .or_else(|| env_trimmed(MODEL_API_TOKEN_ENV))
        .unwrap_or_default();

    let has_explicit_router = state.endpoints.iter().any(|endpoint| {
        endpoint.model_kind == ModelKind::Llm
            && endpoint.endpoint_kind == ModelEndpointKind::Local
            && endpoint.status != ModelEndpointStatus::Disabled
            && (endpoint
                .capability_tags
                .iter()
                .any(|tag| matches_semantic_router_tag(tag))
                || metadata_bool(&endpoint.metadata, "semantic_router"))
    });

    let mut wired_any = false;
    for endpoint in state.endpoints.iter_mut() {
        if endpoint.model_kind != ModelKind::Llm
            || endpoint.endpoint_kind != ModelEndpointKind::Local
            || endpoint.status == ModelEndpointStatus::Disabled
            || !endpoint
                .provider_key
                .eq_ignore_ascii_case("openai_compatible")
        {
            continue;
        }
        let is_explicit_router = endpoint
            .capability_tags
            .iter()
            .any(|tag| matches_semantic_router_tag(tag))
            || metadata_bool(&endpoint.metadata, "semantic_router");
        let is_builtin_default = endpoint.model_endpoint_id == "llm-local-openai-compatible";
        if !is_explicit_router && !(is_builtin_default && !has_explicit_router) {
            continue;
        }
        mark_semantic_router_resident_endpoint(endpoint, &base_url, &healthz_url, &api_key);
        wired_any = true;
    }

    if wired_any {
        return;
    }

    let mut endpoint = ModelEndpoint {
        model_endpoint_id: "llm-local-semantic-router".to_string(),
        workspace_id: Some("home-1".to_string()),
        provider_account_id: None,
        model_kind: ModelKind::Llm,
        endpoint_kind: ModelEndpointKind::Local,
        provider_key: "openai_compatible".to_string(),
        model_name: DEFAULT_SEMANTIC_ROUTER_MODEL.to_string(),
        capability_tags: Vec::new(),
        cost_policy: json!({"cost_hint": "local_candle"}),
        status: ModelEndpointStatus::Degraded,
        metadata: json!({"builtin": true}),
    };
    mark_semantic_router_resident_endpoint(&mut endpoint, &base_url, &healthz_url, &api_key);
    state.endpoints.push(endpoint);
}

fn mark_semantic_router_resident_endpoint(
    endpoint: &mut ModelEndpoint,
    base_url: &str,
    healthz_url: &str,
    api_key: &str,
) {
    for tag in [
        "chat",
        "local_first",
        "semantic_router",
        "assistant_input_parser",
        "k3_nsp",
    ] {
        if !endpoint.capability_tags.iter().any(|value| value == tag) {
            endpoint.capability_tags.push(tag.to_string());
        }
    }
    endpoint.capability_tags.sort();
    endpoint.capability_tags.dedup();
    set_metadata_string(&mut endpoint.metadata, "base_url", base_url.to_string());
    set_metadata_string(
        &mut endpoint.metadata,
        "healthz_url",
        healthz_url.to_string(),
    );
    set_metadata_bool(&mut endpoint.metadata, "semantic_router", true);
    set_metadata_bool(&mut endpoint.metadata, "local_only", true);
    set_metadata_bool(&mut endpoint.metadata, "cloud_fallback_allowed", false);
    set_metadata_bool(
        &mut endpoint.metadata,
        "semantic_router_resident_endpoint",
        true,
    );
    if !api_key.trim().is_empty() {
        set_metadata_string(&mut endpoint.metadata, "api_key", api_key.to_string());
        set_metadata_bool(&mut endpoint.metadata, "api_key_configured", true);
    }
}

fn run_llm_text_on_endpoint(
    prompt: &str,
    endpoint: &ModelEndpoint,
    options: &LlmTextOptions,
) -> LlmTextExecution {
    if let Some(mock_text) = metadata_string(&endpoint.metadata, "mock_text") {
        return LlmTextExecution {
            available: !mock_text.trim().is_empty(),
            status: "active".to_string(),
            summary: "Mock LLM endpoint resolved.".to_string(),
            provider_key: endpoint.provider_key.clone(),
            model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
            text: mock_text,
            details: json!({
                "endpoint_kind": endpoint.endpoint_kind.as_str(),
            }),
        };
    }

    if !endpoint
        .provider_key
        .eq_ignore_ascii_case("openai_compatible")
    {
        return LlmTextExecution {
            available: false,
            status: "degraded".to_string(),
            summary: format!(
                "LLM endpoint {} is configured, but provider {} is not implemented yet.",
                endpoint.model_endpoint_id, endpoint.provider_key
            ),
            provider_key: endpoint.provider_key.clone(),
            model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
            text: String::new(),
            details: json!({
                "endpoint_kind": endpoint.endpoint_kind.as_str(),
            }),
        };
    }

    let Some(config) = openai_compatible_config_from_endpoint(&endpoint) else {
        return LlmTextExecution {
            available: false,
            status: "degraded".to_string(),
            summary: "LLM endpoint base_url / api_key / model_name are not configured.".to_string(),
            provider_key: endpoint.provider_key.clone(),
            model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
            text: String::new(),
            details: json!({
                "endpoint_kind": endpoint.endpoint_kind.as_str(),
            }),
        };
    };

    let system_prompt = options.system_prompt.clone().or_else(|| {
        metadata_string(&endpoint.metadata, "system_prompt").or_else(|| {
            Some(
                "You are a strict HarborBeacon planning translator. Return only valid JSON that follows the requested schema."
                    .to_string(),
            )
        })
    });

    let client = match OpenAiCompatibleTextClient::new(config) {
        Ok(client) => client,
        Err(error) => {
            return LlmTextExecution {
                available: false,
                status: "degraded".to_string(),
                summary: format!("Failed to build LLM client: {error}"),
                provider_key: endpoint.provider_key.clone(),
                model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
                text: String::new(),
                details: json!({}),
            };
        }
    };

    match client.complete_text(&TextCompletionRequest {
        system_prompt,
        user_prompt: prompt.to_string(),
        temperature: options.temperature.or(Some(0.1)),
        max_tokens: options.max_tokens,
        timeout: options.timeout,
    }) {
        Ok(response) => LlmTextExecution {
            available: true,
            status: "active".to_string(),
            summary: format!(
                "LLM {} completed.",
                options
                    .purpose
                    .as_deref()
                    .filter(|value| !value.trim().is_empty())
                    .unwrap_or("text completion")
            ),
            provider_key: endpoint.provider_key.clone(),
            model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
            text: response.text,
            details: json!({
                "purpose": options.purpose.clone(),
                "max_tokens": options.max_tokens,
                "timeout_ms": options.timeout.map(|value| value.as_millis() as u64),
                "raw_response": response.raw_response,
            }),
        },
        Err(error) => LlmTextExecution {
            available: false,
            status: "degraded".to_string(),
            summary: format!("LLM request failed: {error}"),
            provider_key: endpoint.provider_key.clone(),
            model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
            text: String::new(),
            details: json!({}),
        },
    }
}

pub fn run_embedding_with_state(text: &str, state: &AdminModelCenterState) -> EmbeddingExecution {
    let input = text.trim();
    if input.is_empty() {
        return EmbeddingExecution {
            available: false,
            status: "disabled".to_string(),
            summary: "Embedding input is empty.".to_string(),
            provider_key: String::new(),
            model_endpoint_id: None,
            model_name: None,
            vector: Vec::new(),
            details: json!({}),
        };
    }

    let Some(endpoint) = resolve_endpoint(state, ModelKind::Embedder, EMBED_POLICY_ID) else {
        return EmbeddingExecution {
            available: false,
            status: "disabled".to_string(),
            summary: "No embedding endpoint is enabled.".to_string(),
            provider_key: String::new(),
            model_endpoint_id: None,
            model_name: None,
            vector: Vec::new(),
            details: json!({}),
        };
    };

    if let Some(vector) = mock_embedding_vector_from_endpoint(&endpoint, input) {
        return EmbeddingExecution {
            available: !vector.is_empty(),
            status: "active".to_string(),
            summary: "Mock embedding endpoint resolved.".to_string(),
            provider_key: endpoint.provider_key.clone(),
            model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
            model_name: Some(endpoint.model_name.clone()),
            vector,
            details: json!({
                "endpoint_kind": endpoint.endpoint_kind.as_str(),
            }),
        };
    }

    if !endpoint
        .provider_key
        .eq_ignore_ascii_case("openai_compatible")
    {
        return EmbeddingExecution {
            available: false,
            status: "degraded".to_string(),
            summary: format!(
                "Embedding endpoint {} is configured, but provider {} is not implemented yet.",
                endpoint.model_endpoint_id, endpoint.provider_key
            ),
            provider_key: endpoint.provider_key.clone(),
            model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
            model_name: Some(endpoint.model_name.clone()),
            vector: Vec::new(),
            details: json!({
                "endpoint_kind": endpoint.endpoint_kind.as_str(),
            }),
        };
    }

    let Some(config) = openai_compatible_config_from_endpoint(&endpoint) else {
        return EmbeddingExecution {
            available: false,
            status: "degraded".to_string(),
            summary: "Embedding endpoint base_url / api_key / model_name are not configured."
                .to_string(),
            provider_key: endpoint.provider_key.clone(),
            model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
            model_name: Some(endpoint.model_name.clone()),
            vector: Vec::new(),
            details: json!({
                "endpoint_kind": endpoint.endpoint_kind.as_str(),
            }),
        };
    };

    let client = match OpenAiCompatibleEmbeddingClient::new(config) {
        Ok(client) => client,
        Err(error) => {
            return EmbeddingExecution {
                available: false,
                status: "degraded".to_string(),
                summary: format!("Failed to build embedding client: {error}"),
                provider_key: endpoint.provider_key.clone(),
                model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
                model_name: Some(endpoint.model_name.clone()),
                vector: Vec::new(),
                details: json!({}),
            };
        }
    };

    match client.embed_text(&EmbeddingRequest {
        input: input.to_string(),
    }) {
        Ok(response) => EmbeddingExecution {
            available: !response.embedding.is_empty(),
            status: "active".to_string(),
            summary: "Embedding request completed.".to_string(),
            provider_key: endpoint.provider_key.clone(),
            model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
            model_name: Some(endpoint.model_name.clone()),
            vector: response.embedding,
            details: json!({
                "raw_response": response.raw_response,
            }),
        },
        Err(error) => EmbeddingExecution {
            available: false,
            status: "degraded".to_string(),
            summary: format!("Embedding request failed: {error}"),
            provider_key: endpoint.provider_key.clone(),
            model_endpoint_id: Some(endpoint.model_endpoint_id.clone()),
            model_name: Some(endpoint.model_name.clone()),
            vector: Vec::new(),
            details: json!({}),
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct LocalRuntimeProjection {
    base_url: String,
    healthz_url: String,
    api_key: String,
    api_key_configured: bool,
    ready: bool,
    backend_ready: bool,
    chat_model: Option<String>,
    embedding_model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct LocalRuntimeProbeTarget {
    cache_key: String,
    base_url: String,
    healthz_url: String,
    api_key: String,
    api_key_configured: bool,
}

#[derive(Debug, Clone)]
struct CachedLocalRuntimeProjection {
    target_cache_key: String,
    expires_at: Instant,
    projection: LocalRuntimeProjection,
}

const LOCAL_RUNTIME_PROJECTION_CACHE_TTL: Duration = Duration::from_secs(30);

fn local_runtime_projection_cache() -> &'static Mutex<Option<CachedLocalRuntimeProjection>> {
    static CACHE: OnceLock<Mutex<Option<CachedLocalRuntimeProjection>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

#[cfg(test)]
fn clear_local_runtime_projection_cache() {
    *local_runtime_projection_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

fn runtime_wired_model_center_state(state: &AdminModelCenterState) -> AdminModelCenterState {
    let runtime = probe_local_runtime(&state.endpoints);
    AdminModelCenterState {
        endpoints: overlay_endpoints_with_runtime_truth(&state.endpoints, &runtime),
        route_policies: state.route_policies.clone(),
        model_store_root: state.model_store_root.clone(),
        capability_bindings: state.capability_bindings.clone(),
        runtimes: state.runtimes.clone(),
    }
}

fn resolve_endpoint(
    state: &AdminModelCenterState,
    model_kind: ModelKind,
    route_policy_id: &str,
) -> Option<ModelEndpoint> {
    resolve_endpoint_candidates(state, model_kind, route_policy_id)
        .into_iter()
        .next()
}

fn resolve_endpoint_candidates(
    state: &AdminModelCenterState,
    model_kind: ModelKind,
    route_policy_id: &str,
) -> Vec<ModelEndpoint> {
    let state = runtime_wired_model_center_state(state);
    let policy = state
        .route_policies
        .iter()
        .find(|policy| policy.route_policy_id == route_policy_id);
    let fallback_order = policy
        .map(|policy| policy.fallback_order.clone())
        .unwrap_or_else(|| {
            vec![
                "local".to_string(),
                "sidecar".to_string(),
                "cloud".to_string(),
            ]
        });
    let cloud_allowed = policy
        .map(|policy| policy.privacy_level != PrivacyLevel::StrictLocal)
        .unwrap_or(true);

    let mut candidates = state
        .endpoints
        .iter()
        .filter(|endpoint| {
            endpoint.model_kind == model_kind && endpoint.status != ModelEndpointStatus::Disabled
        })
        .filter(|endpoint| cloud_allowed || endpoint.endpoint_kind != ModelEndpointKind::Cloud)
        .cloned()
        .collect::<Vec<_>>();

    candidates.sort_by(|left, right| {
        semantic_router_endpoint_priority(left, route_policy_id)
            .cmp(&semantic_router_endpoint_priority(right, route_policy_id))
            .then(
                endpoint_priority(left, &fallback_order)
                    .cmp(&endpoint_priority(right, &fallback_order)),
            )
            .then(status_priority(left.status).cmp(&status_priority(right.status)))
            .then(left.model_endpoint_id.cmp(&right.model_endpoint_id))
    });

    candidates
}

fn semantic_router_endpoint_priority(endpoint: &ModelEndpoint, route_policy_id: &str) -> usize {
    if route_policy_id != SEMANTIC_ROUTER_POLICY_ID {
        return 0;
    }
    if endpoint
        .capability_tags
        .iter()
        .any(|tag| matches_semantic_router_tag(tag))
        || metadata_bool(&endpoint.metadata, "semantic_router")
        || metadata_bool(&endpoint.metadata, "local_only")
            && endpoint
                .model_endpoint_id
                .to_ascii_lowercase()
                .contains("nsp")
    {
        0
    } else {
        1
    }
}

fn matches_semantic_router_tag(tag: &str) -> bool {
    matches!(
        tag.trim().to_ascii_lowercase().as_str(),
        "semantic_router" | "assistant_input_parser" | "k3_nsp"
    )
}

fn probe_local_runtime(endpoints: &[ModelEndpoint]) -> LocalRuntimeProjection {
    let Some(target) = resolve_local_runtime_probe_target(endpoints) else {
        return LocalRuntimeProjection::default();
    };

    let now = Instant::now();
    let mut cache = local_runtime_projection_cache()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(cached) = cache
        .as_ref()
        .filter(|cached| cached.target_cache_key == target.cache_key && cached.expires_at > now)
        .cloned()
    {
        return cached.projection;
    }

    let projection = probe_local_runtime_target(&target);
    *cache = Some(CachedLocalRuntimeProjection {
        target_cache_key: target.cache_key,
        expires_at: Instant::now() + LOCAL_RUNTIME_PROJECTION_CACHE_TTL,
        projection: projection.clone(),
    });
    projection
}

fn resolve_local_runtime_probe_target(
    endpoints: &[ModelEndpoint],
) -> Option<LocalRuntimeProbeTarget> {
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
    let template = preferred?;
    let fallback = builtin_defaults
        .iter()
        .find(|endpoint| endpoint.model_endpoint_id == template.model_endpoint_id)
        .or_else(|| {
            builtin_defaults
                .iter()
                .find(|endpoint| is_builtin_local_openai_endpoint(endpoint))
        });

    let template_is_builtin = is_builtin_local_openai_endpoint(&template);
    let raw_base_url = metadata_string(&template.metadata, "base_url");
    let fallback_base_url =
        fallback.and_then(|endpoint| metadata_string(&endpoint.metadata, "base_url"));
    let base_url = raw_base_url
        .filter(|value| !(template_is_builtin && is_legacy_model_api_url(value)))
        .or(fallback_base_url)
        .unwrap_or_default();
    let raw_healthz_url = metadata_string(&template.metadata, "healthz_url");
    let fallback_healthz_url =
        fallback.and_then(|endpoint| metadata_string(&endpoint.metadata, "healthz_url"));
    let healthz_url = raw_healthz_url
        .filter(|value| !(template_is_builtin && is_legacy_model_api_url(value)))
        .or(fallback_healthz_url)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| infer_healthz_url(&base_url));
    let api_key = metadata_string(&template.metadata, "api_key")
        .or_else(|| fallback.and_then(|endpoint| metadata_string(&endpoint.metadata, "api_key")))
        .unwrap_or_default();
    let api_key_configured = metadata_bool(&template.metadata, "api_key_configured")
        || !api_key.trim().is_empty()
        || fallback
            .map(|endpoint| metadata_bool(&endpoint.metadata, "api_key_configured"))
            .unwrap_or(false);

    Some(LocalRuntimeProbeTarget {
        cache_key: format!(
            "{}|{}|{}|{}",
            template.model_endpoint_id, base_url, healthz_url, api_key,
        ),
        base_url,
        healthz_url,
        api_key,
        api_key_configured,
    })
}

fn probe_local_runtime_target(target: &LocalRuntimeProbeTarget) -> LocalRuntimeProjection {
    if target.healthz_url.trim().is_empty() {
        return LocalRuntimeProjection {
            base_url: target.base_url.clone(),
            healthz_url: target.healthz_url.clone(),
            api_key: target.api_key.clone(),
            api_key_configured: target.api_key_configured,
            ready: false,
            backend_ready: false,
            ..Default::default()
        };
    }

    let client = match Client::builder().timeout(Duration::from_secs(3)).build() {
        Ok(client) => client,
        Err(_) => {
            return LocalRuntimeProjection {
                base_url: target.base_url.clone(),
                healthz_url: target.healthz_url.clone(),
                api_key: target.api_key.clone(),
                api_key_configured: target.api_key_configured,
                ready: false,
                backend_ready: false,
                ..Default::default()
            }
        }
    };

    let response = match client.get(&target.healthz_url).send() {
        Ok(response) => response,
        Err(_) => {
            return LocalRuntimeProjection {
                base_url: target.base_url.clone(),
                healthz_url: target.healthz_url.clone(),
                api_key: target.api_key.clone(),
                api_key_configured: target.api_key_configured,
                ready: false,
                backend_ready: false,
                ..Default::default()
            }
        }
    };
    let body = match response.text() {
        Ok(body) => body,
        Err(_) => {
            return LocalRuntimeProjection {
                base_url: target.base_url.clone(),
                healthz_url: target.healthz_url.clone(),
                api_key: target.api_key.clone(),
                api_key_configured: target.api_key_configured,
                ready: false,
                backend_ready: false,
                ..Default::default()
            }
        }
    };
    let payload = match serde_json::from_str::<Value>(&body) {
        Ok(payload) => payload,
        Err(_) => {
            return LocalRuntimeProjection {
                base_url: target.base_url.clone(),
                healthz_url: target.healthz_url.clone(),
                api_key: target.api_key.clone(),
                api_key_configured: target.api_key_configured,
                ready: false,
                backend_ready: false,
                ..Default::default()
            }
        }
    };

    LocalRuntimeProjection {
        base_url: target.base_url.clone(),
        healthz_url: target.healthz_url.clone(),
        api_key: target.api_key.clone(),
        api_key_configured: target.api_key_configured,
        ready: payload
            .get("ready")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        backend_ready: payload
            .get("backend")
            .and_then(|value| value.get("ready"))
            .and_then(Value::as_bool)
            .unwrap_or(false),
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
    }
}

fn overlay_endpoints_with_runtime_truth(
    endpoints: &[ModelEndpoint],
    runtime: &LocalRuntimeProjection,
) -> Vec<ModelEndpoint> {
    let builtin_defaults = default_model_endpoints()
        .into_iter()
        .map(|endpoint| (endpoint.model_endpoint_id.clone(), endpoint))
        .collect::<std::collections::HashMap<_, _>>();

    endpoints
        .iter()
        .map(|endpoint| {
            let mut overlayed = endpoint.clone();
            if let Some(default_endpoint) = builtin_defaults.get(&overlayed.model_endpoint_id) {
                if is_builtin_local_openai_endpoint(default_endpoint) {
                    let resident_router_endpoint =
                        metadata_bool(&overlayed.metadata, "semantic_router_resident_endpoint");
                    let legacy_base_url = metadata_string(&overlayed.metadata, "base_url")
                        .is_some_and(|value| {
                            is_legacy_model_api_url(&value) && !resident_router_endpoint
                        });
                    if metadata_missing_or_empty(&overlayed.metadata, "base_url") || legacy_base_url
                    {
                        set_metadata_string(
                            &mut overlayed.metadata,
                            "base_url",
                            metadata_string(&default_endpoint.metadata, "base_url")
                                .or_else(|| runtime.base_url.clone().if_empty_then(|| None))
                                .unwrap_or_default(),
                        );
                        if legacy_base_url {
                            set_metadata_bool(
                                &mut overlayed.metadata,
                                "legacy_model_api_migrated",
                                true,
                            );
                        }
                    }
                    let legacy_healthz_url = metadata_string(&overlayed.metadata, "healthz_url")
                        .is_some_and(|value| {
                            is_legacy_model_api_url(&value) && !resident_router_endpoint
                        });
                    if metadata_missing_or_empty(&overlayed.metadata, "healthz_url")
                        || legacy_healthz_url
                    {
                        set_metadata_string(
                            &mut overlayed.metadata,
                            "healthz_url",
                            metadata_string(&default_endpoint.metadata, "healthz_url")
                                .or_else(|| runtime.healthz_url.clone().if_empty_then(|| None))
                                .unwrap_or_else(|| infer_healthz_url(&runtime.base_url)),
                        );
                        if legacy_healthz_url {
                            set_metadata_bool(
                                &mut overlayed.metadata,
                                "legacy_model_api_migrated",
                                true,
                            );
                        }
                    }
                    if metadata_missing_or_empty(&overlayed.metadata, "api_key") {
                        set_metadata_string(
                            &mut overlayed.metadata,
                            "api_key",
                            runtime
                                .api_key
                                .clone()
                                .if_empty_then(|| {
                                    metadata_string(&default_endpoint.metadata, "api_key")
                                })
                                .unwrap_or_default(),
                        );
                    }
                    if !metadata_bool(&overlayed.metadata, "api_key_configured")
                        && runtime.api_key_configured
                    {
                        set_metadata_bool(&mut overlayed.metadata, "api_key_configured", true);
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
            overlayed
        })
        .collect()
}

fn endpoint_priority(endpoint: &ModelEndpoint, fallback_order: &[String]) -> usize {
    fallback_order
        .iter()
        .position(|item| item.eq_ignore_ascii_case(endpoint.endpoint_kind.as_str()))
        .unwrap_or(fallback_order.len())
}

fn status_priority(status: ModelEndpointStatus) -> usize {
    match status {
        ModelEndpointStatus::Active => 0,
        ModelEndpointStatus::Degraded => 1,
        ModelEndpointStatus::Disabled => 2,
    }
}

fn test_tesseract_endpoint(endpoint: &ModelEndpoint) -> ModelEndpointTestResult {
    let Some(binary_path) = resolve_tesseract_binary(endpoint) else {
        return ModelEndpointTestResult {
            ok: false,
            status: "degraded".to_string(),
            summary: "Tesseract binary is not available.".to_string(),
            endpoint: redact_model_endpoint(endpoint),
            details: json!({
                "languages": resolve_tesseract_languages(endpoint),
            }),
        };
    };

    match Command::new(&binary_path).arg("--version").output() {
        Ok(output) if output.status.success() => {
            let version_line = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("tesseract")
                .trim()
                .to_string();
            ModelEndpointTestResult {
                ok: true,
                status: "active".to_string(),
                summary: "Tesseract endpoint is ready.".to_string(),
                endpoint: redact_model_endpoint(endpoint),
                details: json!({
                    "binary_path": binary_path.to_string_lossy(),
                    "version": version_line,
                    "languages": resolve_tesseract_languages(endpoint),
                }),
            }
        }
        Ok(output) => ModelEndpointTestResult {
            ok: false,
            status: "degraded".to_string(),
            summary: "Tesseract command returned a non-zero exit code.".to_string(),
            endpoint: redact_model_endpoint(endpoint),
            details: json!({
                "binary_path": binary_path.to_string_lossy(),
                "stderr": String::from_utf8_lossy(&output.stderr).trim(),
            }),
        },
        Err(error) => ModelEndpointTestResult {
            ok: false,
            status: "degraded".to_string(),
            summary: format!("Failed to launch tesseract: {error}"),
            endpoint: redact_model_endpoint(endpoint),
            details: json!({
                "binary_path": binary_path.to_string_lossy(),
            }),
        },
    }
}

fn test_http_endpoint(endpoint: &ModelEndpoint) -> ModelEndpointTestResult {
    let Some(base_url) = metadata_string(&endpoint.metadata, "base_url") else {
        return ModelEndpointTestResult {
            ok: false,
            status: "degraded".to_string(),
            summary: "Endpoint base_url is not configured.".to_string(),
            endpoint: redact_model_endpoint(endpoint),
            details: json!({}),
        };
    };

    let url = connectivity_url(endpoint, &base_url);
    let client = match Client::builder().timeout(Duration::from_secs(4)).build() {
        Ok(client) => client,
        Err(error) => {
            return ModelEndpointTestResult {
                ok: false,
                status: "degraded".to_string(),
                summary: format!("Failed to build HTTP client: {error}"),
                endpoint: redact_model_endpoint(endpoint),
                details: json!({
                    "base_url": base_url,
                }),
            }
        }
    };

    let mut request = client.get(url.as_str());
    if let Some(api_key) = metadata_string(&endpoint.metadata, "api_key") {
        if !api_key.trim().is_empty() {
            request = request.bearer_auth(api_key);
        }
    }

    match request.send() {
        Ok(response) => ModelEndpointTestResult {
            ok: response.status().is_success() || response.status().is_redirection(),
            status: if response.status().is_success() {
                "active".to_string()
            } else {
                "degraded".to_string()
            },
            summary: format!(
                "Endpoint responded with HTTP {}.",
                response.status().as_u16()
            ),
            endpoint: redact_model_endpoint(endpoint),
            details: json!({
                "base_url": base_url,
                "connectivity_url": url,
                "http_status": response.status().as_u16(),
            }),
        },
        Err(error) => ModelEndpointTestResult {
            ok: false,
            status: "degraded".to_string(),
            summary: format!("HTTP probe failed: {error}"),
            endpoint: redact_model_endpoint(endpoint),
            details: json!({
                "base_url": base_url,
                "connectivity_url": url,
            }),
        },
    }
}

fn connectivity_url(endpoint: &ModelEndpoint, base_url: &str) -> String {
    if let Some(healthz_url) = metadata_string(&endpoint.metadata, "healthz_url") {
        return healthz_url;
    }
    let trimmed = base_url.trim().trim_end_matches('/');
    if endpoint.provider_key.eq_ignore_ascii_case("ollama") {
        format!("{trimmed}/api/tags")
    } else if trimmed.ends_with("/v1") {
        format!("{trimmed}/models")
    } else {
        trimmed.to_string()
    }
}

fn resolve_tesseract_binary(endpoint: &ModelEndpoint) -> Option<PathBuf> {
    metadata_string(&endpoint.metadata, "binary_path")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .filter(|path| path.exists())
        .or_else(|| {
            std::env::var(OCR_TESSERACT_PATH_ENV)
                .ok()
                .map(PathBuf::from)
                .filter(|path| path.exists())
        })
        .or_else(|| which::which("tesseract").ok())
}

fn resolve_tesseract_languages(endpoint: &ModelEndpoint) -> String {
    metadata_string(&endpoint.metadata, "languages")
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var(OCR_TESSERACT_LANGS_ENV)
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| DEFAULT_TESSERACT_LANGS.to_string())
}

fn openai_compatible_config_from_endpoint(
    endpoint: &ModelEndpoint,
) -> Option<OpenAiCompatibleConfig> {
    let base_url = metadata_string(&endpoint.metadata, "base_url")?;
    let api_key = metadata_string(&endpoint.metadata, "api_key").unwrap_or_default();
    if endpoint.endpoint_kind == ModelEndpointKind::Cloud && api_key.trim().is_empty() {
        return None;
    }
    let model = metadata_string(&endpoint.metadata, "model").or_else(|| {
        (!endpoint.model_name.trim().is_empty()).then_some(endpoint.model_name.clone())
    })?;
    Some(OpenAiCompatibleConfig {
        base_url: base_url.trim_end_matches('/').to_string(),
        api_key,
        model,
    })
}

fn rerank_compatible_config_from_endpoint(
    endpoint: &ModelEndpoint,
) -> Option<RerankCompatibleConfig> {
    let base_url = metadata_string(&endpoint.metadata, "base_url")?;
    if endpoint.endpoint_kind == ModelEndpointKind::Cloud {
        return None;
    }
    let api_key = metadata_string(&endpoint.metadata, "api_key").unwrap_or_default();
    let model = metadata_string(&endpoint.metadata, "model").or_else(|| {
        (!endpoint.model_name.trim().is_empty()).then_some(endpoint.model_name.clone())
    })?;
    let rerank_path =
        metadata_string(&endpoint.metadata, "rerank_path").unwrap_or_else(|| "/rerank".to_string());
    Some(RerankCompatibleConfig {
        base_url: base_url.trim_end_matches('/').to_string(),
        api_key,
        model,
        rerank_path,
    })
}

fn build_image_data_url(image_path: &Path) -> Result<String, String> {
    let bytes = fs::read(image_path)
        .map_err(|error| format!("Failed to read image {}: {error}", image_path.display()))?;
    let mime = image_mime_type(image_path);
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(format!("data:{mime};base64,{encoded}"))
}

fn image_mime_type(image_path: &Path) -> &'static str {
    match image_path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("bmp") => "image/bmp",
        _ => "application/octet-stream",
    }
}

fn metadata_string(metadata: &Value, key: &str) -> Option<String> {
    metadata
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string())
}

fn metadata_bool(metadata: &Value, key: &str) -> bool {
    metadata.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn metadata_missing_or_empty(metadata: &Value, key: &str) -> bool {
    metadata_string(metadata, key).is_none()
}

fn env_trimmed(key: &str) -> Option<String> {
    env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn is_legacy_model_api_url(value: &str) -> bool {
    let normalized = value.trim().to_ascii_lowercase();
    normalized.contains("127.0.0.1:4176") || normalized.contains("localhost:4176")
}

fn set_metadata_string(metadata: &mut Value, key: &str, value: String) {
    if value.trim().is_empty() {
        return;
    }
    if !metadata.is_object() {
        *metadata = json!({});
    }
    if let Some(map) = metadata.as_object_mut() {
        map.insert(key.to_string(), Value::String(value));
    }
}

fn set_metadata_bool(metadata: &mut Value, key: &str, value: bool) {
    if !metadata.is_object() {
        *metadata = json!({});
    }
    if let Some(map) = metadata.as_object_mut() {
        map.insert(key.to_string(), Value::Bool(value));
    }
}

fn infer_healthz_url(base_url: &str) -> String {
    let trimmed = base_url.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    if let Some(prefix) = trimmed.strip_suffix("/v1") {
        format!("{prefix}/healthz")
    } else {
        format!("{trimmed}/healthz")
    }
}

fn is_builtin_local_openai_endpoint(endpoint: &ModelEndpoint) -> bool {
    matches!(
        endpoint.model_kind,
        ModelKind::Llm | ModelKind::Embedder | ModelKind::Vlm
    ) && endpoint.endpoint_kind == crate::control_plane::models::ModelEndpointKind::Local
        && endpoint
            .provider_key
            .eq_ignore_ascii_case("openai_compatible")
        && metadata_bool(&endpoint.metadata, "builtin")
}

trait EmptyStringFallback {
    fn if_empty_then<F>(self, fallback: F) -> Option<String>
    where
        F: FnOnce() -> Option<String>;
}

impl EmptyStringFallback for String {
    fn if_empty_then<F>(self, fallback: F) -> Option<String>
    where
        F: FnOnce() -> Option<String>,
    {
        if self.trim().is_empty() {
            fallback()
        } else {
            Some(self)
        }
    }
}

fn mock_embedding_vector_from_endpoint(endpoint: &ModelEndpoint, input: &str) -> Option<Vec<f32>> {
    if let Some(vector) = endpoint
        .metadata
        .get("mock_embeddings")
        .and_then(Value::as_object)
        .and_then(|map| map.get(input))
        .and_then(parse_embedding_vector)
    {
        return Some(vector);
    }

    let dimensions = endpoint
        .metadata
        .get("mock_embedding_dimensions")
        .and_then(Value::as_u64)
        .map(|value| value as usize)
        .filter(|value| *value > 0)
        .or_else(|| {
            endpoint
                .metadata
                .get("mock_embedding")
                .and_then(Value::as_bool)
                .filter(|value| *value)
                .map(|_| 8usize)
        })?;

    Some(build_mock_embedding(input, dimensions))
}

fn mock_rerank_scores_from_endpoint(
    endpoint: &ModelEndpoint,
    document_count: usize,
) -> Option<Vec<RerankDocumentScore>> {
    let values = endpoint
        .metadata
        .get("mock_rerank_scores")
        .and_then(Value::as_array)?;
    let mut scores = values
        .iter()
        .enumerate()
        .filter_map(|(index, value)| {
            let score = value.as_f64()? as f32;
            (index < document_count && score.is_finite())
                .then_some(RerankDocumentScore { index, score })
        })
        .collect::<Vec<_>>();
    scores.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| left.index.cmp(&right.index))
    });
    Some(scores)
}

fn rerank_score_to_document_score(score: RerankScore) -> RerankDocumentScore {
    RerankDocumentScore {
        index: score.index,
        score: score.score,
    }
}

fn parse_embedding_vector(value: &Value) -> Option<Vec<f32>> {
    let items = value.as_array()?;
    let mut vector = Vec::with_capacity(items.len());
    for item in items {
        vector.push(item.as_f64()? as f32);
    }
    (!vector.is_empty()).then_some(vector)
}

fn build_mock_embedding(input: &str, dimensions: usize) -> Vec<f32> {
    let mut vector = vec![0.0f32; dimensions.max(1)];
    for (index, ch) in input.chars().enumerate() {
        let slot = index % vector.len();
        let weight = ((ch as u32 % 17) + 1) as f32;
        vector[slot] += weight;
    }

    let norm = vector.iter().map(|value| value * value).sum::<f32>().sqrt();
    if norm > 0.0 {
        for value in &mut vector {
            *value /= norm;
        }
    }
    vector
}

fn redact_secret_value(value: &mut Value) {
    match value {
        Value::Object(map) => {
            let mut configured_flags = Vec::new();
            for (key, nested) in map.iter_mut() {
                if is_secret_key(key.as_str()) {
                    let configured = secret_present(nested);
                    *nested = Value::String(String::new());
                    configured_flags.push((format!("{key}_configured"), Value::Bool(configured)));
                    continue;
                }
                redact_secret_value(nested);
            }
            for (key, value) in configured_flags {
                map.entry(key).or_insert(value);
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_secret_value(item);
            }
        }
        _ => {}
    }
}

fn is_secret_key(key: &str) -> bool {
    matches!(
        key,
        "api_key" | "token" | "secret" | "password" | "authorization" | "bearer_token"
    )
}

fn secret_present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(text) => !text.trim().is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Mutex;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    use serde_json::json;
    use tiny_http::{Header, Method, Response, Server};

    use super::{
        clear_local_runtime_projection_cache, connectivity_url,
        openai_compatible_config_from_endpoint, redact_model_endpoint, run_embedding_with_state,
        run_llm_text_with_state, run_llm_text_with_state_and_options, run_rerank_with_state,
        run_vlm_summary_with_state, semantic_router_local_only_model_state, test_model_endpoint,
        vlm_endpoint_readiness, LlmTextOptions, RERANK_POLICY_ID,
    };
    use crate::control_plane::models::{
        ModelEndpoint, ModelEndpointKind, ModelEndpointStatus, ModelKind, ModelRoutePolicy,
        PrivacyLevel,
    };
    use crate::runtime::admin_console::AdminModelCenterState;

    static MODEL_RUNTIME_ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn rerank_mock_endpoint_returns_scores() {
        let _env_guard = MODEL_RUNTIME_ENV_LOCK
            .lock()
            .expect("model runtime env lock");
        clear_local_runtime_projection_cache();
        let state = AdminModelCenterState {
            endpoints: vec![ModelEndpoint {
                model_endpoint_id: "rerank-local".to_string(),
                workspace_id: Some("home-1".to_string()),
                provider_account_id: None,
                model_kind: ModelKind::Reranker,
                endpoint_kind: ModelEndpointKind::Local,
                provider_key: "rerank_compatible".to_string(),
                model_name: "mock-reranker".to_string(),
                capability_tags: vec!["rerank".to_string()],
                cost_policy: json!({}),
                status: ModelEndpointStatus::Active,
                metadata: json!({"mock_rerank_scores": [0.2, 0.9]}),
            }],
            route_policies: vec![ModelRoutePolicy {
                route_policy_id: RERANK_POLICY_ID.to_string(),
                workspace_id: "home-1".to_string(),
                domain_scope: "retrieval".to_string(),
                modality: "text".to_string(),
                privacy_level: PrivacyLevel::StrictLocal,
                local_preferred: true,
                max_cost_per_run: None,
                fallback_order: vec!["local".to_string(), "sidecar".to_string()],
                status: "active".to_string(),
                metadata: json!({"capability": "rerank"}),
            }],
            ..Default::default()
        };

        let documents = vec!["alpha".to_string(), "beta".to_string()];
        let result = run_rerank_with_state("query", &documents, 2, &state);

        assert!(result.available);
        assert_eq!(result.model_endpoint_id.as_deref(), Some("rerank-local"));
        assert_eq!(result.scores[0].index, 1);
    }

    #[test]
    fn rerank_strict_local_policy_does_not_select_cloud_endpoint() {
        let _env_guard = MODEL_RUNTIME_ENV_LOCK
            .lock()
            .expect("model runtime env lock");
        clear_local_runtime_projection_cache();
        let state = AdminModelCenterState {
            endpoints: vec![
                ModelEndpoint {
                    model_endpoint_id: "rerank-cloud".to_string(),
                    workspace_id: Some("home-1".to_string()),
                    provider_account_id: None,
                    model_kind: ModelKind::Reranker,
                    endpoint_kind: ModelEndpointKind::Cloud,
                    provider_key: "rerank_compatible".to_string(),
                    model_name: "cloud-reranker".to_string(),
                    capability_tags: vec!["rerank".to_string()],
                    cost_policy: json!({}),
                    status: ModelEndpointStatus::Active,
                    metadata: json!({"mock_rerank_scores": [1.0, 1.0]}),
                },
                ModelEndpoint {
                    model_endpoint_id: "rerank-local".to_string(),
                    workspace_id: Some("home-1".to_string()),
                    provider_account_id: None,
                    model_kind: ModelKind::Reranker,
                    endpoint_kind: ModelEndpointKind::Local,
                    provider_key: "rerank_compatible".to_string(),
                    model_name: "local-reranker".to_string(),
                    capability_tags: vec!["rerank".to_string()],
                    cost_policy: json!({}),
                    status: ModelEndpointStatus::Active,
                    metadata: json!({"mock_rerank_scores": [0.1, 0.7]}),
                },
            ],
            route_policies: vec![ModelRoutePolicy {
                route_policy_id: RERANK_POLICY_ID.to_string(),
                workspace_id: "home-1".to_string(),
                domain_scope: "retrieval".to_string(),
                modality: "text".to_string(),
                privacy_level: PrivacyLevel::StrictLocal,
                local_preferred: true,
                max_cost_per_run: None,
                fallback_order: vec!["cloud".to_string(), "local".to_string()],
                status: "active".to_string(),
                metadata: json!({"capability": "rerank", "cloud_fallback_allowed": false}),
            }],
            ..Default::default()
        };

        let documents = vec!["alpha".to_string(), "beta".to_string()];
        let result = run_rerank_with_state("query", &documents, 2, &state);

        assert!(result.available);
        assert_eq!(result.model_endpoint_id.as_deref(), Some("rerank-local"));
        assert_eq!(result.scores[0].index, 1);
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(value) = self.previous.as_ref() {
                std::env::set_var(self.key, value);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    #[test]
    fn redact_model_endpoint_masks_api_keys() {
        let endpoint = ModelEndpoint {
            model_endpoint_id: "cloud-llm".to_string(),
            workspace_id: Some("home-1".to_string()),
            provider_account_id: None,
            model_kind: ModelKind::Llm,
            endpoint_kind: ModelEndpointKind::Cloud,
            provider_key: "custom".to_string(),
            model_name: "gpt-like".to_string(),
            capability_tags: vec!["chat".to_string()],
            cost_policy: json!({}),
            status: ModelEndpointStatus::Active,
            metadata: json!({
                "base_url": "https://api.example.com/v1",
                "api_key": "secret_value",
            }),
        };

        let redacted = redact_model_endpoint(&endpoint);

        assert_eq!(redacted.metadata["api_key"], json!(""));
        assert_eq!(redacted.metadata["api_key_configured"], json!(true));
        assert_eq!(
            redacted.metadata["base_url"],
            json!("https://api.example.com/v1")
        );
    }

    #[test]
    fn local_openai_compatible_endpoint_allows_empty_api_key() {
        let endpoint = ModelEndpoint {
            model_endpoint_id: "llm-k3-nsp-local-llama".to_string(),
            workspace_id: Some("home-1".to_string()),
            provider_account_id: None,
            model_kind: ModelKind::Llm,
            endpoint_kind: ModelEndpointKind::Local,
            provider_key: "openai_compatible".to_string(),
            model_name: "Qwen3-1.7B-Q8_0.gguf".to_string(),
            capability_tags: vec!["semantic_router".to_string()],
            cost_policy: json!({}),
            status: ModelEndpointStatus::Active,
            metadata: json!({
                "base_url": "http://127.0.0.1:8091/v1",
                "local_only": true,
            }),
        };

        let config = openai_compatible_config_from_endpoint(&endpoint).expect("local config");

        assert_eq!(config.base_url, "http://127.0.0.1:8091/v1");
        assert_eq!(config.api_key, "");
        assert_eq!(config.model, "Qwen3-1.7B-Q8_0.gguf");
    }

    #[test]
    fn cloud_openai_compatible_endpoint_requires_api_key() {
        let endpoint = ModelEndpoint {
            model_endpoint_id: "llm-cloud-siliconflow".to_string(),
            workspace_id: Some("home-1".to_string()),
            provider_account_id: None,
            model_kind: ModelKind::Llm,
            endpoint_kind: ModelEndpointKind::Cloud,
            provider_key: "openai_compatible".to_string(),
            model_name: "deepseek-ai/DeepSeek-V4-Flash".to_string(),
            capability_tags: vec!["cloud_fallback".to_string()],
            cost_policy: json!({}),
            status: ModelEndpointStatus::Active,
            metadata: json!({
                "base_url": "https://api.siliconflow.cn/v1",
            }),
        };

        assert!(openai_compatible_config_from_endpoint(&endpoint).is_none());
    }

    #[test]
    fn vlm_endpoint_readiness_blocks_cloud_and_non_loopback_endpoints() {
        let cloud_state = AdminModelCenterState {
            endpoints: vec![ModelEndpoint {
                model_endpoint_id: "vlm-cloud".to_string(),
                workspace_id: Some("home-1".to_string()),
                provider_account_id: None,
                model_kind: ModelKind::Vlm,
                endpoint_kind: ModelEndpointKind::Cloud,
                provider_key: "openai_compatible".to_string(),
                model_name: "remote-vlm".to_string(),
                capability_tags: vec!["vision".to_string()],
                cost_policy: json!({}),
                status: ModelEndpointStatus::Active,
                metadata: json!({
                    "base_url": "https://example.invalid/v1",
                    "api_key_configured": true,
                }),
            }],
            route_policies: vec![ModelRoutePolicy {
                route_policy_id: "retrieval.vision_summary".to_string(),
                workspace_id: "home-1".to_string(),
                domain_scope: "retrieval".to_string(),
                modality: "vision".to_string(),
                privacy_level: PrivacyLevel::AllowCloud,
                local_preferred: false,
                max_cost_per_run: None,
                fallback_order: vec!["cloud".to_string()],
                status: "active".to_string(),
                metadata: json!({}),
            }],
            ..Default::default()
        };
        let cloud_readiness = vlm_endpoint_readiness(&cloud_state);
        assert_eq!(cloud_readiness["status"], json!("blocked"));
        assert_eq!(cloud_readiness["endpoint_ready"], json!(false));
        assert_eq!(cloud_readiness["local_only"], json!(false));

        let remote_local_state = AdminModelCenterState {
            endpoints: vec![ModelEndpoint {
                model_endpoint_id: "vlm-remote-local".to_string(),
                workspace_id: Some("home-1".to_string()),
                provider_account_id: None,
                model_kind: ModelKind::Vlm,
                endpoint_kind: ModelEndpointKind::Local,
                provider_key: "openai_compatible".to_string(),
                model_name: "local-vlm".to_string(),
                capability_tags: vec!["vision".to_string()],
                cost_policy: json!({}),
                status: ModelEndpointStatus::Active,
                metadata: json!({
                    "base_url": "http://192.168.3.50:8080/v1",
                    "local_only": true,
                }),
            }],
            ..Default::default()
        };
        let remote_readiness = vlm_endpoint_readiness(&remote_local_state);
        assert_eq!(remote_readiness["status"], json!("blocked"));
        assert_eq!(remote_readiness["endpoint_ready"], json!(false));
        assert_eq!(remote_readiness["local_only"], json!(false));
        assert_eq!(remote_readiness["fallback_allowed"], json!(false));
    }

    #[test]
    fn test_model_endpoint_supports_mock_mode() {
        let endpoint = ModelEndpoint {
            model_endpoint_id: "ocr-mock".to_string(),
            workspace_id: Some("home-1".to_string()),
            provider_account_id: None,
            model_kind: ModelKind::Ocr,
            endpoint_kind: ModelEndpointKind::Local,
            provider_key: "tesseract".to_string(),
            model_name: "mock".to_string(),
            capability_tags: vec!["ocr".to_string()],
            cost_policy: json!({}),
            status: ModelEndpointStatus::Active,
            metadata: json!({
                "mock_text": "front gate camera",
            }),
        };

        let result = test_model_endpoint(&endpoint);

        assert!(result.ok);
        assert_eq!(result.status, "active");
        assert_eq!(result.details["mock_text_length"], json!(17));
    }

    #[test]
    fn run_vlm_summary_supports_mock_mode() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let temp_dir = std::env::temp_dir().join(format!("harborbeacon-vlm-mock-{unique}"));
        fs::create_dir_all(&temp_dir).expect("temp dir");
        let image_path = temp_dir.join("frame.jpg");
        fs::write(&image_path, b"fake-image").expect("write image");

        let state = AdminModelCenterState {
            endpoints: vec![ModelEndpoint {
                model_endpoint_id: "vlm-mock".to_string(),
                workspace_id: Some("home-1".to_string()),
                provider_account_id: None,
                model_kind: ModelKind::Vlm,
                endpoint_kind: ModelEndpointKind::Local,
                provider_key: "openai_compatible".to_string(),
                model_name: "vision".to_string(),
                capability_tags: vec!["multimodal".to_string()],
                cost_policy: json!({}),
                status: ModelEndpointStatus::Active,
                metadata: json!({
                    "mock_text": "画面里有一台放在门口的快递箱",
                }),
            }],
            route_policies: vec![ModelRoutePolicy {
                route_policy_id: "retrieval.vision_summary".to_string(),
                workspace_id: "home-1".to_string(),
                domain_scope: "retrieval".to_string(),
                modality: "multimodal".to_string(),
                privacy_level: PrivacyLevel::AllowRedactedCloud,
                local_preferred: true,
                max_cost_per_run: None,
                fallback_order: vec!["local".to_string(), "cloud".to_string()],
                status: "active".to_string(),
                metadata: json!({}),
            }],
            ..AdminModelCenterState::default()
        };

        let result = run_vlm_summary_with_state(&image_path, &state);
        assert!(result.available);
        assert_eq!(result.status, "active");
        assert_eq!(result.text, "画面里有一台放在门口的快递箱");

        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn run_embedding_supports_mock_dimensions_and_overrides() {
        let state = AdminModelCenterState {
            endpoints: vec![ModelEndpoint {
                model_endpoint_id: "embed-mock".to_string(),
                workspace_id: Some("home-1".to_string()),
                provider_account_id: None,
                model_kind: ModelKind::Embedder,
                endpoint_kind: ModelEndpointKind::Local,
                provider_key: "openai_compatible".to_string(),
                model_name: "mock-embed".to_string(),
                capability_tags: vec!["embeddings".to_string()],
                cost_policy: json!({}),
                status: ModelEndpointStatus::Active,
                metadata: json!({
                    "mock_embedding_dimensions": 4,
                    "mock_embeddings": {
                        "樱花整理": [1.0, 0.0, 0.0, 0.0]
                    }
                }),
            }],
            route_policies: vec![ModelRoutePolicy {
                route_policy_id: "retrieval.embed".to_string(),
                workspace_id: "home-1".to_string(),
                domain_scope: "retrieval".to_string(),
                modality: "text".to_string(),
                privacy_level: PrivacyLevel::StrictLocal,
                local_preferred: true,
                max_cost_per_run: None,
                fallback_order: vec!["local".to_string(), "cloud".to_string()],
                status: "active".to_string(),
                metadata: json!({}),
            }],
            ..AdminModelCenterState::default()
        };

        let exact = run_embedding_with_state("樱花整理", &state);
        assert!(exact.available);
        assert_eq!(exact.vector, vec![1.0, 0.0, 0.0, 0.0]);

        let generated = run_embedding_with_state("整理计划", &state);
        assert!(generated.available);
        assert_eq!(generated.vector.len(), 4);
    }

    #[test]
    fn connectivity_url_prefers_explicit_healthz_metadata() {
        let endpoint = ModelEndpoint {
            model_endpoint_id: "llm-local".to_string(),
            workspace_id: Some("home-1".to_string()),
            provider_account_id: None,
            model_kind: ModelKind::Llm,
            endpoint_kind: ModelEndpointKind::Local,
            provider_key: "openai_compatible".to_string(),
            model_name: "chat".to_string(),
            capability_tags: vec!["chat".to_string()],
            cost_policy: json!({}),
            status: ModelEndpointStatus::Degraded,
            metadata: json!({
                "base_url": "http://127.0.0.1:4176/v1",
                "healthz_url": "http://127.0.0.1:4176/healthz",
            }),
        };

        assert_eq!(
            connectivity_url(&endpoint, "http://127.0.0.1:4176/v1"),
            "http://127.0.0.1:4176/healthz"
        );
    }

    #[test]
    fn run_llm_text_with_state_uses_runtime_overlay_for_stale_builtin_local_endpoint() {
        let _guard = MODEL_RUNTIME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        clear_local_runtime_projection_cache();
        let server = Server::http("127.0.0.1:0").expect("server");
        let base_url = format!("http://{}/v1", server.server_addr());
        let healthz_header =
            Header::from_bytes(b"Content-Type", b"application/json").expect("health header");
        let chat_header =
            Header::from_bytes(b"Content-Type", b"application/json").expect("chat header");

        let server_thread = thread::spawn(move || {
            for _ in 0..2 {
                let request = server.recv().expect("request");
                match (request.method(), request.url()) {
                    (&Method::Get, "/healthz") => request
                        .respond(
                            Response::from_string(
                                r#"{"ready":true,"backend":{"ready":true,"kind":"candle"},"chat_model":"Qwen/Qwen2.5-0.5B-Instruct"}"#,
                            )
                            .with_header(healthz_header.clone()),
                        )
                        .expect("health response"),
                    (&Method::Post, "/v1/chat/completions") => request
                        .respond(
                            Response::from_string(
                                r#"{"choices":[{"message":{"content":"{\"decision\":\"capability_summary\",\"reply_text\":\"我可以帮你抓拍最新画面。\"}"}}]}"#,
                            )
                            .with_header(chat_header.clone()),
                        )
                        .expect("chat response"),
                    _ => request
                        .respond(Response::from_string("not found").with_status_code(404))
                        .expect("404 response"),
                }
            }
        });

        std::env::set_var("HARBOR_MODEL_API_BASE_URL", &base_url);
        std::env::set_var("HARBOR_MODEL_API_TOKEN", "runtime-overlay-token");

        let state = AdminModelCenterState {
            endpoints: vec![ModelEndpoint {
                model_endpoint_id: "llm-local-openai-compatible".to_string(),
                workspace_id: Some("home-1".to_string()),
                provider_account_id: None,
                model_kind: ModelKind::Llm,
                endpoint_kind: ModelEndpointKind::Local,
                provider_key: "openai_compatible".to_string(),
                model_name: "harbor-local-chat".to_string(),
                capability_tags: vec!["chat".to_string(), "local_first".to_string()],
                cost_policy: json!({}),
                status: ModelEndpointStatus::Disabled,
                metadata: json!({
                    "builtin": true,
                    "base_url": "",
                    "healthz_url": "",
                    "api_key": "",
                    "api_key_configured": false,
                }),
            }],
            route_policies: vec![ModelRoutePolicy {
                route_policy_id: "retrieval.answer".to_string(),
                workspace_id: "home-1".to_string(),
                domain_scope: "retrieval".to_string(),
                modality: "text".to_string(),
                privacy_level: PrivacyLevel::AllowRedactedCloud,
                local_preferred: true,
                max_cost_per_run: None,
                fallback_order: vec!["local".to_string(), "cloud".to_string()],
                status: "active".to_string(),
                metadata: json!({}),
            }],
            ..AdminModelCenterState::default()
        };

        let result = run_llm_text_with_state("摄像头能干什么", &state);

        std::env::remove_var("HARBOR_MODEL_API_BASE_URL");
        std::env::remove_var("HARBOR_MODEL_API_TOKEN");
        clear_local_runtime_projection_cache();
        server_thread.join().expect("server thread");

        assert!(result.available);
        assert_eq!(result.status, "active");
        assert!(result.text.contains("\"decision\":\"capability_summary\""));
        assert!(result.text.contains("我可以帮你抓拍最新画面。"));
    }

    #[test]
    fn run_embedding_migrates_legacy_builtin_4176_endpoint_to_runtime_proxy() {
        let _guard = MODEL_RUNTIME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        clear_local_runtime_projection_cache();
        let server = Server::http("127.0.0.1:0").expect("server");
        let base_url = format!("http://{}/v1", server.server_addr());
        let header = Header::from_bytes(b"Content-Type", b"application/json").expect("header");

        let server_thread = thread::spawn(move || {
            for _ in 0..2 {
                let request = server.recv().expect("request");
                match (request.method(), request.url()) {
                    (&Method::Get, "/healthz") => request
                        .respond(
                            Response::from_string(
                                r#"{"ready":true,"backend":{"ready":true,"kind":"openai_proxy"},"embedding_model":"Qwen/Qwen3-Embedding-0.6B"}"#,
                            )
                            .with_header(header.clone()),
                        )
                        .expect("health response"),
                    (&Method::Post, "/v1/embeddings") => request
                        .respond(
                            Response::from_string(
                                r#"{"data":[{"embedding":[0.1,0.2,0.3]}],"model":"Qwen/Qwen3-Embedding-0.6B"}"#,
                            )
                            .with_header(header.clone()),
                        )
                        .expect("embedding response"),
                    _ => request
                        .respond(Response::from_string("not found").with_status_code(404))
                        .expect("404 response"),
                }
            }
        });

        std::env::set_var("HARBOR_MODEL_API_BASE_URL", &base_url);
        std::env::set_var("HARBOR_MODEL_API_TOKEN", "runtime-overlay-token");

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
            route_policies: vec![ModelRoutePolicy {
                route_policy_id: "retrieval.embed".to_string(),
                workspace_id: "home-1".to_string(),
                domain_scope: "retrieval".to_string(),
                modality: "text".to_string(),
                privacy_level: PrivacyLevel::StrictLocal,
                local_preferred: true,
                max_cost_per_run: None,
                fallback_order: vec!["local".to_string(), "cloud".to_string()],
                status: "active".to_string(),
                metadata: json!({}),
            }],
            ..AdminModelCenterState::default()
        };

        let result = run_embedding_with_state("谁在倒啤酒", &state);

        std::env::remove_var("HARBOR_MODEL_API_BASE_URL");
        std::env::remove_var("HARBOR_MODEL_API_TOKEN");
        clear_local_runtime_projection_cache();
        server_thread.join().expect("server thread");

        assert!(result.available);
        assert_eq!(
            result.model_endpoint_id.as_deref(),
            Some("embed-local-openai-compatible")
        );
        assert_eq!(result.vector, vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn run_llm_text_with_state_and_options_forwards_max_tokens() {
        let _guard = MODEL_RUNTIME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let server = Server::http("127.0.0.1:0").expect("server");
        let base_url = format!("http://{}/v1", server.server_addr());
        let healthz_url = format!("http://{}/healthz", server.server_addr());
        let chat_header =
            Header::from_bytes(b"Content-Type", b"application/json").expect("chat header");

        let server_thread = thread::spawn(move || {
            let mut request = server.recv().expect("request");
            assert_eq!(request.method(), &Method::Post);
            assert_eq!(request.url(), "/v1/chat/completions");
            let mut body = String::new();
            request
                .as_reader()
                .read_to_string(&mut body)
                .expect("read body");
            let payload: serde_json::Value = serde_json::from_str(&body).expect("payload json");
            assert_eq!(payload["max_tokens"], json!(12), "{body}");
            request
                .respond(
                    Response::from_string(
                        r#"{"choices":[{"message":{"content":"capability_summary"}}]}"#,
                    )
                    .with_header(chat_header.clone()),
                )
                .expect("chat response");
        });

        let state = AdminModelCenterState {
            endpoints: vec![ModelEndpoint {
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
                    "builtin": false,
                    "base_url": base_url,
                    "healthz_url": healthz_url,
                    "api_key": "runtime-overlay-token",
                    "api_key_configured": true,
                }),
            }],
            route_policies: vec![ModelRoutePolicy {
                route_policy_id: "retrieval.answer".to_string(),
                workspace_id: "home-1".to_string(),
                domain_scope: "retrieval".to_string(),
                modality: "text".to_string(),
                privacy_level: PrivacyLevel::AllowRedactedCloud,
                local_preferred: true,
                max_cost_per_run: None,
                fallback_order: vec!["local".to_string(), "cloud".to_string()],
                status: "active".to_string(),
                metadata: json!({}),
            }],
            ..AdminModelCenterState::default()
        };

        let result = run_llm_text_with_state_and_options(
            "摄像头能干什么",
            &state,
            &LlmTextOptions {
                purpose: Some("rag.answer".to_string()),
                max_tokens: Some(12),
                ..Default::default()
            },
        );

        clear_local_runtime_projection_cache();
        server_thread.join().expect("server thread");

        assert!(result.available);
        assert_eq!(result.text, "capability_summary");
        assert_eq!(result.details["max_tokens"], json!(12));
    }

    #[test]
    fn run_llm_text_with_state_keeps_router_local_only_even_when_cloud_is_configured() {
        let state = AdminModelCenterState {
            endpoints: vec![
                ModelEndpoint {
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
                        "builtin": false,
                        "base_url": "http://127.0.0.1:9/v1",
                        "api_key": "",
                    }),
                },
                ModelEndpoint {
                    model_endpoint_id: "llm-cloud-siliconflow".to_string(),
                    workspace_id: Some("home-1".to_string()),
                    provider_account_id: None,
                    model_kind: ModelKind::Llm,
                    endpoint_kind: ModelEndpointKind::Cloud,
                    provider_key: "openai_compatible".to_string(),
                    model_name: "deepseek-ai/DeepSeek-V4-Flash".to_string(),
                    capability_tags: vec![
                        "chat".to_string(),
                        "cloud_fallback".to_string(),
                        "openai_compatible".to_string(),
                    ],
                    cost_policy: json!({"cost_hint": "cloud_metered"}),
                    status: ModelEndpointStatus::Active,
                    metadata: json!({
                        "builtin": true,
                        "base_url": "https://api.siliconflow.cn/v1",
                        "api_key": "configured",
                        "mock_text": "rag_answer",
                    }),
                },
            ],
            route_policies: vec![ModelRoutePolicy {
                route_policy_id: "semantic.router".to_string(),
                workspace_id: "home-1".to_string(),
                domain_scope: "semantic".to_string(),
                modality: "text".to_string(),
                privacy_level: PrivacyLevel::AllowRedactedCloud,
                local_preferred: true,
                max_cost_per_run: None,
                fallback_order: vec!["local".to_string(), "cloud".to_string()],
                status: "active".to_string(),
                metadata: json!({"capability": "router"}),
            }],
            ..AdminModelCenterState::default()
        };

        let result = run_llm_text_with_state_and_options(
            "route this",
            &state,
            &LlmTextOptions {
                purpose: Some("router".to_string()),
                ..Default::default()
            },
        );

        assert!(!result.available);
        assert_eq!(result.details["route_policy_id"], json!("semantic.router"));
        assert_eq!(result.details["local_only"], json!(true));
        assert_eq!(result.details["fallback_used"], json!(false));
        assert_eq!(
            result.details["attempted_endpoints"],
            json!(["llm-local-openai-compatible"])
        );
    }

    #[test]
    fn semantic_router_local_only_state_wires_resident_endpoint() {
        let _guard = MODEL_RUNTIME_ENV_LOCK
            .lock()
            .expect("model runtime env lock");
        let _base_url = EnvVarGuard::set(
            "HARBOR_SEMANTIC_ROUTER_BASE_URL",
            "http://127.0.0.1:4176/v1",
        );
        let _healthz = EnvVarGuard::set(
            "HARBOR_SEMANTIC_ROUTER_HEALTHZ_URL",
            "http://127.0.0.1:4176/healthz",
        );
        let state = AdminModelCenterState {
            endpoints: vec![
                ModelEndpoint {
                    model_endpoint_id: "llm-local-openai-compatible".to_string(),
                    workspace_id: Some("home-1".to_string()),
                    provider_account_id: None,
                    model_kind: ModelKind::Llm,
                    endpoint_kind: ModelEndpointKind::Local,
                    provider_key: "openai_compatible".to_string(),
                    model_name: "harbor-local-chat".to_string(),
                    capability_tags: vec!["chat".to_string(), "local_first".to_string()],
                    cost_policy: json!({}),
                    status: ModelEndpointStatus::Degraded,
                    metadata: json!({
                        "builtin": true,
                        "base_url": "http://127.0.0.1:4174/api/inference/v1",
                        "healthz_url": "http://127.0.0.1:4174/api/inference/healthz",
                    }),
                },
                ModelEndpoint {
                    model_endpoint_id: "llm-cloud-siliconflow".to_string(),
                    workspace_id: Some("home-1".to_string()),
                    provider_account_id: None,
                    model_kind: ModelKind::Llm,
                    endpoint_kind: ModelEndpointKind::Cloud,
                    provider_key: "openai_compatible".to_string(),
                    model_name: "deepseek-ai/DeepSeek-V4-Flash".to_string(),
                    capability_tags: vec!["cloud_fallback".to_string()],
                    cost_policy: json!({}),
                    status: ModelEndpointStatus::Active,
                    metadata: json!({
                        "base_url": "https://api.siliconflow.cn/v1",
                        "api_key": "configured",
                    }),
                },
            ],
            route_policies: vec![ModelRoutePolicy {
                route_policy_id: "semantic.router".to_string(),
                workspace_id: "home-1".to_string(),
                domain_scope: "semantic".to_string(),
                modality: "text".to_string(),
                privacy_level: PrivacyLevel::AllowRedactedCloud,
                local_preferred: true,
                max_cost_per_run: None,
                fallback_order: vec!["local".to_string(), "cloud".to_string()],
                status: "active".to_string(),
                metadata: json!({"capability": "router"}),
            }],
            ..AdminModelCenterState::default()
        };

        let local_state = semantic_router_local_only_model_state(&state);
        let endpoint = local_state
            .endpoints
            .iter()
            .find(|endpoint| endpoint.model_endpoint_id == "llm-local-openai-compatible")
            .expect("resident semantic router endpoint");
        assert_eq!(
            endpoint.metadata["base_url"],
            json!("http://127.0.0.1:4176/v1")
        );
        assert_eq!(
            endpoint.metadata["healthz_url"],
            json!("http://127.0.0.1:4176/healthz")
        );
        assert_eq!(endpoint.metadata["semantic_router"], json!(true));
        assert_eq!(endpoint.metadata["local_only"], json!(true));
        assert_eq!(
            endpoint.metadata["semantic_router_resident_endpoint"],
            json!(true)
        );
        assert!(endpoint
            .capability_tags
            .iter()
            .any(|tag| tag == "semantic_router"));
        assert!(!local_state
            .endpoints
            .iter()
            .any(|endpoint| endpoint.endpoint_kind == ModelEndpointKind::Cloud));
        let router_policy = local_state
            .route_policies
            .iter()
            .find(|policy| policy.route_policy_id == "semantic.router")
            .expect("router policy");
        assert_eq!(router_policy.privacy_level, PrivacyLevel::StrictLocal);
        assert!(!router_policy
            .fallback_order
            .iter()
            .any(|kind| kind.eq_ignore_ascii_case("cloud")));
    }

    #[test]
    fn semantic_router_prefers_tagged_local_parser_endpoint() {
        let state = AdminModelCenterState {
            endpoints: vec![
                ModelEndpoint {
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
                        "builtin": false,
                        "mock_text": "generic_local",
                    }),
                },
                ModelEndpoint {
                    model_endpoint_id: "zz-k3-nsp-router".to_string(),
                    workspace_id: Some("home-1".to_string()),
                    provider_account_id: None,
                    model_kind: ModelKind::Llm,
                    endpoint_kind: ModelEndpointKind::Local,
                    provider_key: "openai_compatible".to_string(),
                    model_name: "Qwen3-1.7B-Q8_0.gguf".to_string(),
                    capability_tags: vec![
                        "assistant_input_parser".to_string(),
                        "k3_nsp".to_string(),
                        "semantic_router".to_string(),
                    ],
                    cost_policy: json!({}),
                    status: ModelEndpointStatus::Active,
                    metadata: json!({
                        "builtin": false,
                        "local_only": true,
                        "mock_text": "{\"decision\":\"status\",\"confidence\":0.95}",
                    }),
                },
            ],
            route_policies: vec![ModelRoutePolicy {
                route_policy_id: "semantic.router".to_string(),
                workspace_id: "home-1".to_string(),
                domain_scope: "semantic".to_string(),
                modality: "text".to_string(),
                privacy_level: PrivacyLevel::StrictLocal,
                local_preferred: true,
                max_cost_per_run: None,
                fallback_order: vec!["local".to_string(), "sidecar".to_string()],
                status: "active".to_string(),
                metadata: json!({"capability": "router", "local_only": true}),
            }],
            ..AdminModelCenterState::default()
        };

        let result = run_llm_text_with_state_and_options(
            "家里入口现在状态正常吗",
            &state,
            &LlmTextOptions {
                purpose: Some("semantic.router".to_string()),
                ..Default::default()
            },
        );

        assert!(result.available);
        assert_eq!(
            result.model_endpoint_id.as_deref(),
            Some("zz-k3-nsp-router")
        );
        assert_eq!(
            result.details["selected_endpoint"],
            json!("zz-k3-nsp-router")
        );
        assert_eq!(
            result.details["attempted_endpoints"],
            json!(["zz-k3-nsp-router"])
        );
    }

    #[test]
    fn strict_local_route_policy_blocks_cloud_llm_endpoint() {
        let state = AdminModelCenterState {
            endpoints: vec![ModelEndpoint {
                model_endpoint_id: "llm-cloud-siliconflow".to_string(),
                workspace_id: Some("home-1".to_string()),
                provider_account_id: None,
                model_kind: ModelKind::Llm,
                endpoint_kind: ModelEndpointKind::Cloud,
                provider_key: "openai_compatible".to_string(),
                model_name: "deepseek-ai/DeepSeek-V4-Flash".to_string(),
                capability_tags: vec!["chat".to_string(), "cloud_fallback".to_string()],
                cost_policy: json!({}),
                status: ModelEndpointStatus::Active,
                metadata: json!({
                    "base_url": "https://api.siliconflow.cn/v1",
                    "api_key": "configured",
                    "mock_text": "rag_answer",
                }),
            }],
            route_policies: vec![ModelRoutePolicy {
                route_policy_id: "retrieval.answer".to_string(),
                workspace_id: "home-1".to_string(),
                domain_scope: "retrieval".to_string(),
                modality: "text".to_string(),
                privacy_level: PrivacyLevel::StrictLocal,
                local_preferred: true,
                max_cost_per_run: None,
                fallback_order: vec!["cloud".to_string()],
                status: "active".to_string(),
                metadata: json!({}),
            }],
            ..AdminModelCenterState::default()
        };

        let result = run_llm_text_with_state("answer locally", &state);

        assert!(!result.available);
        assert_eq!(result.status, "disabled");
        assert_eq!(result.details["attempted_endpoints"], json!([]));
    }

    #[test]
    fn run_llm_text_with_state_reuses_runtime_probe_within_ttl() {
        let _guard = MODEL_RUNTIME_ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        clear_local_runtime_projection_cache();
        let server = Server::http("127.0.0.1:0").expect("server");
        let base_url = format!("http://{}/v1", server.server_addr());
        let healthz_header =
            Header::from_bytes(b"Content-Type", b"application/json").expect("health header");
        let chat_header =
            Header::from_bytes(b"Content-Type", b"application/json").expect("chat header");

        let server_thread = thread::spawn(move || {
            for _ in 0..3 {
                let request = server.recv().expect("request");
                match (request.method(), request.url()) {
                    (&Method::Get, "/healthz") => request
                        .respond(
                            Response::from_string(
                                r#"{"ready":true,"backend":{"ready":true,"kind":"candle"},"chat_model":"Qwen/Qwen2.5-0.5B-Instruct"}"#,
                            )
                            .with_header(healthz_header.clone()),
                        )
                        .expect("health response"),
                    (&Method::Post, "/v1/chat/completions") => request
                        .respond(
                            Response::from_string(
                                r#"{"choices":[{"message":{"content":"capability_summary"}}]}"#,
                            )
                            .with_header(chat_header.clone()),
                        )
                        .expect("chat response"),
                    _ => request
                        .respond(Response::from_string("not found").with_status_code(404))
                        .expect("404 response"),
                }
            }
        });

        std::env::set_var("HARBOR_MODEL_API_BASE_URL", &base_url);
        std::env::set_var("HARBOR_MODEL_API_TOKEN", "runtime-overlay-token");
        let state = AdminModelCenterState {
            endpoints: vec![ModelEndpoint {
                model_endpoint_id: "llm-local-openai-compatible".to_string(),
                workspace_id: Some("home-1".to_string()),
                provider_account_id: None,
                model_kind: ModelKind::Llm,
                endpoint_kind: ModelEndpointKind::Local,
                provider_key: "openai_compatible".to_string(),
                model_name: "harbor-local-chat".to_string(),
                capability_tags: vec!["chat".to_string(), "local_first".to_string()],
                cost_policy: json!({}),
                status: ModelEndpointStatus::Disabled,
                metadata: json!({
                    "builtin": true,
                    "base_url": "",
                    "healthz_url": "",
                    "api_key": "",
                    "api_key_configured": false,
                }),
            }],
            route_policies: vec![ModelRoutePolicy {
                route_policy_id: "retrieval.answer".to_string(),
                workspace_id: "home-1".to_string(),
                domain_scope: "retrieval".to_string(),
                modality: "text".to_string(),
                privacy_level: PrivacyLevel::AllowRedactedCloud,
                local_preferred: true,
                max_cost_per_run: None,
                fallback_order: vec!["local".to_string(), "cloud".to_string()],
                status: "active".to_string(),
                metadata: json!({}),
            }],
            ..AdminModelCenterState::default()
        };

        let first = run_llm_text_with_state("摄像头能干什么", &state);
        let second = run_llm_text_with_state("再说一遍", &state);

        std::env::remove_var("HARBOR_MODEL_API_BASE_URL");
        std::env::remove_var("HARBOR_MODEL_API_TOKEN");
        clear_local_runtime_projection_cache();
        server_thread.join().expect("server thread");

        assert!(first.available);
        assert!(second.available);
    }
}
