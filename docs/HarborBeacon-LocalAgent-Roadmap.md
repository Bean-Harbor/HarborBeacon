# HarborBeacon Local Agent V2 路线图与任务分配

> 当前定位说明（2026-04-30）
> 本文档当前执行线已从 `HarborBeacon x HarborGate Contract v2.0 upgrade`
> 进入 post-RC2 收口与下一阶段规划。
> 双仓边界已经明确：IM 仓库负责 `adapter/gateway/route/平台凭据/delivery`，HarborBeacon 负责 `conversation turn/business state/active frame/approval/artifact/audit`。
> 两边只通过 HTTP/JSON contract 通信，不互相 import，也不共享 `.harborbeacon/*.json`。  
> 本仓库只负责 HarborBeacon 侧工作项；IM 仓库能力只作为外部依赖与联调对象跟踪。  
> 协作术语统一以 `HarborBeacon-Harbor-Collaboration-Contract-v3` 与 `harbor-*` lane 命名为准。
>
> 当前进展快照（2026-04-30）
> 已合并：HarborBeacon v2.0 turn core、MMRAG/knowledge search/preview、VLM sidecar packaging、Harbor Assistant validation/docs、HarborGate v2.0 delivery guard、HarborNAS WebUI Harbor Assistant/Search native pages。
> 已部署：`.82` post-merge RC2
> `20260430-rc2-beacona5f6da0-gate57ff759`。
> 已验证：`/ui/harbor-assistant`、`/ui/harbor-assistant?tab=search`、knowledge search/preview、protected
> `POST /api/web/turns` content retrieval and local-first architecture explanation；
> `/api/turns` 仅作为 deprecated alias 保留。
> 下一阶段：先补 release evidence/rollback notes，再推进 Runtime Manager 的
> Harbor-managed Candle-first 路线和 ISO bootstrap LLM，最后恢复 Home Agent
> Hub / AIoT MVP 队列。
>
> 本文后续早期 v1.5 phase 描述保留为历史上下文；当前执行、验收与回滚以
> `HarborBeacon-HarborGate-v2.0-Upgrade-Runbook.md` 和外部 v2.0 contract 为准。

## 1. 目标重申（真实北极星）

本项目不是文档工程，而是在 HarborOS 基础上落地:

1. 个人助手（多终端自然语言交互）
2. 多模态 RAG（文本/图像/音频/视频）
3. 智能编排（规划、路由、执行、审计、回滚）

当前执行策略补充：

- `Local Agent V2` 负责平台骨干
- `Home Agent Hub` 负责首个垂直域产品闭环
- Home Agent Hub 已验证出的 artifact / event / long-running task / 补参机制，后续应反哺平台抽象
- 模型能力按共享能力层治理，不再把 Candle、sidecar、Mistral 或 SiliconFlow 各自写成业务域；统一通过 Runtime Manager、Model Center endpoint 与 route policy 决策。
- 默认产品体验调整为 Harbor-managed Candle-first：Candle 负责默认 LLM / embedding runtime，约 0.5B bootstrap LLM 随 `harboros-beacon.deb` 进入 ISO 并提供基础自然语言入口；OpenAI-compatible endpoint 只作为高级外接配置，不自动复用用户 `127.0.0.1:11434`。
- 当前模型路线保持 local-first；云端 fallback 只进入 `semantic.router` 与 `retrieval.answer`，并要求 endpoint redaction、attempt audit 和 policy gate。
- Hugging Face 模型下载走 mirror-aware download job：Harbor Assistant 输入 mirror 优先，其次 `HF_ENDPOINT`，最后默认 `https://hf-mirror.com`。

执行优先级强约束:

1. `Middleware API`
2. `MidCLI`
3. `Browser`
4. `MCP`

---

## 2. V2 分阶段路线图（12 周）

### Phase 0（Week 1）边界冻结与现状盘点

交付物:

- v1.5 双仓边界在 HarborBeacon 侧冻结
- HarborBeacon 现有 IM 耦合点清单（直连发送、凭据校验、状态共享假设）
- cutover 特性开关与回滚预案草案

关键任务:

- 逐项确认 HarborBeacon 只保留 `task/business state/approval/artifact/audit`
- 盘点 `assistant_task_api`、`runtime/task_api`、`admin_console` 中的 IM 平台耦合
- 建立 HarborBeacon 侧 contract regression 基线

### Phase 1（Week 2-3）Inbound Task Contract 对齐

交付物:

- HarborBeacon `POST /api/tasks` 对齐 v1.5 inbound contract
- 统一支持 `X-Contract-Version: 1.5`、服务鉴权、RFC 3339 UTC
- 非 200 错误统一为 shared error envelope

关键任务:

- 扩展 `TaskRequest`，支持 `source.route_key` 与顶层 `message` block
- 建立基于 `task_id` 的幂等重放与冲突检测
- 固化 `task_id/trace_id/source.route_key/message.message_id` 观测字段
- 为 legacy 非 IM caller 保留受控兼容路径

