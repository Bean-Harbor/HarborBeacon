//! Beacon-owned routing projections for admin diagnostics.

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::control_plane::models::{
    ModelEndpoint, ModelEndpointKind, ModelEndpointStatus, ModelRoutePolicy, PrivacyLevel,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingRuntimeProjection {
    pub runtime_id: String,
    pub status: String,
    pub enabled: bool,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainRouteProjection {
    pub domain_id: String,
    pub display_name: String,
    pub owner_lane: String,
    pub preferred_order: Vec<String>,
    pub boundary: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EndpointKindCounts {
    pub local: usize,
    pub sidecar: usize,
    pub cloud: usize,
    pub disabled: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRoutePolicyProjection {
    pub route_policy_id: String,
    pub domain_scope: String,
    pub modality: String,
    pub privacy_level: PrivacyLevel,
    pub local_preferred: bool,
    pub fallback_order: Vec<String>,
    pub status: String,
    pub cloud_allowed: bool,
    pub cloud_fallback_allowed: bool,
    pub endpoint_counts: EndpointKindCounts,
    pub selected_endpoint_id: Option<String>,
    pub selected_endpoint_kind: Option<String>,
    pub blockers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingCapabilityProjection {
    pub capability_id: String,
    pub route_policy_id: String,
    pub readiness: String,
    pub local_only: bool,
    pub selected_endpoint_id: Option<String>,
    pub blockers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingBoundaryProjection {
    pub boundary_id: String,
    pub owner_lane: String,
    pub status: String,
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoutingStatus {
    pub kind: String,
    pub generated_at: String,
    pub metadata_only: bool,
    pub scope: String,
    pub execution_routes: Vec<DomainRouteProjection>,
    pub model_route_policies: Vec<ModelRoutePolicyProjection>,
    pub capability_readiness: Vec<RoutingCapabilityProjection>,
    pub runtimes: Vec<RoutingRuntimeProjection>,
    pub boundaries: Vec<RoutingBoundaryProjection>,
    pub fallback_blockers: Vec<String>,
    pub secret_scan: String,
}

pub fn build_routing_status(
    endpoints: &[ModelEndpoint],
    policies: &[ModelRoutePolicy],
    runtimes: &[RoutingRuntimeProjection],
    generated_at: impl Into<String>,
) -> RoutingStatus {
    let model_route_policies = policies
        .iter()
        .map(|policy| project_model_route_policy(policy, endpoints))
        .collect::<Vec<_>>();
    let capability_readiness = build_capability_readiness(&model_route_policies);
    let fallback_blockers = model_route_policies
        .iter()
        .flat_map(|policy| policy.blockers.iter().cloned())
        .collect::<Vec<_>>();

    RoutingStatus {
        kind: "harborbeacon.routing_status.v1".to_string(),
        generated_at: generated_at.into(),
        metadata_only: true,
        scope: "beacon_internal_orchestration".to_string(),
        execution_routes: default_execution_routes(),
        model_route_policies,
        capability_readiness,
        runtimes: runtimes.to_vec(),
        boundaries: default_routing_boundaries(),
        fallback_blockers,
        secret_scan: "clean".to_string(),
    }
}

pub fn default_execution_routes() -> Vec<DomainRouteProjection> {
    vec![
        DomainRouteProjection {
            domain_id: "harboros_system".to_string(),
            display_name: "HarborOS System Domain".to_string(),
            owner_lane: "harbor-hos-control".to_string(),
            preferred_order: vec![
                "middleware_api".to_string(),
                "midcli".to_string(),
                "browser".to_string(),
                "mcp".to_string(),
            ],
            boundary: "system control stays separate from AIoT device-native control".to_string(),
            status: "active".to_string(),
        },
        DomainRouteProjection {
            domain_id: "home_device".to_string(),
            display_name: "Home Device Domain".to_string(),
            owner_lane: "harbor-aiot".to_string(),
            preferred_order: vec![
                "native_adapter".to_string(),
                "lan_bridge".to_string(),
                "harboros_connector".to_string(),
                "cloud_or_mcp".to_string(),
            ],
            boundary: "device-native work does not default to HarborOS CLI".to_string(),
            status: "active".to_string(),
        },
    ]
}

fn default_routing_boundaries() -> Vec<RoutingBoundaryProjection> {
    vec![
        RoutingBoundaryProjection {
            boundary_id: "im_gateway_route_registry".to_string(),
            owner_lane: "harbor-im-gateway".to_string(),
            status: "external".to_string(),
            note: "Beacon treats transport route handles as opaque delivery metadata.".to_string(),
        },
        RoutingBoundaryProjection {
            boundary_id: "beacon_business_routing".to_string(),
            owner_lane: "harbor-framework".to_string(),
            status: "beacon_owned".to_string(),
            note:
                "Planner, model route policy, approval, artifact, and audit truth stay in Beacon."
                    .to_string(),
        },
    ]
}

fn project_model_route_policy(
    policy: &ModelRoutePolicy,
    endpoints: &[ModelEndpoint],
) -> ModelRoutePolicyProjection {
    let cloud_allowed = policy.privacy_level != PrivacyLevel::StrictLocal;
    let cloud_fallback_allowed = cloud_allowed
        && policy
            .fallback_order
            .iter()
            .any(|kind| kind.eq_ignore_ascii_case("cloud"));
    let candidates = model_route_candidates(policy, endpoints);
    let selected = candidates.first();
    let mut blockers = Vec::new();
    if candidates.is_empty() {
        blockers.push(format!(
            "route_policy={} has no enabled endpoint candidates",
            policy.route_policy_id
        ));
    }
    if policy.route_policy_id == "semantic.router" && cloud_fallback_allowed {
        blockers.push("semantic.router must remain local-only".to_string());
    }

    ModelRoutePolicyProjection {
        route_policy_id: policy.route_policy_id.clone(),
        domain_scope: policy.domain_scope.clone(),
        modality: policy.modality.clone(),
        privacy_level: policy.privacy_level,
        local_preferred: policy.local_preferred,
        fallback_order: normalized_fallback_order(policy),
        status: policy.status.clone(),
        cloud_allowed,
        cloud_fallback_allowed,
        endpoint_counts: endpoint_kind_counts(policy, endpoints),
        selected_endpoint_id: selected.map(|endpoint| endpoint.model_endpoint_id.clone()),
        selected_endpoint_kind: selected
            .map(|endpoint| endpoint.endpoint_kind.as_str().to_string()),
        blockers,
    }
}

fn model_route_candidates<'a>(
    policy: &ModelRoutePolicy,
    endpoints: &'a [ModelEndpoint],
) -> Vec<&'a ModelEndpoint> {
    let cloud_allowed = policy.privacy_level != PrivacyLevel::StrictLocal;
    let mut candidates = endpoints
        .iter()
        .filter(|endpoint| endpoint.status != ModelEndpointStatus::Disabled)
        .filter(|endpoint| cloud_allowed || endpoint.endpoint_kind != ModelEndpointKind::Cloud)
        .filter(|endpoint| endpoint_matches_policy(endpoint, policy))
        .collect::<Vec<_>>();
    let fallback_order = normalized_fallback_order(policy);
    candidates.sort_by(|left, right| {
        endpoint_order(left, &fallback_order)
            .cmp(&endpoint_order(right, &fallback_order))
            .then(endpoint_status_order(left.status).cmp(&endpoint_status_order(right.status)))
            .then(left.model_endpoint_id.cmp(&right.model_endpoint_id))
    });
    candidates
}

fn endpoint_matches_policy(endpoint: &ModelEndpoint, policy: &ModelRoutePolicy) -> bool {
    let policy_key = policy.route_policy_id.replace('.', "_");
    if endpoint
        .capability_tags
        .iter()
        .any(|tag| tag.eq_ignore_ascii_case(&policy.route_policy_id) || tag == &policy_key)
    {
        return true;
    }
    if let Some(capability) = policy
        .metadata
        .get("capability")
        .and_then(|value| value.as_str())
    {
        return endpoint
            .capability_tags
            .iter()
            .any(|tag| tag.eq_ignore_ascii_case(capability));
    }
    endpoint
        .model_kind
        .as_str()
        .eq_ignore_ascii_case(&policy.modality)
        || policy.modality.eq_ignore_ascii_case("text")
            && endpoint.model_kind.as_str().eq_ignore_ascii_case("llm")
        || policy.modality.eq_ignore_ascii_case("multimodal")
            && matches!(endpoint.model_kind.as_str(), "llm" | "vlm")
}

fn endpoint_kind_counts(
    policy: &ModelRoutePolicy,
    endpoints: &[ModelEndpoint],
) -> EndpointKindCounts {
    let mut counts = EndpointKindCounts::default();
    for endpoint in endpoints
        .iter()
        .filter(|endpoint| endpoint_matches_policy(endpoint, policy))
    {
        if endpoint.status == ModelEndpointStatus::Disabled {
            counts.disabled += 1;
            continue;
        }
        match endpoint.endpoint_kind {
            ModelEndpointKind::Local => counts.local += 1,
            ModelEndpointKind::Sidecar => counts.sidecar += 1,
            ModelEndpointKind::Cloud => counts.cloud += 1,
        }
    }
    counts
}

fn normalized_fallback_order(policy: &ModelRoutePolicy) -> Vec<String> {
    if policy.fallback_order.is_empty() {
        return vec![
            "local".to_string(),
            "sidecar".to_string(),
            "cloud".to_string(),
        ];
    }
    policy
        .fallback_order
        .iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

fn endpoint_order(endpoint: &ModelEndpoint, fallback_order: &[String]) -> usize {
    fallback_order
        .iter()
        .position(|kind| kind == endpoint.endpoint_kind.as_str())
        .unwrap_or(fallback_order.len())
}

fn endpoint_status_order(status: ModelEndpointStatus) -> usize {
    match status {
        ModelEndpointStatus::Active => 0,
        ModelEndpointStatus::Degraded => 1,
        ModelEndpointStatus::Disabled => 2,
    }
}

fn build_capability_readiness(
    policies: &[ModelRoutePolicyProjection],
) -> Vec<RoutingCapabilityProjection> {
    policies
        .iter()
        .map(|policy| {
            let local_only = !policy.cloud_allowed || !policy.cloud_fallback_allowed;
            let readiness = if policy.selected_endpoint_id.is_some() && policy.blockers.is_empty() {
                "ready"
            } else if policy.selected_endpoint_id.is_some() {
                "degraded"
            } else {
                "blocked"
            };
            RoutingCapabilityProjection {
                capability_id: capability_id_for_policy(policy),
                route_policy_id: policy.route_policy_id.clone(),
                readiness: readiness.to_string(),
                local_only,
                selected_endpoint_id: policy.selected_endpoint_id.clone(),
                blockers: policy.blockers.clone(),
            }
        })
        .collect()
}

fn capability_id_for_policy(policy: &ModelRoutePolicyProjection) -> String {
    match policy.route_policy_id.as_str() {
        "semantic.router" => "semantic_router".to_string(),
        "retrieval.answer" => "retrieval_answer".to_string(),
        "retrieval.embed" => "retrieval_embed".to_string(),
        "retrieval.ocr" => "retrieval_ocr".to_string(),
        "retrieval.vision_summary" => "retrieval_vision_summary".to_string(),
        other => other.replace('.', "_"),
    }
}

pub fn routing_status_json(status: &RoutingStatus) -> serde_json::Value {
    json!(status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane::models::{ModelEndpoint, ModelKind};

    fn endpoint(id: &str, kind: ModelEndpointKind, tags: &[&str]) -> ModelEndpoint {
        ModelEndpoint {
            model_endpoint_id: id.to_string(),
            workspace_id: Some("home-1".to_string()),
            provider_account_id: None,
            model_kind: ModelKind::Llm,
            endpoint_kind: kind,
            provider_key: "openai_compatible".to_string(),
            model_name: "qwen".to_string(),
            capability_tags: tags.iter().map(|tag| tag.to_string()).collect(),
            cost_policy: json!({}),
            status: ModelEndpointStatus::Active,
            metadata: json!({}),
        }
    }

    fn policy(
        id: &str,
        privacy_level: PrivacyLevel,
        fallback_order: Vec<&str>,
    ) -> ModelRoutePolicy {
        ModelRoutePolicy {
            route_policy_id: id.to_string(),
            workspace_id: "home-1".to_string(),
            domain_scope: "assistant".to_string(),
            modality: "text".to_string(),
            privacy_level,
            local_preferred: true,
            max_cost_per_run: None,
            fallback_order: fallback_order.into_iter().map(str::to_string).collect(),
            status: "active".to_string(),
            metadata: json!({"capability": "semantic_router"}),
        }
    }

    #[test]
    fn routing_status_keeps_gate_route_registry_external() {
        let status = build_routing_status(&[], &[], &[], "1700000000");
        let text = serde_json::to_string(&status).expect("routing json");
        assert!(text.contains("im_gateway_route_registry"));
        assert!(!text.contains("gw_route_"));
        assert_eq!(status.scope, "beacon_internal_orchestration");
    }

    #[test]
    fn semantic_router_policy_prefers_local_endpoint_and_blocks_cloud() {
        let endpoints = vec![
            endpoint(
                "cloud-router",
                ModelEndpointKind::Cloud,
                &["semantic_router"],
            ),
            endpoint(
                "local-router",
                ModelEndpointKind::Local,
                &["semantic_router"],
            ),
        ];
        let policies = vec![policy(
            "semantic.router",
            PrivacyLevel::StrictLocal,
            vec!["cloud", "local"],
        )];
        let status = build_routing_status(&endpoints, &policies, &[], "1700000000");
        let router = status
            .model_route_policies
            .iter()
            .find(|policy| policy.route_policy_id == "semantic.router")
            .expect("router policy");
        assert_eq!(router.selected_endpoint_id.as_deref(), Some("local-router"));
        assert!(!router.cloud_allowed);
        assert_eq!(router.endpoint_counts.cloud, 1);
    }
}
