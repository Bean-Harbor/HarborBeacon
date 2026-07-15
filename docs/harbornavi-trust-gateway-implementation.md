# HarborNavi Trust Gateway Implementation Design

更新时间：2026-06-13

## 1. 目标

本文把 HarborNavi Trust Gateway 从产品定义落到代码实现。实现形态不新增统一代码层或独立服务进程；它在 HarborBeacon、HarborGate、HarborLink、Harbor Assistant/WebUI 之间形成一组可验证的策略切面：

```text
家庭入口请求
  -> 来源与身份上下文
  -> 家庭账号/信息归属/动作风险判断
  -> 本地回答、拒绝、确认、脱敏上云或执行动作
  -> metadata-only audit
```

相关文档：

- `docs/harbornavi-trust-gateway.md`
- `docs/harbornavi-voice-trust-model.md`
- `docs/research/harbor-privacy-gateway-investor-plan.md`
- `docs/HarborBeacon-Harbor-Collaboration-Contract-v3.md`
- `C:\Users\beanw\OpenSource\HarborGate\docs\HarborBeacon-HarborGate-Agent-Contract-v2.0.md`
- `C:\Users\beanw\OpenSource\HarborLink\docs\protocol.md`
- `C:\Users\beanw\OpenSource\HarborNAS-webui\docs\harbor-assistant-webui-integration.md`

## 2. 实现边界

### 2.1 HarborBeacon

HarborBeacon 是 Trust Gateway 的策略、业务状态和审计归属地。

归属：

- 家庭账号模型：`root / admin / member / guest / system`
- 成员、身份绑定、workspace membership、分享状态
- 请求级 policy decision
- action risk：L0 / L1 / L2 / L3
- step-up approval
- semantic capsule 与 cloud route policy
- privacy gateway evidence
- audit record 与 readiness/evaluation projection

优先复用现有模块：

- `src/runtime/access_control.rs`
- `src/runtime/admin_console.rs`
- `src/runtime/privacy_gateway.rs`
- `src/runtime/task_api.rs`
- `src/runtime/model_center.rs`
- `src/runtime/family_memory.rs`
- `src/runtime/family_timeline.rs`
- `src/control_plane/audit.rs`
- `src/control_plane/approvals.rs`

### 2.2 HarborGate

HarborGate 负责 IM 入口、平台身份归一化、route registry、平台凭据和投递。

归属：

- IM adapter / webhook / long-poll / websocket
- `transport.route_key`
- 平台 message id、attachment proxy、delivery formatting
- platform credential 与 redacted gateway status

Trust Gateway 相关职责：

- 在 `POST /api/web/turns` 中传递来源上下文和 message parts。
- 对 IM 语音、图片、文件等输入保留平台中立 metadata。
- 只传身份线索，不做家庭权限最终判定。
- 不解释 HarborBeacon 返回的 `active_frame.kind` 业务语义。

### 2.3 HarborLink

HarborLink 是 Hub 侧连接器和家庭设备桥，负责从 Home Assistant、摄像头和本地事件中提供受控输入源。

归属：

- Hub identity 与 outbound-only MQTT
- Home Assistant selected entity/state/service bridge
- camera snapshot/live/record clip 的 allowlisted bridge
- local vision event 的 cloud-safe metadata publish
- 设备/capture source 能力声明

Trust Gateway 相关职责：

- 给 Beacon/Cloud 提供 `capture_source_id`、设备能力、allowlist、事件摘要和风险 metadata。
- 对 cloud event payload 做 allowlist 与敏感字段拦截。
- 不发布 HA token、camera URL、RTSP、local path、upload URL、原始图片或凭据。
- 不绕过 Beacon policy 直接执行高风险家庭动作。

### 2.4 Harbor Assistant / WebUI

Harbor Assistant 是用户可见的配置、解释、审批和审计表面。

归属：