### Phase 2（Week 4-5）Business State / Approval / Artifact / Audit 加固

交付物:

- HarborBeacon 继续作为业务会话与流程状态唯一事实源
- `needs_input` / `resume_token` / approval / artifact / audit 全链路与 v1.5 对齐
- 会话状态中持久化 `route_key`，但不解释其平台语义

关键任务:

- 将 `route_key` 纳入业务会话持久化与恢复逻辑
- 审批、artifact、audit 记录补齐 `trace_id`、`task_id`、`route_key` 关联
- 明确附件下载信息为 opaque transport contract，不落入 HarborBeacon 平台逻辑
- 补齐会话恢复与长任务通知场景测试

### Phase 3（Week 6-8）Outbound Notification Intent Cutover

交付物:

- HarborBeacon 改为只生成 notification intent
- HarborBeacon 通过 HTTP 调用 HarborGate `POST /api/notifications/deliveries`
- HarborBeacon 不再直接投递 Feishu/Telegram/其他平台消息

关键任务:

- 将当前通知发送逻辑替换为 HarborGate client
- 对齐 `destination.route_key`、`delivery.mode`、`delivery.idempotency_key` 语义
- 按 v1.5 处理 accepted-request delivery failure 与 non-200 request rejection
- 增加通知链路的 contract / retry / observability 回归

### Phase 4（Week 9-10）管理面去凭据化与状态解耦

交付物:

- HarborBeacon 不再保存或校验 IM 平台原始凭据
- HarborBeacon UI / setup flow 改读 HarborGate redacted status
- 旧桥接配置迁移方案与兼容策略明确

关键任务:

- 移除 HarborBeacon 对 `app_id/app_secret/bot token` 的长期 ownership
- 将管理台连接状态改为消费 `GET /api/gateway/status` 一类 redacted 接口
- 清理 `bridge_provider`、凭据脚本、直连验证入口中的平台耦合
- 固化迁移期 feature flag 与灰度开关

### Phase 5（Week 11-12）联调发布与旧路径清理

交付物:

- HarborBeacon x HarborGate 双仓联调通过
- cutover 清单、回滚清单、验收报告完成
- HarborBeacon 旧直连 IM 路径完成下线

关键任务:

- 执行跨仓 E2E：inbound、resume、approval、artifact、notification
- 清理 HarborBeacon 残留的 IM 运行时代码与平台凭据校验逻辑
- 观察 cutover 后指标并收敛高优先级缺陷
- 输出 cutover 后 backlog，把更长期的 RAG / assistant 能力重新排期

---

## 3. 任务分配（按 `harbor-*` lane，不按文件）

## 3.1 当前 lane 定义

- `harbor-architect`
  跨 lane 边界治理、cutover 节奏、发布/回滚 gate、最终验收。
- `harbor-framework`
  HarborBeacon 共享 runtime、北向 contract、task/business state、approval、artifact、audit、本地推理、账号与智能编排。
- `harbor-im-gateway`
  外部 IM 仓库 owner，负责 adapter/gateway/route/平台凭据/outbound delivery；在本仓库中主要作为 contract 与联调协作者出现。
- `harbor-hos-control`
  HarborOS System Domain owner，负责 `Middleware API -> MidCLI -> Browser/MCP fallback` 这一条系统域 southbound。
- `harbor-aiot`
  Home Device Domain owner，负责 camera / AIoT / LAN device 的南向协议与控制，不与 HarborOS system control 混同。

当前阶段补充说明:

- HarborBeacon 仓库内当前主实施 owner 默认是 `harbor-framework`。
- 只有涉及跨 lane 边界、cutover 策略或发布 gate 时，`harbor-architect` 才作为批准与仲裁 owner 介入。
- `harbor-im-gateway` 在本表中出现时，表示外部协作与联调责任，不表示本仓库会直接实现 IM 仓库代码。

## 3.2 RACI（核心工作包）

