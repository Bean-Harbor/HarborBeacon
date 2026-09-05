//! Product-owned local model identities and the N2 configuration boundary.

use serde_json::json;

use crate::control_plane::models::{
    ModelEndpoint, ModelEndpointKind, ModelEndpointStatus, ModelKind, ModelRoutePolicy,
    PrivacyLevel,
};
use crate::runtime::admin_console::AdminModelCenterState;

#[cfg(all(feature = "fixed-local-models", feature = "local-model-management"))]
compile_error!("fixed-local-models and local-model-management are mutually exclusive");
#[cfg(all(feature = "fixed-local-models", feature = "embedded-model-runtime"))]
compile_error!("fixed-local-models must not include the embedded Candle runtime");

pub const FIXED: bool = cfg!(feature = "fixed-local-models");
pub const LOCAL_MODELS_FIXED: &str = "LOCAL_MODELS_FIXED";
pub const CHAT_MODEL: &str = "Qwen2.5-1.5B-Instruct-Q4_K_M";
pub const CHAT_SHA256: &str = "6a1a2eb6d15622bf3c96857206351ba97e1af16c30d7a74ee38970e434e9407e";
pub const EMBEDDING_MODEL: &str = "jina-v2-base-zh-998b913-onnx-fp32-mean-l2-v1";
pub const EMBEDDING_SHA256: &str =
    "4b0e9fa6e5c77cff56e0c9c673ba1aad61e793e592fdd4b05690b68826b7d3a2";
pub const TOKENIZER_SHA256: &str =
    "0046da43cc8c424b317f56b092b0512aaaa65c4f925d2f16af9d9eeb4d0ef902";
pub const CHAT_ENDPOINT: &str = "llm-local-openai-compatible";
pub const EMBEDDING_ENDPOINT: &str = "embed-local-openai-compatible";
pub const INFERENCE_BASE: &str = "http://127.0.0.1:4174/api/inference/v1";

pub fn policy() -> &'static str {
    if FIXED {
        "fixed"
    } else {
        "configurable"
    }
}

pub fn is_fixed_endpoint(endpoint: &ModelEndpoint) -> bool {
    FIXED
        && matches!(
            endpoint.model_endpoint_id.as_str(),
            CHAT_ENDPOINT | EMBEDDING_ENDPOINT
        )
        && endpoint.endpoint_kind == ModelEndpointKind::Local
}

pub fn validate_endpoint_write(
    existing: Option<&ModelEndpoint>,
    incoming: &ModelEndpoint,
) -> Result<(), String> {
    if !FIXED {
        return Ok(());
    }
    if incoming.endpoint_kind != ModelEndpointKind::Cloud
        || incoming.model_kind != ModelKind::Llm
        || matches!(
            incoming.model_endpoint_id.as_str(),
            CHAT_ENDPOINT | EMBEDDING_ENDPOINT
        )
        || existing.is_some_and(|item| item.endpoint_kind != ModelEndpointKind::Cloud)
    {
        return Err(LOCAL_MODELS_FIXED.to_string());
    }
    // Cloud entries cannot point back into a local inference process.
    for key in ["base_url", "healthz_url"] {
        if let Some(raw) = incoming.metadata.get(key).and_then(|value| value.as_str()) {
            let url = reqwest::Url::parse(raw).map_err(|_| "invalid cloud URL".to_string())?;
            if url.scheme() != "https"
                || url.host_str().is_none_or(|host| {
                    host == "localhost"
                        || host.ends_with(".localhost")
                        || host.parse::<std::net::IpAddr>().is_ok()
                })
            {
                return Err(LOCAL_MODELS_FIXED.to_string());
            }
        }
    }
    Ok(())
}

pub fn fixed_answer_policy(mut policy: ModelRoutePolicy) -> ModelRoutePolicy {
    policy.workspace_id = "home-1".into();
    policy.domain_scope = "retrieval".into();
    policy.modality = "text".into();
    policy.metadata = json!({"capability": "answer"});
    policy.local_preferred = true;
    policy.status = "active".into();
    policy.fallback_order = vec!["local".into(), "sidecar".into()];
    if policy.privacy_level != PrivacyLevel::StrictLocal {
        policy.fallback_order.push("cloud".into());
    }
    policy
}

pub fn validate_answer_policy(policy: &ModelRoutePolicy) -> Result<(), String> {
    let fixed = fixed_answer_policy(policy.clone());
    if policy.workspace_id != fixed.workspace_id
        || policy.domain_scope != fixed.domain_scope
        || policy.modality != fixed.modality
        || policy.local_preferred != fixed.local_preferred
        || policy.status != fixed.status
        || policy.fallback_order != fixed.fallback_order
    {
        return Err(LOCAL_MODELS_FIXED.into());
    }
    Ok(())
}