- `/ui/harbor-assistant`
- same-origin `/api/beacon/*`
- 家庭账号/成员/绑定状态的展示与操作
- Trust Gateway decision 的可解释展示
- approval queue、audit stream、model route policy、privacy readiness

WebUI 不拥有 task state、runtime state、device execution semantics、IM transport semantics 或 HarborLink MQTT 语义。

## 3. 核心数据模型

### 3.1 请求上下文

建议新增或统一成内部结构 `TrustGatewayRequestContext`。第一阶段可先作为 Rust 内部 facade，不急于新增公开 HTTP contract。

```rust
pub struct TrustGatewayRequestContext {
    pub trace_id: String,
    pub workspace_id: String,
    pub conversation_handle: Option<String>,
    pub requester: RequesterContext,
    pub source: SourceContext,
    pub intent: IntentContext,
    pub resources: Vec<ResourceDescriptor>,
    pub destination: DecisionDestination,
}
```

关键字段：

- `requester.user_id`
- `requester.role`
- `requester.identity_confidence`
- `source.kind`：`webui / app / im / voice / automation / device_event / harborlink`
- `source.route_key`
- `source.capture_source_id`
- `source.audio_live_mode`
- `intent.action_kind`
- `intent.action_risk`
- `resources[].subject_user_id`
- `resources[].resource_scope`
- `resources[].share_state`
- `destination`：`local / redacted_cloud / cloud / device_action`

### 3.2 家庭账号与分享状态

账号分组沿用产品定义：

- `root`
- `admin`
- `member`
- `guest`
- `system`

分享状态：

- `private`
- `home_shared`
- `role_shared`
- `temporary_shared`
- `system_only`

实现建议：

- 在现有 `Workspace / UserAccount / IdentityBinding / Membership` 基础上增加 HarborNavi-facing projection。
- `root` 对应 workspace owner / recovery owner，不鼓励日常登录态使用。
- `admin` 管理规则与设备，默认不能静默读取 `member.private`。
- `system` 只作为自动化和设备事件主体，不能伪装成家庭成员。

### 3.3 决策结果

```rust
pub enum TrustGatewayDecision {
    AllowLocal,
    AllowDeviceAction,
    AllowRedactedCloud,
    AllowCloud,
    StepUpRequired,
    Deny,
}

pub struct TrustGatewayDecisionRecord {
    pub decision_id: String,
    pub trace_id: String,
    pub decision: TrustGatewayDecision,
    pub policy_version: String,
    pub reasons: Vec<String>,
    pub required_approval_policy: Option<String>,
    pub semantic_capsule_ref: Option<String>,
    pub audit_ref: String,
}
```

原则：

- 决策结果可以被 UI 展示为普通语言。
- 审计只保存 metadata、policy evidence、reason code、capsule 摘要，不保存 raw audio、RTSP URL、source path、credential、API key 或完整家庭原文。
- `StepUpRequired` 必须落到现有 approval flow，避免另起一套确认系统。

## 4. 决策流程

### 4.1 总流程

```text
Ingress
  -> normalize actor/source
  -> resolve household account and membership
  -> classify resource ownership and share_state
  -> classify action_risk
  -> evaluate voice/context/privacy risk
  -> decide local / deny / step-up / redacted cloud / cloud / device action
  -> write audit evidence
  -> route to selected capability domain
```

### 4.2 访问控制

最小实现：

- `private`：仅 subject 本人可直接访问。
- `home_shared`：active household member 可访问。
- `role_shared`：指定 role 可访问。
- `temporary_shared`：必须校验 expiry、scope、revocation。
- `system_only`：只给策略判断使用，不直接展示给用户。

特殊规则：

- `admin` 可以管理分享规则，但读取成员私有信息需要 step-up 和 audit。
- `root` 可以做恢复和迁移，但读取成员私有信息需要原因、确认和 audit。
- `guest` 默认不能访问家庭时间线、摄像头、成员资料、家庭记忆。

### 4.3 动作风险