| 工作包 | R | A | C | I |
|---|---|---|---|---|
| v1.5 contract 治理与兼容性 | `harbor-framework` | `harbor-architect` | `harbor-im-gateway` | 全员 |
| `assistant_task_api` 入站 contract 改造 | `harbor-framework` | `harbor-architect` | `harbor-im-gateway` | 全员 |
| 业务会话 / approval / artifact / audit | `harbor-framework` | `harbor-architect` | `harbor-im-gateway` | 全员 |
| 通知意图与 HarborGate client | `harbor-framework` | `harbor-architect` | `harbor-im-gateway` | 全员 |
| 管理台去凭据化与状态解耦 | `harbor-framework` | `harbor-architect` | `harbor-im-gateway` | 全员 |
| Planner 与路由策略 | `harbor-framework` | `harbor-architect` | `harbor-hos-control`, `harbor-aiot` | `harbor-im-gateway` |
| MiddlewareExecutor | `harbor-hos-control` | `harbor-architect` | `harbor-framework` | `harbor-im-gateway`, `harbor-aiot` |
| MidCLIExecutor | `harbor-hos-control` | `harbor-architect` | `harbor-framework` | `harbor-im-gateway`, `harbor-aiot` |
| Browser/MCP fallback | `harbor-hos-control` | `harbor-architect` | `harbor-framework` | `harbor-im-gateway`, `harbor-aiot` |
| Skills registry/runtime | `harbor-framework` | `harbor-architect` | `harbor-hos-control`, `harbor-aiot` | `harbor-im-gateway` |
| 多模态 RAG pipeline | `harbor-framework` | `harbor-architect` | `harbor-aiot`, `harbor-hos-control` | `harbor-im-gateway` |
| 可观测性与审计 | `harbor-framework` | `harbor-architect` | `harbor-im-gateway`, `harbor-hos-control`, `harbor-aiot` | 全员 |
| CI/CD 门禁与发布 | `harbor-architect` | `harbor-architect` | `harbor-framework`, `harbor-im-gateway`, `harbor-hos-control`, `harbor-aiot` | 全员 |

---

## 4. 每周执行节奏（建议）

1. 周一：里程碑对齐 + 风险评估（30 分钟）
2. 周三：技术评审（架构、接口、回归）
3. 周五：可运行演示（必须是端到端，不是 PPT）

每周必须产出:

- 可执行增量（代码 + 测试 + 文档）
- 指标快照（成功率、P95、fallback ratio、失败分类）
- 下周明确阻塞项与负责人

---

## 5. 里程碑验收标准（Definition of Done）

### M1（Week 3）

- HarborBeacon `POST /api/tasks` 可接受 v1.5 HarborGate request
- `source.route_key`、`message.message_id`、`task_id`、`trace_id` 均可被正确校验与记录
- 非 200 请求错误统一为 shared error envelope
- 核心回归测试通过

### M2（Week 5）

- HarborBeacon 继续独占 business session / resumable workflow / approval / artifact / audit 真相
- `needs_input`、`resume_token`、审批恢复链路可稳定工作
- 高风险操作仍必须确认，且审批结果与任务审计可关联回溯

### M3（Week 8）

- HarborBeacon 只向 HarborGate 发 notification intent，不再直接触达平台
- `route_key` 仅作为写入型路由元数据使用，不承载平台语义
- 通知链路支持 accepted-request delivery failure 与 retry 语义

### M4（Week 10）

- HarborBeacon 管理面不再长期保存或校验 IM 原始凭据
- 连接状态改由 HarborGate redacted status 提供
- 迁移期特性开关、灰度与回滚策略可用

### M5（Week 12）

- 双仓 cutover 联调完成且核心回归通过
- HarborBeacon 旧直连 IM 路径与残留平台凭据校验逻辑清理完成
- 发布评审通过，cutover 后 backlog 明确

---

## 6. V2 KPI（发布门禁）

1. HarborBeacon 侧 v1.5 contract 回归通过率 = 100%
2. `task_id` 幂等重放与冲突检测覆盖率 = 100%
3. 业务会话 / approval / artifact / audit 真相仍全部落在 HarborBeacon
4. HarborBeacon 直连平台消息投递次数在 cutover 后 = 0
5. HarborBeacon 持有原始 IM 平台凭据数量在 cutover 后 = 0
6. 双仓联调回归通过率 >= 98%

---

## 7. 当前建议优先级（post-RC2）

P0:

1. Land the post-RC2 docs-only closeout: RC2 evidence, rollback notes, current `.82/.197` targets, and the next-stage backlog.
2. Keep the v2.0 seam frozen: no v1.5/v2.0 runtime dual stack, no `/api/tasks` active fallback, no public `args.resume_token`.
3. Promote RC2 toward a GA candidate from merged mainline code only.
4. Land the Runtime Manager Candle-first cutover plan: default Candle runtime `enabled/idle`, no default user-Ollama/`11434` dependency, and healthz separates runtime alive from model loaded.
5. Add the HarborBeacon deb / ISO bootstrap LLM lane: bundle a ~0.5B Qwen-class model for IM / WebUI natural-language parsing and setup guidance, lazy-load it, and avoid presenting it as the full answer/RAG model.

P1:

1. Harden Harbor Assistant and Search as product surfaces while keeping them on real `/api/beacon/*` APIs through the HarborGate northbound edge.
2. Extend release packaging toward HarborNAS ISO integration without changing the v2.0 public contract.
3. Add local-first observability: fallback ratio, runtime/model readiness, policy decision evidence, bootstrap model usage, and failed-promotion reasons.

P2:

1. Resume Home Agent Hub / AIoT MVP after GA and Candle-first/bootstrap runtime decisions.
2. Continue Browser/MCP fallback optimization inside the HarborOS System Domain.
3. Re-rank orchestration, cost, cache, OCR/ASR, and multimodal expansion work after the local runtime proof.
