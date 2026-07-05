//! Model registry entries and local-first / cloud-augmentation routing schemas.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    #[default]
    Llm,
    Vlm,
    Ocr,
    Asr,
    Detector,
    Embedder,
    Reranker,
}

impl ModelKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Llm => "llm",
            Self::Vlm => "vlm",
            Self::Ocr => "ocr",
            Self::Asr => "asr",
            Self::Detector => "detector",
            Self::Embedder => "embedder",
            Self::Reranker => "reranker",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModelEndpointKind {
    #[default]
    Local,
    Sidecar,
    Cloud,
}

impl ModelEndpointKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Sidecar => "sidecar",
            Self::Cloud => "cloud",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ModelEndpointStatus {
    #[default]
    Active,
    Degraded,
    Disabled,
}

impl ModelEndpointStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Degraded => "degraded",
            Self::Disabled => "disabled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ModelEndpoint {
    pub model_endpoint_id: String,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub provider_account_id: Option<String>,
    pub model_kind: ModelKind,
    pub endpoint_kind: ModelEndpointKind,
    pub provider_key: String,
    pub model_name: String,
    #[serde(default)]
    pub capability_tags: Vec<String>,
    #[serde(default)]
    pub cost_policy: Value,
    pub status: ModelEndpointStatus,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PrivacyLevel {
    #[default]
    StrictLocal,
    AllowRedactedCloud,
    AllowCloud,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ModelRoutePolicy {
    pub route_policy_id: String,
    pub workspace_id: String,
    pub domain_scope: String,
    pub modality: String,
    pub privacy_level: PrivacyLevel,
    #[serde(default)]
    pub local_preferred: bool,
    #[serde(default)]
    pub max_cost_per_run: Option<f64>,
    #[serde(default)]
    pub fallback_order: Vec<String>,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct PrivacyTransformRecord {
    pub privacy_transform_id: String,
    pub workspace_id: String,
    pub source_kind: String,
    pub source_ref: String,
    #[serde(default)]
    pub transform_steps: Value,
    #[serde(default)]
    pub output_ref: Value,
    #[serde(default)]
    pub policy_version: String,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InferenceRunStatus {
    #[default]
    Queued,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InferenceExecutionMode {
    #[default]
    Local,
    Cloud,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct InferenceRun {
    pub inference_run_id: String,
    pub workspace_id: String,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub trigger_event_id: Option<String>,
    pub model_endpoint_id: String,
    pub route_policy_id: String,
    pub execution_mode: InferenceExecutionMode,
    #[serde(default)]
    pub privacy_transform_id: Option<String>,
    pub status: InferenceRunStatus,
    #[serde(default)]
    pub input_ref: Value,
    #[serde(default)]
    pub output_ref: Value,
    #[serde(default)]
    pub ledger_id: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub ended_at: Option<String>,
}