| 风险 | 例子 | 默认策略 |
| --- | --- | --- |
| L0 | 普通问答、播放音乐、开关灯 | 可本地执行，写轻量 audit |
| L1 | 读日程、家庭消息、摄像头摘要、时间线查询 | 需要账号/来源/分享状态共同放行 |
| L2 | 开门、关闭报警、关闭摄像头、购买、导出视频、分享资料 | 必须 step-up |
| L3 | 改权限、解绑设备、删除数据、开启云同步、恢复出厂 | 必须 admin/root step-up，强 audit |

语音入口只能提高或降低风险置信度，不能独立授权 L2/L3。

### 4.4 上云决策

沿用现有 Privacy Gateway contract：

- `strict_local`：阻断 cloud。
- `allow_redacted_cloud`：仅上传 task-minimal semantic capsule。
- `allow_cloud`：允许 cloud，但仍记录 evidence。

新增 Trust Gateway 后，上云前应多一层 household decision：

```text
account/share/action/source decision
  -> privacy gateway
  -> semantic capsule
  -> model center route policy
  -> cloud fallback
```

`semantic.router` 仍保持 local-only。第一阶段 cloud fallback 仍以 `retrieval.answer` 为主，不把 AIoT 控制、HarborOS 命令、OCR、VLM、embedding 默认放到 cloud。

## 5. 分仓实现方案

### 5.1 HarborBeacon 实现项

P0 目标是先形成内部可调用的 Trust Gateway facade。

建议工作包：

1. 在 `src/runtime/access_control.rs` 增加 HarborNavi household projection：
   - `HouseholdRole`
   - `ShareState`
   - `ResourceScope`
   - `ResourceDescriptor`
   - `can_access_resource(...)`
2. 在 `src/runtime/privacy_gateway.rs` 扩展 semantic capsule 输入：
   - 加入 `subject_user_id`
   - 加入 `share_state`
   - 加入 `source_kind`
   - 加入 `purpose`
   - 继续保持 evidence metadata-only
3. 在 `src/runtime/task_api.rs` 的 action admission 前插入 Trust Gateway decision：
   - 对 L2/L3 生成 approval ticket
   - 对 cloud fallback 先走 Privacy Gateway
   - 对 deny/step-up 返回 conversation act 或 frame prompt
4. 在 `src/runtime/admin_console.rs` 增加只读 projection：
   - household accounts
   - policy version
   - recent decisions
   - pending approvals
   - privacy readiness
5. 在 `src/control_plane/audit.rs` 补充 query/action kind：
   - `trust_gateway.evaluate`
   - `trust_gateway.step_up_required`
   - `trust_gateway.cloud_route`
   - `trust_gateway.device_action_admitted`
   - `trust_gateway.denied`

### 5.2 HarborGate 实现项

P0 不改变 v2.0 contract 字段语义，优先把已有字段填得更完整。

建议工作包：

1. 在 inbound normalization 中补齐：
   - `conversation.surface`
   - `transport.route_key`
   - `transport.message_id`
   - `transport.capabilities`
   - `input.parts`
2. 对 IM voice message 增加平台中立 part metadata：
   - `part.kind = "audio"`
   - `source = "im_voice"`
   - `duration_ms`
   - `mime_type`
   - `has_audio_sample`
   - `transcription_source`
3. 传递 external identity hint：
   - platform user id
   - bound account id if known
   - route binding state
4. 继续由 Beacon 决定家庭权限、approval 和 audit。

如果需要新增字段，应先走 v2.0 contract 变更评审；P0 尽量放在 `transport.metadata` 或 `input.parts[].metadata` 中。

### 5.3 HarborLink 实现项

P0 重点是 source provenance 和 cloud-safe event。

建议工作包：

1. 扩展 Home Assistant / camera snapshot / local vision event payload 的 source metadata：
   - `capture_source_id`
   - `capture_source_kind`
   - `room_hint`
   - `device_binding_state`
   - `capabilities`
   - `local_only_fields_dropped`
