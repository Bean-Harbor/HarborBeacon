use super::*;
use harborbeacon_local_agent::runtime::fixed_models;

pub(super) fn build_model_capabilities_response(
    model_state: &AdminModelCenterState,
    endpoints: &[ModelEndpoint],
    _policies: &[ModelRoutePolicy],
    _downloads: Vec<ModelDownloadJobRecord>,
    runtime: &LocalModelRuntimeProjection,
) -> ModelCapabilitiesResponse {
    let mut capabilities = Vec::new();
    for (id, label, kind, endpoint_id, model, loaded) in [
        (
            "semantic_router",
            "问题理解",
            "llm",
            fixed_models::CHAT_ENDPOINT,
            fixed_models::CHAT_MODEL,
            runtime.chat_model_loaded,
        ),
        (
            "embedder",
            "向量检索",
            "embedder",
            fixed_models::EMBEDDING_ENDPOINT,
            fixed_models::EMBEDDING_MODEL,
            runtime.embedding_model_loaded,
        ),
        (
            "retrieval_answer",
            "对话回答",
            "llm",
            fixed_models::CHAT_ENDPOINT,
            fixed_models::CHAT_MODEL,
            runtime.chat_model_loaded,
        ),
    ] {
        let ready = loaded == Some(true);
        let status = if ready { "ready" } else { "degraded" };
        capabilities.push(ModelCapabilityStatus {
            capability_id: id.to_string(),
            label: label.to_string(),
            model_kind: kind.to_string(),
            status: status.to_string(),
            desired_model_id: Some(model.to_string()),
            active_model_id: ready.then(|| model.to_string()),
            transition_status: status.to_string(),
            last_error: runtime.error.clone(),
            selected_model_id: None,
            runtime_model_id: ready.then(|| model.to_string()),
            current_model: endpoints
                .iter()
                .find(|endpoint| endpoint.model_endpoint_id == endpoint_id)
                .map(|endpoint| ModelCapabilityCurrentModel {
                    model_endpoint_id: endpoint.model_endpoint_id.clone(),
                    model_name: model.to_string(),
                    provider_key: endpoint.provider_key.clone(),
                    status: status.to_string(),
                }),
            installed_models: Vec::new(),
            installable_models: Vec::new(),
            download_jobs: Vec::new(),
            next_action: if ready { "" } else { "retry" }.to_string(),
            runtime_ready: ready,
            required_runtime_profile: Some("n2-fixed".to_string()),
            runtime_installed: runtime.ready,
            runtime_installable: false,
            runtime_status: Some(status.to_string()),
            runtime_next_action: None,
            source_of_truth: "official_model_manifest".to_string(),
            evidence: vec![format!("execution_identity={model}")],
        });
    }
    ModelCapabilitiesResponse {
        local_model_policy: "fixed",
        generated_at: now_unix_string(),
        checked_at: now_unix_string(),
        status: if capabilities.iter().all(|c| c.runtime_ready) {
            "ready"
        } else {
            "degraded"
        }
        .to_string(),
        model_store: build_model_store_status(model_state),
        runtime_manager: build_model_runtime_manager_response(model_state, runtime),
        capabilities,
        blockers: runtime.error.clone().into_iter().collect(),
        warnings: Vec::new(),
    }
}
