# HarborNavi Trust Gateway

更新时间：2026-06-12

## 1. 产品定义

HarborNavi Trust Gateway 是家庭 AI 的本地可信入口层。它不只是语音防伪，也不只是云端调用前的脱敏，而是把家庭入口、账号分组、私有信息、可分享信息、隐私网关、动作审批和审计收成一个用户能理解的产品模块：

```text
先判断谁在问、从哪里问、要访问谁的信息、要执行什么动作，
再决定是否本地回答、是否需要确认、哪些信息可以分享或上云。
```

产品口径可以合并为一个模块，但内部实现仍然保持 HarborBeacon 边界：

- 入口适配只负责接入请求和来源上下文。
- HarborBeacon / Framework 负责 policy、approval、audit、privacy gateway 和业务状态。
- Home Device / AIoT Domain 与 HarborOS System Domain 只执行已被授权的 domain action。

## 2. 能力组成

```text
HarborNavi Trust Gateway
  = Trusted Ingress
  + Household Account Groups
  + Private / Shareable Information Policy
  + Voice And Multimodal Risk
  + Privacy Gateway
  + Action Policy
  + Step-up Approval
  + Explainable Audit
  + Data Lifecycle
```

第一版不做企业 IAM，不做复杂组织树，只保留家庭场景必须有的账号和信息边界。

## 3. 账号分组

HarborNavi 账号分组应复用现有 `Workspace / UserAccount / IdentityBinding / Membership` 模型。产品上可以解释为一个“家庭空间”里的几个简单分组。

| 分组 | 定位 | 默认能力 | 约束 |
| --- | --- | --- | --- |
| `root` | HarborNavi 本机根账号 / 家庭空间拥有者 | 首次初始化、设备归属、恢复、迁移、核心安全策略 | 不作为日常使用账号；访问成员私有信息应走显式确认和审计 |
| `admin` | 家庭管理员 | 管理设备、成员、分享范围、审批高风险动作 | 管理权不等于默认读取所有成员私有信息 |
| `member` | 家庭成员 | 使用普通设备、访问已共享信息、管理自己的私有信息 | 默认不能访问其他成员私有信息 |
| `guest` | 临时访客 | 使用被授权的少量场景或设备 | 默认无家庭时间线、摄像头、成员资料访问权 |
| `system` | 系统服务 / 自动化 / 设备事件 | 触发规则、写入事件、生成审计 | 不能伪装成家庭成员，不能绕过 policy |

这里的 `root` 是产品语义上的 HarborNavi 根账号，不等同于 Linux `root`。实现上可以映射为 `Workspace.owner_user_id` 加本机恢复凭据，避免在普通 UI 里鼓励 root 日常登录。

## 4. 私有信息与可分享信息

每条家庭信息至少要能回答三个问题：

1. 这是谁的信息？
2. 谁能看？
3. 能不能离开本地？

P0 只需要五种分享状态：

| 状态 | 说明 | 例子 |
| --- | --- | --- |
| `private` | 仅本人可见 | 个人偏好、个人记忆、私人对话、声纹特征 |
| `home_shared` | 家庭成员可见 | 家庭购物清单、共享日程、普通设备状态 |
| `role_shared` | 指定角色可见 | 管理员可见的安全告警、root/admin 可见的系统日志 |
| `temporary_shared` | 限时分享 | 临时摄像头链接、访客一次性门禁 |
| `system_only` | 只给系统策略使用，不直接展示 | spoof risk、模型路由证据、内部评分、设备健康元数据 |

默认规则：

- 成员个人记忆、声纹、会话历史、位置、健康、私人物品识别结果默认为 `private`。
- 家庭公共设备状态、普通场景、公共房间事件可以是 `home_shared`。
- 门锁、报警、摄像头关闭、数据删除、分享链接属于高风险动作，至少需要 `admin` 或 `root` 审批。
- `admin` 可以配置分享规则，但不能默认静默读取 `member` 的 `private` 内容。
- `root` 可以做设备恢复和所有权迁移，但读取私有信息应有本地确认、原因记录和审计。

## 5. 权限决策输入

Trust Gateway 每次决策至少考虑：

- `requester_user_id`
- `requester_role`
- `source_kind`：WebUI / App / IM / voice / automation / device event
- `subject_user_id`：这条信息涉及谁
- `resource_scope`：home / room / device / file / memory / camera / automation
- `share_state`：private / home_shared / role_shared / temporary_shared / system_only
- `action_risk`：L0 / L1 / L2 / L3
- `destination`：local / redacted_cloud / cloud
- `approval_state`

简化后的判断可以理解为：

```text
requester + role + source + subject + share_state + action_risk + destination
  -> allow / step_up / redact / local_only / deny
```

## 6. 与 Privacy Gateway 的关系

Privacy Gateway 负责“信息能不能离开家”。Account Groups 负责“谁能访问这条信息”。两者必须一起工作：

- 如果信息是 `private`，默认不能进入云端 raw prompt。
- 如果目的地是 `redacted_cloud`，只能上传 task-minimal semantic capsule。
- 如果请求者不是信息主体，也没有分享权限，先在本地拒绝或请求授权。
- 如果家庭管理员要求查看成员私有信息，必须走 step-up，并写入可解释审计。

## 7. 与语音入口的关系

语音只提供意图和部分身份线索，不提供最终授权。语音请求进入 Trust Gateway 后，需要绑定到家庭账号和分享状态：

- 声纹像某个成员，只能提高 `household identity confidence`。
- 语音来自外部 capture source 时，必须记录 source provenance。
- 如果请求访问其他成员 `private` 信息，即使声纹置信度高，也不能直接放行。
- L2/L3 动作仍然必须走 step-up。

详细语音策略见 [harbornavi-voice-trust-model.md](./harbornavi-voice-trust-model.md)。

## 8. P0 落地范围

P0 不需要做复杂权限编辑器，只需要：

1. `root / admin / member / guest / system` 五类产品分组。
2. `private / home_shared / role_shared / temporary_shared / system_only` 五类信息分享状态。
3. L2/L3 动作必须 step-up。
4. 云端 fallback 只接收 semantic capsule，除非明确允许 raw cloud。
5. 每次拒绝、脱敏、审批、分享都生成 metadata-only audit。
6. UI 用普通语言解释“为什么允许 / 为什么需要确认 / 哪些信息被分享”。

## 9. 非目标

第一版不做：

- 企业级 RBAC / ABAC 策略编辑器
- 多层组织架构
- 成员之间复杂委托链
- 默认让 root/admin 读取所有成员私有内容
- 把第三方 IM 账号或语音声纹当成最终身份事实
- 未经确认的跨家庭分享

## 10. 验收门槛

1. 新家庭空间必须有一个 `root` / owner 归属。
2. 至少能创建 `admin`、`member`、`guest` 三类家庭账号。
3. 成员私有信息默认不可被其他成员、guest、普通自动化读取。
4. admin/root 管理动作和私有信息访问动作在审计中可区分。
5. 可分享信息必须有分享范围和可撤销状态。
6. 云端调用必须能证明使用了 local-only、semantic capsule 或明确 raw allowed 之一。