2. 保持 event payload allowlist：
   - event id
   - event type
   - confidence
   - labels
   - summary
   - timestamps
   - analyzer
   - safe metrics
3. 对 command ack 增加 policy-facing result：
   - `allowlist_checked`
   - `entity_allowed`
   - `camera_allowed`
   - `sensitive_fields_redacted`
4. 高风险设备动作继续 fail closed：
   - lock
   - siren
   - access control
   - camera PTZ
   - arbitrary automation/script

### 5.4 Harbor Assistant / WebUI 实现项

P0 先做 operator-visible，不做复杂策略编辑器。

建议页面或子模块：

1. 家庭账号
   - root/admin/member/guest/system projection
   - identity binding 状态
   - 成员分享状态摘要
2. 可信网关策略
   - L0-L3 风险规则说明
   - cloud route policy 状态
   - semantic capsule readiness
3. 审批队列
   - L2/L3 pending approvals
   - 请求来源、动作、风险原因
   - approve/reject 仍调用 Beacon-owned API
4. 审计记录
   - answer / deny / step-up / redacted cloud / device action
   - 不展示 raw sensitive content
5. 语音入口状态
   - capture source 列表
   - spoof risk 可用性
   - raw audio retention policy

## 6. API 与内部接口建议

### 6.1 内部 Rust facade

第一阶段建议内部接口先于外部 HTTP API：

```rust
pub trait TrustGateway {
    fn evaluate(&self, ctx: TrustGatewayRequestContext) -> TrustGatewayDecisionRecord;
}
```

调用点：

- `general_message_router` 选出候选 intent 后。
- capability handler 执行前。
- cloud fallback 生成 prompt 前。
- approval resume 前复核高风险动作上下文。

### 6.2 Admin API projection

WebUI 只需要 projection，不承载完整策略引擎：

- `GET /api/beacon/trust-gateway/status`
- `GET /api/beacon/trust-gateway/decisions`
- `GET /api/beacon/trust-gateway/policies`
- `GET /api/beacon/trust-gateway/accounts`

这些接口可以由 `agent_hub_admin_api.rs` 暴露，返回 redacted / metadata-only 视图。

### 6.3 Turn API 集成

`POST /api/web/turns` 不需要为 Trust Gateway 另开一条业务入口。Trust Gateway 应融入 turn handling：

```text
TaskTurnEnvelope
  -> actor/conversation/transport/input
  -> route candidate
  -> TrustGateway.evaluate
  -> selected capability or frame_prompt
  -> reply + active_frame + observability
```

对于需要确认的请求，返回 `active_frame.kind = trust_gateway.step_up` 或复用现有 approval frame 语义，具体命名要和现有 active frame enum 对齐后再定。

## 7. 评测与验收

### 7.1 单元测试

HarborBeacon：

- `member` 不能读其他成员 `private`。
- `admin` 不能静默读成员 `private`。
- `root` 读取成员 `private` 需要 step-up。
- `guest` 不能读家庭时间线和摄像头摘要。
- `system` 不能伪装成家庭成员。
- `allow_redacted_cloud` 只能使用 semantic capsule。
- capsule 失败时 cloud 不被调用。
- L2/L3 voice-only 全部进入 step-up。

HarborLink：

- local vision event 不包含 camera id、RTSP、local path、image bytes、upload URL。
- unlisted camera / entity 被拒绝。
- command ack 不泄露敏感字段。

HarborGate：

- voice/image/file parts metadata 可被 Beacon 接收。
- route_key 仍 opaque。
- Gate 不解释 active_frame business kind。

WebUI：

- Trust Gateway 页面只读 projection 正常渲染。
- approval approve/reject 调用 Beacon API。
- audit records 不显示敏感原文。

### 7.2 合同与评测 CLI

继续使用现有检查：

