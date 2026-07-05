//! HarborOS System Domain executor planning for Harbor apps.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::control_plane::apps::{
    harbor_app_paths, validate_app_manifest, HarborAppCommandPreviewSnapshot,
    HarborAppExecutionPlanSnapshot, HarborAppExposure, HarborAppManifest, HarborAppPathRoots,
    HarborAppPaths,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarborAppLifecycleAction {
    Install,
    Start,
    Stop,
    Restart,
    Health,
    Logs,
    EnableExposure,
    DisableExposure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarborAppExecutorConfig {
    pub roots: HarborAppPathRoots,
    pub compose_project_prefix: String,
    pub log_tail_lines: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarborAppExecutionPlan {
    pub app_id: String,
    pub action: HarborAppLifecycleAction,
    pub paths: HarborAppPaths,
    pub compose_project: String,
    pub commands: Vec<HarborAppCommandPreview>,
    pub route_prefixes: Vec<String>,
    pub exposure: HarborAppExposure,
    pub audit_action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarborAppCommandPreview {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub requires_approval: bool,
    pub risk: HarborAppCommandRisk,
    pub reason: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HarborAppCommandRisk {
    ReadOnly,
    Medium,
    High,
}

impl Default for HarborAppExecutorConfig {
    fn default() -> Self {
        Self {
            roots: HarborAppPathRoots::default(),
            compose_project_prefix: "harbor".to_string(),
            log_tail_lines: 200,
        }
    }
}

pub fn build_harbor_app_execution_plan(
    manifest: &HarborAppManifest,
    action: HarborAppLifecycleAction,
    config: &HarborAppExecutorConfig,
) -> Result<HarborAppExecutionPlan, String> {
    let validation = validate_app_manifest(manifest);
    if !validation.ok {
        return Err(validation.errors.join("; "));
    }

    let paths = harbor_app_paths(&manifest.id, &config.roots)?;
    let compose_project = format!("{}-{}", config.compose_project_prefix, manifest.id);
    let commands = build_command_previews(manifest, action, &paths, &compose_project, config);
    let route_prefixes = manifest
        .routes
        .iter()
        .map(|route| route.path_prefix.clone())
        .collect::<Vec<_>>();

    Ok(HarborAppExecutionPlan {
        app_id: manifest.id.clone(),
        action,
        paths,
        compose_project,
        commands,
        route_prefixes,
        exposure: manifest.exposure,
        audit_action: audit_action_for(action).to_string(),
    })
}

pub fn harbor_app_execution_plan_snapshot(
    plan: &HarborAppExecutionPlan,
) -> HarborAppExecutionPlanSnapshot {
    let commands = plan
        .commands
        .iter()
        .map(|command| HarborAppCommandPreviewSnapshot {
            program: command.program.clone(),
            args: command.args.clone(),
            cwd: command.cwd.as_ref().map(|path| path.display().to_string()),
            requires_approval: command.requires_approval,
            risk: command_risk_label(command.risk).to_string(),
            reason: command.reason.clone(),
        })
        .collect::<Vec<_>>();
    let requires_approval = commands.iter().any(|command| command.requires_approval);
    HarborAppExecutionPlanSnapshot {
        app_id: plan.app_id.clone(),
        action: lifecycle_action_label(plan.action).to_string(),
        compose_project: plan.compose_project.clone(),
        route_prefixes: plan.route_prefixes.clone(),
        exposure: plan.exposure,
        audit_action: plan.audit_action.clone(),
        command_count: commands.len(),
        commands,
        requires_approval,
    }
}

fn build_command_previews(
    manifest: &HarborAppManifest,
    action: HarborAppLifecycleAction,
    paths: &HarborAppPaths,
    compose_project: &str,
    config: &HarborAppExecutorConfig,
) -> Vec<HarborAppCommandPreview> {
    match action {
        HarborAppLifecycleAction::Install => vec![
            manager_preview(
                "harbor-app-manager",
                vec![
                    "render-compose".to_string(),
                    "--app".to_string(),
                    manifest.id.clone(),
                    "--manifest".to_string(),
                    paths.manifest_file.display().to_string(),
                    "--output".to_string(),
                    paths.compose_file.display().to_string(),
                ],
                None,
                true,
                HarborAppCommandRisk::Medium,
                "render managed compose and route metadata",
            ),
            compose_preview(
                paths,
                compose_project,
                vec!["up".to_string(), "-d".to_string()],
                true,
                HarborAppCommandRisk::Medium,
                "start app through managed Docker Compose",
            ),
        ],
        HarborAppLifecycleAction::Start => vec![compose_preview(
            paths,
            compose_project,
            vec!["up".to_string(), "-d".to_string()],
            true,
            HarborAppCommandRisk::Medium,
            "start app through managed Docker Compose",
        )],
        HarborAppLifecycleAction::Stop => vec![compose_preview(
            paths,
            compose_project,
            vec!["down".to_string()],
            true,
            HarborAppCommandRisk::Medium,
            "stop only this app compose project",
        )],
        HarborAppLifecycleAction::Restart => vec![compose_preview(
            paths,
            compose_project,
            vec!["restart".to_string()],
            true,
            HarborAppCommandRisk::Medium,
            "restart only this app compose project",
        )],
        HarborAppLifecycleAction::Health => vec![compose_preview(
            paths,
            compose_project,
            vec!["ps".to_string(), "--format".to_string(), "json".to_string()],
            false,
            HarborAppCommandRisk::ReadOnly,
            "inspect app compose status",
        )],
        HarborAppLifecycleAction::Logs => vec![compose_preview(
            paths,
            compose_project,
            vec![
                "logs".to_string(),
                "--tail".to_string(),
                config.log_tail_lines.to_string(),
            ],
            false,
            HarborAppCommandRisk::ReadOnly,
            "read bounded recent app logs",
        )],
        HarborAppLifecycleAction::EnableExposure => vec![manager_preview(
            "harbor-app-manager",
            vec![
                "exposure".to_string(),
                "enable".to_string(),
                "--app".to_string(),
                manifest.id.clone(),
            ],
            None,
            true,
            HarborAppCommandRisk::High,
            "enable app route outside the default local scope",
        )],
        HarborAppLifecycleAction::DisableExposure => vec![manager_preview(
            "harbor-app-manager",
            vec![
                "exposure".to_string(),
                "disable".to_string(),
                "--app".to_string(),
                manifest.id.clone(),
            ],
            None,
            false,
            HarborAppCommandRisk::ReadOnly,
            "disable optional app tunnel exposure",
        )],
    }
}

fn compose_preview(
    paths: &HarborAppPaths,
    compose_project: &str,
    compose_args: Vec<String>,
    requires_approval: bool,
    risk: HarborAppCommandRisk,
    reason: &str,
) -> HarborAppCommandPreview {
    let mut args = vec![
        "compose".to_string(),
        "-f".to_string(),
        paths.compose_file.display().to_string(),
        "--project-name".to_string(),
        compose_project.to_string(),
    ];
    args.extend(compose_args);

    manager_preview(
        "docker",
        args,
        Some(paths.app_root.clone()),
        requires_approval,
        risk,
        reason,
    )
}

fn manager_preview(
    program: &str,
    args: Vec<String>,
    cwd: Option<PathBuf>,
    requires_approval: bool,
    risk: HarborAppCommandRisk,
    reason: &str,
) -> HarborAppCommandPreview {
    HarborAppCommandPreview {
        program: program.to_string(),
        args,
        cwd,
        requires_approval,
        risk,
        reason: reason.to_string(),
    }
}

fn lifecycle_action_label(action: HarborAppLifecycleAction) -> &'static str {
    match action {
        HarborAppLifecycleAction::Install => "install",
        HarborAppLifecycleAction::Start => "start",
        HarborAppLifecycleAction::Stop => "stop",
        HarborAppLifecycleAction::Restart => "restart",
        HarborAppLifecycleAction::Health => "health",
        HarborAppLifecycleAction::Logs => "logs",
        HarborAppLifecycleAction::EnableExposure => "enable_exposure",
        HarborAppLifecycleAction::DisableExposure => "disable_exposure",
    }
}

fn command_risk_label(risk: HarborAppCommandRisk) -> &'static str {
    match risk {
        HarborAppCommandRisk::ReadOnly => "read_only",
        HarborAppCommandRisk::Medium => "medium",
        HarborAppCommandRisk::High => "high",
    }
}

fn audit_action_for(action: HarborAppLifecycleAction) -> &'static str {
    match action {
        HarborAppLifecycleAction::Install => "harbor_app.install.plan",
        HarborAppLifecycleAction::Start => "harbor_app.start.plan",
        HarborAppLifecycleAction::Stop => "harbor_app.stop.plan",
        HarborAppLifecycleAction::Restart => "harbor_app.restart.plan",
        HarborAppLifecycleAction::Health => "harbor_app.health.inspect",
        HarborAppLifecycleAction::Logs => "harbor_app.logs.inspect",
        HarborAppLifecycleAction::EnableExposure => "harbor_app.exposure.enable.plan",
        HarborAppLifecycleAction::DisableExposure => "harbor_app.exposure.disable.plan",
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use crate::control_plane::apps::{
        HarborAppHealth, HarborAppManifest, HarborAppPermission, HarborAppRisk, HarborAppRoute,
        HarborAppVolume, HarborAppVolumeKind, HARBOR_APP_CONTRACT_VERSION,
    };

    use super::*;

    fn outreach_manifest() -> HarborAppManifest {
        HarborAppManifest {
            contract: HARBOR_APP_CONTRACT_VERSION.to_string(),
            id: "outreach".to_string(),
            name: "Outreach".to_string(),
            version: "0.1.0".to_string(),
            image: Some("harbor.local/outreach:0.1.0".to_string()),
            build: None,
            routes: vec![HarborAppRoute {
                path_prefix: "/apps/outreach/".to_string(),
                service_port: 4192,
                strip_prefix: false,
            }],
            health: HarborAppHealth {
                path: "/healthz".to_string(),
                port: 4192,
                interval_seconds: 30,
            },
            permissions: vec![HarborAppPermission {
                capability: "platform.compliance.evaluate".to_string(),
                actions: vec!["call".to_string()],
                risk: HarborAppRisk::Medium,
            }],
            volumes: vec![HarborAppVolume {
                name: "data".to_string(),
                mount_path: "/data".to_string(),
                kind: HarborAppVolumeKind::Data,
            }],
            platform_capabilities: vec!["platform.compliance.evaluate".to_string()],
            exposure: HarborAppExposure::Lan,
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn start_plan_uses_managed_compose_and_requires_approval() {
        let plan = build_harbor_app_execution_plan(
            &outreach_manifest(),
            HarborAppLifecycleAction::Start,
            &HarborAppExecutorConfig::default(),
        )
        .unwrap();

        assert_eq!(plan.audit_action, "harbor_app.start.plan");
        assert_eq!(plan.commands.len(), 1);
        assert_eq!(plan.commands[0].program, "docker");
        assert!(plan.commands[0].requires_approval);
        assert!(plan.commands[0].args.contains(&"compose".to_string()));
        assert!(plan.commands[0]
            .args
            .contains(&plan.paths.compose_file.display().to_string()));
    }

    #[test]
    fn health_plan_is_read_only() {
        let plan = build_harbor_app_execution_plan(
            &outreach_manifest(),
            HarborAppLifecycleAction::Health,
            &HarborAppExecutorConfig::default(),
        )
        .unwrap();

        assert_eq!(plan.commands[0].risk, HarborAppCommandRisk::ReadOnly);
        assert!(!plan.commands[0].requires_approval);
        assert_eq!(plan.audit_action, "harbor_app.health.inspect");
    }

    #[test]
    fn invalid_manifest_does_not_build_execution_plan() {
        let mut manifest = outreach_manifest();
        manifest.routes[0].path_prefix = "/apps/finance-audit/".to_string();

        let error = build_harbor_app_execution_plan(
            &manifest,
            HarborAppLifecycleAction::Start,
            &HarborAppExecutorConfig::default(),
        )
        .unwrap_err();

        assert!(error.contains("must stay under"));
    }

    #[test]
    fn execution_plan_snapshot_preserves_command_previews() {
        let plan = build_harbor_app_execution_plan(
            &outreach_manifest(),
            HarborAppLifecycleAction::Start,
            &HarborAppExecutorConfig::default(),
        )
        .unwrap();

        let snapshot = harbor_app_execution_plan_snapshot(&plan);

        assert_eq!(snapshot.action, "start");
        assert_eq!(snapshot.command_count, 1);
        assert!(snapshot.requires_approval);
        assert_eq!(snapshot.commands[0].risk, "medium");
    }
}
