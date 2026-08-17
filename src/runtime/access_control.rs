use crate::control_plane::access::action_allowed;
use crate::control_plane::users::{
    MembershipStatus, RoleKind, UserAccount, Workspace, WorkspaceStatus,
};
use crate::runtime::admin_console::AdminConsoleState;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AccessIdentityHints {
    pub user_id: Option<String>,
    pub open_id: Option<String>,
    pub harboros_user_id: Option<String>,
}

impl AccessIdentityHints {
    pub fn is_empty(&self) -> bool {
        self.user_id
            .as_deref()
            .map(str::trim)
            .is_none_or(str::is_empty)
            && self
                .open_id
                .as_deref()
                .map(str::trim)
                .is_none_or(str::is_empty)
            && self
                .harboros_user_id
                .as_deref()
                .map(str::trim)
                .is_none_or(str::is_empty)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessAction {
    AdminReadState,
    AdminManage,
    CameraView,
    CameraOperate,
    ApprovalReview,
}

impl AccessAction {
    pub fn permission_key(self) -> &'static str {
        match self {
            Self::AdminReadState => "admin.read_state",
            Self::AdminManage => "admin.manage",
            Self::CameraView => "camera.view",
            Self::CameraOperate => "camera.operate",
            Self::ApprovalReview => "approval.review",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::AdminReadState => "查看控制台状态",
            Self::AdminManage => "修改控制台配置",
            Self::CameraView => "查看摄像头画面",
            Self::CameraOperate => "执行摄像头操作",
            Self::ApprovalReview => "处理审批任务",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessPrincipal {
    pub workspace_id: String,
    pub user_id: String,
    pub display_name: String,
    pub role_kind: RoleKind,
}

pub fn authorize_access(
    state: &AdminConsoleState,
    hints: &AccessIdentityHints,
    action: AccessAction,
    resource: &str,
    allow_local_owner_fallback: bool,
) -> Result<AccessPrincipal, String> {
    let workspace = active_workspace(state)?;
    let user_id = resolve_user_id(state, workspace, hints, allow_local_owner_fallback)?;
    let membership = state
        .platform
        .memberships
        .iter()
        .find(|membership| {
            membership.workspace_id == workspace.workspace_id
                && membership.user_id == user_id
                && membership.status == MembershipStatus::Active
        })
        .ok_or_else(|| format!("当前身份尚未加入工作空间 {}。", workspace.display_name))?;

    let role_key = role_kind_key(membership.role_kind);
    if !action_allowed(
        &state.platform.permission_bindings,
        &workspace.workspace_id,
        role_key,
        resource,
        action.permission_key(),
    ) {
        return Err(format!(
            "当前身份没有{}权限（role={}）。",
            action.label(),
            role_key
        ));
    }

    Ok(AccessPrincipal {
        workspace_id: workspace.workspace_id.clone(),
        user_id: membership.user_id.clone(),
        display_name: display_name_for_user(&state.platform.users, &membership.user_id),
        role_kind: membership.role_kind,
    })
}

pub fn authorize_authenticated_principal(
    state: &AdminConsoleState,
    principal: &AccessPrincipal,
    action: AccessAction,
    resource: &str,
) -> Result<AccessPrincipal, String> {
    let workspace = active_workspace(state)?;
    if principal.workspace_id != workspace.workspace_id {
        return Err(format!(
            "当前身份不属于活动工作空间 {}。",
            workspace.display_name
        ));
    }

    let role_key = role_kind_key(principal.role_kind);
    if !action_allowed(
        &state.platform.permission_bindings,
        &workspace.workspace_id,
        role_key,
        resource,
        action.permission_key(),
    ) {
        return Err(format!(
            "当前身份没有{}权限（role={}）。",
            action.label(),
            role_key
        ));
    }

    Ok(principal.clone())
}

fn active_workspace(state: &AdminConsoleState) -> Result<&Workspace, String> {
    state
        .platform
        .workspaces
        .iter()
        .find(|workspace| workspace.status == WorkspaceStatus::Active)
        .or_else(|| state.platform.workspaces.first())
        .ok_or_else(|| "当前未配置可用 workspace。".to_string())
}

fn resolve_user_id(
    state: &AdminConsoleState,
    workspace: &Workspace,
    hints: &AccessIdentityHints,
    allow_local_owner_fallback: bool,
) -> Result<String, String> {
    if let Some(user_id) = hints
        .user_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(user_id.to_string());
    }

    if let Some(open_id) = hints
        .open_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return state
            .platform
            .identity_bindings
            .iter()
            .find(|binding| binding.external_user_id == open_id)
            .map(|binding| binding.user_id.clone())
            .ok_or_else(|| format!("未找到 open_id={open_id} 对应的绑定身份。"));
    }

    if let Some(harboros_user_id) = hints
        .harboros_user_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Ok(harboros_user_id.to_string());
    }

    if allow_local_owner_fallback {
        return Ok(workspace.owner_user_id.clone());
    }

    Err("当前请求缺少用户身份，无法校验访问权限。".to_string())
}

fn display_name_for_user(users: &[UserAccount], user_id: &str) -> String {
    users
        .iter()
        .find(|user| user.user_id == user_id)
        .map(|user| user.display_name.clone())
        .unwrap_or_else(|| user_id.to_string())
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

#[cfg(test)]
mod tests {
    use crate::runtime::admin_console::{
        build_platform_state, AdminConsoleState, IdentityBindingRecord,
    };

    use super::{action_allowed, authorize_access, AccessAction, AccessIdentityHints};

    #[test]
    fn local_owner_fallback_can_manage_admin_surface() {
        let mut state = AdminConsoleState::default();
        state.platform = build_platform_state(&state);

        let principal = authorize_access(
            &state,
            &AccessIdentityHints::default(),
            AccessAction::AdminManage,
            "workspace:home-1",
            true,
        )
        .expect("principal");

        assert_eq!(principal.user_id, "local-owner");
    }

    #[test]
    fn admin_manage_contract_allows_owner_and_admin_but_denies_operator() {
        let mut state = AdminConsoleState::default();
        state.platform = build_platform_state(&state);
        let permissions = &state.platform.permission_bindings;

        for role in ["owner", "admin"] {
            assert!(action_allowed(
                permissions,
                "home-1",
                role,
                "workspace:home-1",
                AccessAction::AdminManage.permission_key(),
            ));
        }
        assert!(!action_allowed(
            permissions,
            "home-1",
            "operator",
            "workspace:home-1",
            AccessAction::AdminManage.permission_key(),
        ));
    }

    #[test]
    fn bound_viewer_can_only_view_camera() {
        let mut state = AdminConsoleState::default();
        state.identity_bindings.push(IdentityBindingRecord {
            open_id: "ou_viewer".to_string(),
            user_id: Some("viewer-1".to_string()),
            union_id: None,
            display_name: "Viewer".to_string(),
            chat_id: None,
        });
        state.platform = build_platform_state(&state);

        assert!(authorize_access(
            &state,
            &AccessIdentityHints {
                user_id: None,
                open_id: Some("ou_viewer".to_string()),
                harboros_user_id: None,
            },
            AccessAction::CameraView,
            "camera:living-room",
            false,
        )
        .is_ok());
        assert!(authorize_access(
            &state,
            &AccessIdentityHints {
                user_id: None,
                open_id: Some("ou_viewer".to_string()),
                harboros_user_id: None,
            },
            AccessAction::AdminManage,
            "workspace:home-1",
            false,
        )
        .is_err());
    }

    #[test]
    fn harboros_user_hint_reuses_existing_workspace_membership() {
        let mut state = AdminConsoleState::default();
        state.platform = build_platform_state(&state);

        let principal = authorize_access(
            &state,
            &AccessIdentityHints {
                user_id: None,
                open_id: None,
                harboros_user_id: Some("local-owner".to_string()),
            },
            AccessAction::AdminReadState,
            "workspace:home-1",
            false,
        )
        .expect("principal");

        assert_eq!(principal.user_id, "local-owner");
    }
}
