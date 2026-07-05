//! Harbor app manifest and control-plane contract.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const HARBOR_APP_CONTRACT_VERSION: &str = "harbor.app.v1";
pub const DEFAULT_APP_COMPOSE_ROOT: &str = "/var/lib/harbor/apps";
pub const DEFAULT_APP_DATA_ROOT: &str = "/mnt/software/harbor-apps";
pub const DEFAULT_APP_ENV_ROOT: &str = "/etc/harbor/apps";

pub const APP_MANAGER_ENDPOINTS: &[&str] = &[
    "GET /api/apps",
    "POST /api/apps/install",
    "POST /api/apps/{id}/start",
    "POST /api/apps/{id}/stop",
    "POST /api/apps/{id}/restart",
    "GET /api/apps/{id}/health",
    "GET /api/apps/{id}/logs",
    "POST /api/apps/{id}/exposure",
];

pub const PLATFORM_CAPABILITY_ENDPOINTS: &[(&str, &str)] = &[
    ("platform.nsp.plan", "POST /api/platform/nsp/plan"),
    ("platform.router.route", "POST /api/platform/router/route"),
    (
        "platform.privacy.redact",
        "POST /api/platform/privacy/redact",
    ),
    (
        "platform.compliance.evaluate",
        "POST /api/platform/compliance/evaluate",
    ),
    ("platform.models.infer", "POST /api/platform/models/infer"),
    (
        "platform.approval.tickets.create",
        "POST /api/platform/approval/tickets",
    ),
    (
        "platform.audit.events.read",
        "GET /api/platform/audit/events",
    ),
    (
        "platform.audit.events.write",
        "POST /api/platform/audit/events",
    ),
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarborAppManifest {
    pub contract: String,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<HarborAppBuild>,
    #[serde(default)]
    pub routes: Vec<HarborAppRoute>,
    pub health: HarborAppHealth,
    #[serde(default)]
    pub permissions: Vec<HarborAppPermission>,
    #[serde(default)]
    pub volumes: Vec<HarborAppVolume>,
    #[serde(default)]
    pub platform_capabilities: Vec<String>,
    pub exposure: HarborAppExposure,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarborAppBuild {
    pub context: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dockerfile: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarborAppRoute {
    pub path_prefix: String,
    pub service_port: u16,
    #[serde(default)]
    pub strip_prefix: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarborAppHealth {
    pub path: String,
    pub port: u16,
    #[serde(default = "default_health_interval_seconds")]
    pub interval_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarborAppPermission {
    pub capability: String,
    #[serde(default)]
    pub actions: Vec<String>,
    pub risk: HarborAppRisk,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarborAppVolume {
    pub name: String,
    pub mount_path: String,
    pub kind: HarborAppVolumeKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarborAppExposure {
    None,
    Lan,
    Tunnel,
}

impl Default for HarborAppExposure {
    fn default() -> Self {
        Self::None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarborAppRisk {
    Low,
    Medium,
    High,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarborAppVolumeKind {
    Data,
    Cache,
    Config,
    Secret,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarborAppRuntimeStatus {
    Unknown,
    Registered,
    PlanReady,
    ApprovalRequired,
    Blocked,
}

impl Default for HarborAppRuntimeStatus {
    fn default() -> Self {
        Self::Unknown
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarborAppValidationReport {
    pub ok: bool,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub declared_capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarborAppPathRoots {
    pub compose_root: PathBuf,
    pub data_root: PathBuf,
    pub env_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarborAppPaths {
    pub app_root: PathBuf,
    pub compose_file: PathBuf,
    pub manifest_file: PathBuf,
    pub data_root: PathBuf,
    pub env_file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarborAppCommandPreviewSnapshot {
    pub program: String,
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub requires_approval: bool,
    pub risk: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarborAppExecutionPlanSnapshot {
    pub app_id: String,
    pub action: String,
    pub compose_project: String,
    pub route_prefixes: Vec<String>,
    pub exposure: HarborAppExposure,
    pub audit_action: String,
    pub commands: Vec<HarborAppCommandPreviewSnapshot>,
    pub command_count: usize,
    pub requires_approval: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarborAppRegistryEntry {
    pub app_id: String,
    pub name: String,
    pub version: String,
    pub exposure: HarborAppExposure,
    pub manifest: HarborAppManifest,
    pub paths: HarborAppPaths,
    pub declared_capabilities: Vec<String>,
    #[serde(default)]
    pub status: HarborAppRuntimeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_requested_action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_execution_plan: Option<HarborAppExecutionPlanSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_health_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_log_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_audit_id: Option<String>,
    pub registered_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarborAppInstallRequest {
    pub manifest: HarborAppManifest,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_note: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarborAppLifecycleRequest {
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarborAppExposureRequest {
    pub exposure: HarborAppExposure,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarborAppListResponse {
    pub apps: Vec<HarborAppRegistryEntry>,
    pub metadata_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarborAppInstallResponse {
    pub app: HarborAppRegistryEntry,
    pub dry_run: bool,
    pub validation: HarborAppValidationReport,
    pub execution_plan: HarborAppExecutionPlanSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_id: Option<String>,
    pub metadata_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarborAppLifecycleResponse {
    pub app: HarborAppRegistryEntry,
    pub action: String,
    pub status: HarborAppRuntimeStatus,
    pub dry_run: bool,
    pub approval_required: bool,
    pub message: String,
    pub execution_plan: HarborAppExecutionPlanSnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audit_id: Option<String>,
    pub metadata_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarborAppHealthResponse {
    pub app_id: String,
    pub status: HarborAppRuntimeStatus,
    pub message: String,
    pub execution_plan: HarborAppExecutionPlanSnapshot,
    pub metadata_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarborAppLogsResponse {
    pub app_id: String,
    pub status: HarborAppRuntimeStatus,
    pub lines: Vec<String>,
    pub message: String,
    pub execution_plan: HarborAppExecutionPlanSnapshot,
    pub metadata_only: bool,
}

impl Default for HarborAppPathRoots {
    fn default() -> Self {
        Self {
            compose_root: PathBuf::from(DEFAULT_APP_COMPOSE_ROOT),
            data_root: PathBuf::from(DEFAULT_APP_DATA_ROOT),
            env_root: PathBuf::from(DEFAULT_APP_ENV_ROOT),
        }
    }
}

pub fn default_health_interval_seconds() -> u64 {
    30
}

pub fn parse_harbor_app_manifest_yaml(input: &str) -> Result<HarborAppManifest, String> {
    serde_yaml::from_str(input)
        .map_err(|error| format!("invalid Harbor app manifest YAML: {error}"))
}

pub fn validate_app_manifest(manifest: &HarborAppManifest) -> HarborAppValidationReport {
    let mut errors = Vec::new();
    let mut warnings = Vec::new();

    if manifest.contract != HARBOR_APP_CONTRACT_VERSION {
        errors.push(format!(
            "contract must be {HARBOR_APP_CONTRACT_VERSION}, got {}",
            manifest.contract
        ));
    }

    if !is_safe_app_id(&manifest.id) {
        errors.push("id must be lowercase ascii letters, digits, or hyphen".to_string());
    }

    if manifest.name.trim().is_empty() {
        errors.push("name is required".to_string());
    }

    if manifest.version.trim().is_empty() {
        errors.push("version is required".to_string());
    }

    match (manifest.image.as_ref(), manifest.build.as_ref()) {
        (Some(_), Some(_)) => errors.push("only one of image or build may be set".to_string()),
        (None, None) => errors.push("one of image or build is required".to_string()),
        _ => {}
    }

    if manifest.routes.is_empty() && manifest.exposure != HarborAppExposure::None {
        errors.push("lan or tunnel exposure requires at least one route".to_string());
    }

    let expected_prefix = format!("/apps/{}/", manifest.id);
    for route in &manifest.routes {
        if !route.path_prefix.starts_with(&expected_prefix) {
            errors.push(format!(
                "route {} must stay under {}",
                route.path_prefix, expected_prefix
            ));
        }
        if route.service_port == 0 {
            errors.push(format!("route {} has invalid port 0", route.path_prefix));
        }
    }

    if !manifest.health.path.starts_with('/') {
        errors.push("health.path must start with /".to_string());
    }
    if manifest.health.port == 0 {
        errors.push("health.port must not be 0".to_string());
    }
    if manifest.health.interval_seconds < 5 {
        warnings.push("health.interval_seconds below 5s may be noisy".to_string());
    }

    let supported = supported_platform_capability_set();
    let mut seen_capabilities = BTreeSet::new();
    for capability in &manifest.platform_capabilities {
        if capability == "*" {
            errors.push("wildcard platform capability is not allowed".to_string());
        }
        if is_forbidden_capability(capability) {
            errors.push(format!("{capability} is outside Harbor app scope"));
        }
        if !supported.contains(capability.as_str()) {
            warnings.push(format!(
                "{capability} is not a known v1 platform capability"
            ));
        }
        seen_capabilities.insert(capability.clone());
    }

    for permission in &manifest.permissions {
        if is_forbidden_capability(&permission.capability) {
            errors.push(format!(
                "{} is not an app-grantable permission",
                permission.capability
            ));
        }
        if !seen_capabilities.contains(&permission.capability) {
            warnings.push(format!(
                "permission {} is not listed in platform_capabilities",
                permission.capability
            ));
        }
    }

    for volume in &manifest.volumes {
        if !is_safe_volume_mount(&volume.mount_path) {
            errors.push(format!(
                "volume {} mount_path {} is outside app container scope",
                volume.name, volume.mount_path
            ));
        }
    }

    if manifest.id == "home-event-rule-bridge" || manifest.id == "ha-bridge" {
        warnings.push("HA Bridge is intentionally outside the first Harbor app batch".to_string());
    }

    let declared_capabilities = seen_capabilities.into_iter().collect::<Vec<_>>();

    HarborAppValidationReport {
        ok: errors.is_empty(),
        errors,
        warnings,
        declared_capabilities,
    }
}

pub fn harbor_app_paths(
    app_id: &str,
    roots: &HarborAppPathRoots,
) -> Result<HarborAppPaths, String> {
    if !is_safe_app_id(app_id) {
        return Err("unsafe app id".to_string());
    }
    let app_root = roots.compose_root.join(app_id);
    Ok(HarborAppPaths {
        app_root: app_root.clone(),
        compose_file: app_root.join("compose.yaml"),
        manifest_file: app_root.join("app.manifest.yaml"),
        data_root: roots.data_root.join(app_id),
        env_file: roots.env_root.join(format!("{app_id}.env")),
    })
}

pub fn build_harbor_app_registry_entry(
    manifest: HarborAppManifest,
    roots: &HarborAppPathRoots,
    registered_at: impl Into<String>,
    updated_at: impl Into<String>,
) -> Result<(HarborAppRegistryEntry, HarborAppValidationReport), String> {
    let validation = validate_app_manifest(&manifest);
    if !validation.ok {
        return Err(validation.errors.join("; "));
    }
    let paths = harbor_app_paths(&manifest.id, roots)?;
    let entry = HarborAppRegistryEntry {
        app_id: manifest.id.clone(),
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        exposure: manifest.exposure,
        manifest,
        paths,
        declared_capabilities: validation.declared_capabilities.clone(),
        status: HarborAppRuntimeStatus::Registered,
        last_requested_action: None,
        last_execution_plan: None,
        last_health_status: None,
        last_log_status: None,
        last_audit_id: None,
        registered_at: registered_at.into(),
        updated_at: updated_at.into(),
    };
    Ok((entry, validation))
}

pub fn sanitize_harbor_app_registry(
    entries: Vec<HarborAppRegistryEntry>,
    roots: &HarborAppPathRoots,
) -> Vec<HarborAppRegistryEntry> {
    let mut sanitized = Vec::new();
    let mut seen = BTreeSet::new();
    for mut entry in entries {
        if !seen.insert(entry.app_id.clone()) {
            continue;
        }
        let validation = validate_app_manifest(&entry.manifest);
        if !validation.ok {
            continue;
        }
        let Ok(paths) = harbor_app_paths(&entry.manifest.id, roots) else {
            continue;
        };
        entry.app_id = entry.manifest.id.clone();
        entry.name = entry.manifest.name.clone();
        entry.version = entry.manifest.version.clone();
        entry.exposure = entry.manifest.exposure;
        entry.paths = paths;
        entry.declared_capabilities = validation.declared_capabilities;
        if entry.registered_at.trim().is_empty() {
            entry.registered_at = entry.updated_at.clone();
        }
        if entry.updated_at.trim().is_empty() {
            entry.updated_at = entry.registered_at.clone();
        }
        sanitized.push(entry);
    }
    sanitized
}

pub fn supported_platform_capabilities() -> Vec<&'static str> {
    PLATFORM_CAPABILITY_ENDPOINTS
        .iter()
        .map(|(capability, _)| *capability)
        .collect()
}

fn supported_platform_capability_set() -> BTreeSet<&'static str> {
    supported_platform_capabilities().into_iter().collect()
}

fn is_safe_app_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && id
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && id
            .as_bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

fn is_forbidden_capability(capability: &str) -> bool {
    matches!(
        capability,
        "harborgate.credentials.raw"
            | "harborgate.route_registry.write"
            | "harborbeacon.state.raw"
            | "filesystem.host.write"
            | "docker.socket.raw"
    )
}

fn is_safe_volume_mount(path: &str) -> bool {
    path.starts_with("/data")
        || path.starts_with("/app/data")
        || path.starts_with("/cache")
        || path.starts_with("/app/cache")
        || path.starts_with("/config")
        || path.starts_with("/app/config")
        || path.starts_with("/run/secrets")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_manifest() -> HarborAppManifest {
        HarborAppManifest {
            contract: HARBOR_APP_CONTRACT_VERSION.to_string(),
            id: "finance-audit".to_string(),
            name: "Finance Audit".to_string(),
            version: "0.1.0".to_string(),
            image: Some("harbor.local/finance-audit:0.1.0".to_string()),
            build: None,
            routes: vec![HarborAppRoute {
                path_prefix: "/apps/finance-audit/".to_string(),
                service_port: 4190,
                strip_prefix: false,
            }],
            health: HarborAppHealth {
                path: "/healthz".to_string(),
                port: 4190,
                interval_seconds: 30,
            },
            permissions: vec![HarborAppPermission {
                capability: "platform.models.infer".to_string(),
                actions: vec!["call".to_string()],
                risk: HarborAppRisk::Medium,
            }],
            volumes: vec![HarborAppVolume {
                name: "data".to_string(),
                mount_path: "/data".to_string(),
                kind: HarborAppVolumeKind::Data,
            }],
            platform_capabilities: vec!["platform.models.infer".to_string()],
            exposure: HarborAppExposure::Lan,
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn harbor_app_manifest_accepts_first_batch_app() {
        let report = validate_app_manifest(&valid_manifest());

        assert!(report.ok, "{:?}", report.errors);
        assert_eq!(
            report.declared_capabilities,
            vec!["platform.models.infer".to_string()]
        );
    }

    #[test]
    fn harbor_app_manifest_rejects_cross_app_route_and_raw_credentials() {
        let mut manifest = valid_manifest();
        manifest.routes[0].path_prefix = "/apps/other/".to_string();
        manifest
            .platform_capabilities
            .push("harborgate.credentials.raw".to_string());

        let report = validate_app_manifest(&manifest);

        assert!(!report.ok);
        assert!(report
            .errors
            .iter()
            .any(|error| error.contains("must stay under")));
        assert!(report
            .errors
            .iter()
            .any(|error| error.contains("outside Harbor app scope")));
    }

    #[test]
    fn harbor_app_paths_stay_under_default_roots() {
        let paths = harbor_app_paths("outreach", &HarborAppPathRoots::default()).unwrap();

        assert_eq!(
            paths.compose_file,
            PathBuf::from("/var/lib/harbor/apps/outreach/compose.yaml")
        );
        assert_eq!(
            paths.data_root,
            PathBuf::from("/mnt/software/harbor-apps/outreach")
        );
        assert_eq!(
            paths.env_file,
            PathBuf::from("/etc/harbor/apps/outreach.env")
        );
    }

    #[test]
    fn harbor_app_paths_reject_traversal_ids() {
        assert!(harbor_app_paths("../gate", &HarborAppPathRoots::default()).is_err());
    }

    #[test]
    fn harbor_app_manifest_parses_yaml() {
        let manifest = parse_harbor_app_manifest_yaml(
            r#"
contract: harbor.app.v1
id: finance-audit
name: Finance Audit
version: 0.1.0
image: harbor.local/finance-audit:0.1.0
routes:
  - path_prefix: /apps/finance-audit/
    service_port: 4190
health:
  path: /healthz
  port: 4190
permissions:
  - capability: platform.models.infer
    actions: [call]
    risk: medium
volumes:
  - name: data
    mount_path: /data
    kind: data
platform_capabilities:
  - platform.models.infer
exposure: lan
"#,
        )
        .expect("parse manifest");

        assert!(validate_app_manifest(&manifest).ok);
    }

    #[test]
    fn harbor_app_registry_entry_captures_paths_and_capabilities() {
        let (entry, report) = build_harbor_app_registry_entry(
            valid_manifest(),
            &HarborAppPathRoots::default(),
            "100",
            "101",
        )
        .expect("entry");

        assert!(report.ok);
        assert_eq!(entry.app_id, "finance-audit");
        assert_eq!(entry.status, HarborAppRuntimeStatus::Registered);
        assert_eq!(
            entry.paths.manifest_file,
            PathBuf::from("/var/lib/harbor/apps/finance-audit/app.manifest.yaml")
        );
        assert_eq!(entry.declared_capabilities, vec!["platform.models.infer"]);
    }
}