pub fn project(mut state: AdminModelCenterState) -> AdminModelCenterState {
    state.endpoints.retain(|endpoint| {
        endpoint.endpoint_kind == ModelEndpointKind::Cloud
            && validate_endpoint_write(None, endpoint).is_ok()
    });
    let token = std::env::var("HARBOR_MODEL_API_TOKEN").unwrap_or_default();
    for (id, kind, model, sha, tags) in [
        (
            CHAT_ENDPOINT,
            ModelKind::Llm,
            CHAT_MODEL,
            CHAT_SHA256,
            vec![
                "chat",
                "semantic_router",
                "assistant_input_parser",
                "local_first",
            ],
        ),
        (
            EMBEDDING_ENDPOINT,
            ModelKind::Embedder,
            EMBEDDING_MODEL,
            EMBEDDING_SHA256,
            vec!["embeddings", "local_first"],
        ),
    ] {
        state.endpoints.push(ModelEndpoint {
            model_endpoint_id: id.to_string(),
            workspace_id: Some("home-1".to_string()),
            provider_account_id: None,
            model_kind: kind,
            endpoint_kind: ModelEndpointKind::Local,
            provider_key: "openai_compatible".to_string(),
            model_name: model.to_string(),
            capability_tags: tags.into_iter().map(str::to_string).collect(),
            cost_policy: json!({"cost_hint": "local"}),
            status: ModelEndpointStatus::Active,
            metadata: json!({
                "builtin": true, "local_model_policy": "fixed", "model": model,
                "base_url": INFERENCE_BASE,
                "healthz_url": "http://127.0.0.1:4174/api/inference/healthz",
                "api_key": token, "api_key_required": true, "api_key_configured": !token.is_empty(),
                "sha256": sha, "lease_owner": "beacon_inference", "local_only": true,
                "cloud_fallback_allowed": false,
            }),
        });
    }
    state.model_store_root = "/data/models/current".to_string();
    state.endpoints.extend(
        crate::runtime::admin_console::default_model_endpoints()
            .into_iter()
            .filter(|endpoint| endpoint.model_kind == ModelKind::Ocr),
    );
    state.capability_bindings.clear();
    state.runtimes.clear();
    let answer_policy = state
        .route_policies
        .iter()
        .find(|policy| policy.route_policy_id == "retrieval.answer")
        .cloned();
    state.route_policies = crate::runtime::admin_console::default_model_route_policies();
    state
        .route_policies
        .retain(|policy| policy.route_policy_id != "retrieval.vision_summary");
    let answer_policy = answer_policy.or_else(|| {
        state
            .route_policies
            .iter()
            .find(|policy| policy.route_policy_id == "retrieval.answer")
            .cloned()
    });
    if let Some(policy) = answer_policy {
        state
            .route_policies
            .retain(|item| item.route_policy_id != "retrieval.answer");
        state.route_policies.push(fixed_answer_policy(policy));
    }
    state
}

#[cfg(all(test, feature = "fixed-local-models"))]
mod tests {
    use super::*;

    #[test]
    fn cloud_only_answer_policy_is_rejected_and_old_routes_are_projected_local_first() {
        let mut state = AdminModelCenterState::default();
        let policy = state
            .route_policies
            .iter_mut()
            .find(|policy| policy.route_policy_id == "retrieval.answer")
            .unwrap();
        policy.privacy_level = PrivacyLevel::AllowRedactedCloud;
        policy.local_preferred = false;
        policy.fallback_order = vec!["cloud".into()];
        assert_eq!(
            validate_answer_policy(policy).unwrap_err(),
            LOCAL_MODELS_FIXED
        );
        let projected = project(state);
        let policy = projected
            .route_policies
            .iter()
            .find(|policy| policy.route_policy_id == "retrieval.answer")
            .unwrap();
        assert!(policy.local_preferred);
        assert_eq!(policy.fallback_order, vec!["local", "sidecar", "cloud"]);
        assert_eq!(policy.privacy_level, PrivacyLevel::AllowRedactedCloud);
        assert!(validate_answer_policy(policy).is_ok());
    }