- `cargo test`
- `python -m pytest tests/contracts -q`
- `target/release/evaluate-privacy-gateway`
- `target/release/evaluate-release-gate`

建议新增：

- `target/release/evaluate-trust-gateway`

评测集 P0 场景：

- 电视外放 owner 录音要求关闭摄像头。
- IM voice 转写要求导出家庭视频。
- guest 查询家庭时间线。
- admin 查询 member 私人记忆。
- member 查询 home_shared 购物清单。
- system automation 触发开灯。
- cloud fallback 处理账单 OCR 摘要。
- semantic capsule 中出现 credential-like 字段。

### 7.3 观测指标

- `trust_gateway.decision.count`
- `trust_gateway.step_up.count`
- `trust_gateway.denied.count`
- `trust_gateway.redacted_cloud.count`
- `trust_gateway.raw_cloud.count`
- `trust_gateway.policy_version`
- `trust_gateway.audit_lag_ms`
- `privacy_gateway.capsule_blocked.count`
- `voice_spoof.unknown.count`

指标默认不带 raw prompt、source path、URL、credential、RTSP 或 raw audio。

## 8. 分阶段路线

### P0：可信入口闭环

目标：内部 API + 多入口接入 + 本地决策闭环。

交付：

- HarborBeacon 内部 `TrustGateway.evaluate(...)`
- household account/share projection
- L0-L3 action risk rules
- L2/L3 approval admission
- redacted cloud path 继续走 Privacy Gateway
- Gate/Link source metadata 接入
- WebUI status + approval + audit projection

### P1：家庭场景验证

目标：成员、访客、儿童、重放仿冒、隐私任务集验证。

交付：

- 家庭高风险场景评测集
- voice spoof / replay metadata 接入
- semantic capsule 策略库初版
- policy versioning
- decision explanation 文案
- release gate 加入 Trust Gateway scenario pack

### P2：受控伙伴接入

目标：认证设备/服务可以接入，但必须经过 Harbor Trust Gateway。

交付：

- partner-facing Trust Gateway API profile
- capture source capability declaration
- certified device/service allowlist
- third-party model/tool route policy
- 学术合作评估体系与 benchmark 报告

这里的第三方接入只指受控伙伴、认证设备和受控服务，不等同于开放任意家庭数据接口。

## 9. 非目标

P0 不做：

- 企业级 IAM 或复杂组织树。
- 独立 Trust Gateway 服务进程。
- 公开开放平台。
- voice-only 高风险授权。
- 默认保存 raw audio。
- 把 HarborGate route_key 当家庭账号。
- 把 HarborLink 变成通用 AIoT hub。
- 把 Harbor Assistant/WebUI 变成业务状态源。
- 把所有云端模型调用都纳入默认路径。

## 10. Stop-the-line 条件

遇到以下情况需要架构评审：

- 需要改 `POST /api/web/turns` 公共 contract 字段。
- 需要让 HarborGate 保存或解释家庭权限。
- 需要让 HarborLink 执行 lock/siren/access-control/PTZ/arbitrary automation。
- 需要让 WebUI 保存业务状态或绕过 Beacon API。
- 需要把 raw private data、raw audio、camera URL、source path、credential 送入 cloud prompt。
- 需要把 `semantic.router` 改成 cloud fallback。

## 11. 最小可运行 Demo

建议第一个 demo 选三条路径：

1. **member 查询自己的私人信息**
   - 本地放行。
   - 写 `trust_gateway.evaluate` audit。
2. **guest 查询家庭摄像头摘要**
   - 本地拒绝或要求 admin approval。
   - UI 展示原因。
3. **admin 请求云端总结账单 OCR**
   - Beacon 生成 task-minimal semantic capsule。
   - Privacy Gateway 记录 redacted cloud evidence。
   - Cloud model 只拿到最小事实。

这三条路径足以证明入口鉴权、家庭权限、隐私上云和审计闭环已经打通。