    #[test]
    fn persisted_state_rejects_local_edits_and_preserves_cloud_credentials_and_backup() {
        use crate::runtime::admin_console::AdminConsoleStore;
        use crate::runtime::registry::DeviceRegistryStore;
        let root = std::env::temp_dir().join(format!("n2-fixed-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let path = root.join("admin.json");
        let store = AdminConsoleStore::new(
            path.clone(),
            DeviceRegistryStore::new(root.join("registry.json")),
        );
        let mut state = store.load_or_create_state().unwrap();
        state
            .models
            .endpoints
            .iter_mut()
            .find(|item| item.model_endpoint_id == CHAT_ENDPOINT)
            .unwrap()
            .model_name = "legacy-user-choice".into();
        state.models.model_store_root = "/legacy/downloads".into();
        let original = serde_json::to_vec_pretty(&state).unwrap();
        std::fs::write(&path, &original).unwrap();
        assert_eq!(
            store
                .load_state()
                .unwrap()
                .models
                .endpoints
                .iter()
                .find(|item| item.model_endpoint_id == CHAT_ENDPOINT)
                .unwrap()
                .model_name,
            CHAT_MODEL
        );
        for patch in [
            json!({"model_name":"replacement"}),
            json!({"endpoint_kind":"cloud"}),
            json!({"metadata":{"base_url":"http://127.0.0.1:9999/v1"}}),
        ] {
            assert_eq!(
                store
                    .patch_model_endpoint(CHAT_ENDPOINT, patch)
                    .unwrap_err(),
                LOCAL_MODELS_FIXED
            );
        }
        let cloud = ModelEndpoint {
            model_endpoint_id: "cloud-custom".into(),
            workspace_id: Some("home-1".into()),
            model_kind: ModelKind::Llm,
            endpoint_kind: ModelEndpointKind::Cloud,
            provider_key: "openai_compatible".into(),
            model_name: "cloud-model".into(),
            status: ModelEndpointStatus::Active,
            metadata: json!({"base_url":"https://api.example.com/v1","api_key":"retained-cloud-secret"}),
            ..Default::default()
        };
        store.save_model_endpoint(cloud).unwrap();
        store
            .patch_model_endpoint("cloud-custom", json!({"metadata":{"api_key":""}}))
            .unwrap();
        let saved = store.load_state().unwrap();
        assert_eq!(
            saved
                .models
                .endpoints
                .iter()
                .find(|item| item.model_endpoint_id == "cloud-custom")
                .unwrap()
                .metadata["api_key"],
            "retained-cloud-secret"
        );
        assert_eq!(
            std::fs::read(path.with_extension("pre-fixed-models.json")).unwrap(),
            original
        );
        assert!(store.list_model_download_jobs().unwrap().is_empty());
        let mut policies = saved.models.route_policies;
        policies
            .iter_mut()
            .find(|item| item.route_policy_id == "semantic.router")
            .unwrap()
            .fallback_order = vec!["cloud".into()];
        assert_eq!(
            store.save_model_route_policies(policies).unwrap_err(),
            LOCAL_MODELS_FIXED
        );
        assert!(store
            .patch_model_endpoint("cloud-custom", json!({"endpoint_kind":"local"}))
            .is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn persisted_local_configuration_cannot_replace_official_models() {
        let state = project(AdminModelCenterState::default());
        let original = state
            .endpoints
            .iter()
            .find(|e| e.model_endpoint_id == CHAT_ENDPOINT)
            .unwrap();
        let mut forged = original.clone();
        forged.endpoint_kind = ModelEndpointKind::Cloud;
        forged.model_name = "other".into();
        assert_eq!(
            validate_endpoint_write(Some(original), &forged).unwrap_err(),
            LOCAL_MODELS_FIXED
        );
        let projected = project(AdminModelCenterState {
            endpoints: vec![forged],
            ..state
        });
        assert_eq!(
            projected
                .endpoints
                .iter()
                .find(|e| e.model_endpoint_id == CHAT_ENDPOINT)
                .unwrap()
                .model_name,
            CHAT_MODEL
        );
        assert!(projected.capability_bindings.is_empty());
    }

    #[test]
    fn cloud_settings_remain_editable_without_local_type_conversion() {
        let mut cloud = ModelEndpoint {
            model_endpoint_id: "cloud-custom".into(),
            endpoint_kind: ModelEndpointKind::Cloud,
            model_kind: ModelKind::Llm,
            metadata: json!({"base_url": "https://api.example.com/v1", "api_key": "user-secret"}),
            ..Default::default()
        };
        assert!(validate_endpoint_write(None, &cloud).is_ok());
        cloud.metadata["base_url"] = json!("http://127.0.0.1:8793/v1");
        assert!(validate_endpoint_write(None, &cloud).is_err());
        cloud.endpoint_kind = ModelEndpointKind::Sidecar;
        assert!(validate_endpoint_write(None, &cloud).is_err());
    }
}
