# HarborBeacon 新同事项目导览

更新时间：2026-05-01

这份文档面向第一次接触 HarborBeacon 的协作者。先不用急着读所有契约、runbook 和 daily closeout，先建立一个完整心智模型：这个项目要解决什么问题、整体架构怎么分层、现在已经实现了哪些能力、接下来主要推进什么。

## 1. 项目目标

HarborBeacon 的目标是成为 HarborOS 上的本地优先智能体核心。

用户可以用自然语言，通过 IM、WebUI 或本地 API 让 HarborBeacon 帮他完成四类事情：

1. 查询和理解个人数据，例如文档、图片、视频、知识库内容。
2. 操作 HarborOS 和 NAS 能力，例如服务状态、文件任务、系统管理动作。
3. 连接和控制 Home AIoT 设备，例如摄像头发现、抓图、开流、PTZ、回放、ONVIF / RTSP / vendor bridge、局域网设备控制和设备媒体归档。
4. 编排更长流程，例如需要补充信息、需要审批、产生 artifact、后续继续对话的任务。

它不是单纯的聊天机器人，也不是某个 IM 平台 adapter。它更像 Harbor 系统里的业务大脑：理解用户意图，维护会话和任务状态，决定要不要调用工具、知识库、HarborOS 能力或 Home AIoT 设备，记录审批、产物和审计，然后把结果交给北向入口返回给用户。

其中 Home AIoT 不是外围功能，而是 HarborBeacon 南向能力的关键目标之一：HarborBeacon 要能把自然语言请求落到真实家庭设备上，同时把设备发现、设备控制、媒体采集、artifact、审批和审计放进同一个可恢复的业务流程里。

## 2. 一句话定位

HarborBeacon 是 Harbor 多仓体系里的 business-core repo。

- 北向入口可以来自 IM、WebUI、本地 API 或自动化。
- Core 层负责 conversation、task、workflow、planner、policy、RAG、approval、artifact、audit。
- 南向执行连接 HarborOS、模型服务、知识检索、Home AIoT 设备连接与控制、外部 delivery 通道。

外部 HarborGate repo 负责 IM transport，HarborBeacon 不负责 IM 平台细节。

## 3. 整体框架

可以先把 HarborBeacon 理解成三层：

```text
┌──────────────────────────────────────────────────────────────┐
│ 北向 Northbound                                               │
│ Harbor Assistant / IM / API / local automation           │
│ 管理配置、用户助手、检索请求、设备请求、系统任务                 │
└──────────────────────────────┬───────────────────────────────┘
                               │
┌──────────────────────────────▼───────────────────────────────┐
│ HarborBeacon Core                                             │
│ conversation / task / workflow state                          │
│ planner / router / policy / approvals                         │
│ RAG / model policy / artifacts / audit                         │
└───────────────┬───────────────────────────────┬──────────────┘
                │                               │
┌───────────────▼────────────────┐  ┌───────────▼────────────────┐
│ 南向 Southbound: HarborOS       │  │ 南向 Southbound: Data/AIoT   │
│ middleware API -> midcli        │  │ knowledge / model / devices  │
│ -> browser -> MCP               │  │ native adapters / LAN / media│
└────────────────────────────────┘  └────────────────────────────┘
```

## 4. 北向入口

北向就是用户或外部系统怎么进入 HarborBeacon。

当前主要入口：

- Harbor Assistant：面向管理员和高级用户的管理入口，负责 source roots、index、模型 endpoint、privacy / resource profile、runtime truth、gateway status 等配置和运行态查看。
- Search：面向普通用户的助手入口，承接自然语言问答、多模态知识检索、内容预览、引用结果和后续任务发起。
- IM 入口：由 HarborGate 接入微信等 IM 平台，再通过 HTTP/JSON 把标准化后的 turn 发给 HarborBeacon。
- 本地 API：HarborBeacon 暴露任务、检索、模型配置、runtime truth 等 API。
- 自动化 / 运维入口：release gate、drift matrix、E2E 工具可以调用 runtime 做验证。

Harbor Assistant 和 Search 是需要重点关注的一等北向产品面，不是临时 demo 页面。正式产品面在 HarborNAS WebUI 中集成；本仓的 `frontend/harbor-assistant/` 更像验证 shell，用来确保 HarborBeacon API 和交互模型成立。

北向的关键原则是：入口可以很多，但进入 Core 后都要变成可追踪、可审计、可恢复的业务请求。

## 5. HarborBeacon Core

Core 是这个项目最重要的部分。它负责把“用户说了一句话”变成“可执行、可追踪、可继续”的业务流程。

核心能力包括：

- Conversation continuity：维护业务会话，不把 transport session 当作业务真相。
- Task / workflow state：保存任务状态、执行进度、失败原因和恢复点。
- Planner：把自然语言意图拆成步骤。
- Router：决定走哪个能力、哪个 domain、哪个 executor。
- Policy：处理权限、风险等级、本地优先、fallback、审批策略。
- Approval：高风险动作需要用户确认。
- Artifact：任务过程中产生的文件、截图、视频、检索结果等产物。
- Audit：记录 trace、task、route、决策、执行结果，方便回放和追责。
- RAG / knowledge：对文档、图片等内容做索引、检索、引用和回答。
- Model policy：管理本地模型、OpenAI-compatible endpoint、fallback 和 runtime readiness。

Core 层的设计重点不是“能不能调到某个工具”，而是调工具前后所有业务状态都可解释、可恢复、可审计。

## 6. 南向执行

南向就是 HarborBeacon 真正调用能力的地方。目前要分清几个 domain。

### HarborOS System Domain

这是控制 HarborOS 系统能力的路径，例如服务状态、文件操作、系统任务。

执行优先级固定为：

```text
middleware API -> midcli -> browser -> MCP
```

意思是：

- 有稳定 middleware API 时优先用 API。
- API 不够时再用 `midcli`。
- Browser / MCP 是 fallback，不是首选路径。
- 高风险动作必须走审批和 dry-run / preview 逻辑。

### Knowledge / Model Domain

这部分服务 Harbor Assistant / RAG / 本地优先智能体能力。

已经具备的方向：

- 文档和图片进入统一检索主线。
- Search 使用真实 knowledge search / preview API。
- Model Center 可以配置本地或 OpenAI-compatible endpoint。
- runtime truth 通过 feature availability / inference health 等接口暴露。
- 本地模型 backend promotion 需要 benchmark gate 证明，不能只靠口头判断。

### Home Device / AIoT Domain

这是摄像头、LAN device、ONVIF / RTSP、设备媒体和设备控制相关方向。

它和 HarborOS System Domain 不是一回事：

- HarborOS 可以提供存储、归档、策略、协调。
- 设备控制本身应优先走设备原生 adapter / LAN bridge。
- 不要把摄像头和 AIoT 控制简单塞进 HarborOS system control。

### IM Delivery Domain

实际 IM 平台投递属于 HarborGate，不属于 HarborBeacon。

HarborBeacon 只生成业务回复、artifact 和平台无关的 delivery hint。HarborGate 再负责把它变成具体平台消息。

## 7. 一次请求怎么流动

以 IM 用户发一句“帮我找最近包含人物的照片，并总结一下”为例：

1. 用户在 IM 里发消息。
2. HarborGate 接收平台事件，归一化成 turn request。
3. HarborBeacon 接收 turn，找到或创建业务 conversation。
4. Core 判断这是 knowledge / multimodal retrieval 类型任务。
5. Planner / Router 选择检索和回答路径。
6. Knowledge API 检索文档或图片索引，返回 evidence。
7. Core 生成回答，附带引用、预览或 artifact。
8. Audit 记录 trace、turn、检索依据和结果。
9. HarborBeacon 返回 reply / artifacts / delivery hints。
10. HarborGate 负责把回复投递回 IM。

同样，如果用户说“重启某个服务”，Core 会先判断这是 HarborOS System Domain，而且是高风险或中高风险动作，通常需要 preview 和 approval，然后才会走 middleware / midcli 执行。

## 8. 已实现的主要能力

按功能视角看，目前已经有这些基础：

### Conversation / Turn Core

- v2.0 turn 模型已经成为当前 active seam。
- HarborBeacon 使用业务 conversation handle 和 active frame 维护连续对话。
- 支持 clarification、cancel、frame prompt、普通 conversation act。
- 不再把 IM transport session 当作业务会话真相。

### HarborGate 协作边界

- HarborGate 负责 IM adapter、route key、平台凭据和 outbound delivery。
- HarborBeacon 负责业务状态、审批、artifact、audit 和 conversation continuity。
- 两边通过 HTTP/JSON contract 通信，不互相 import runtime code。

### Harbor Assistant

- Harbor Assistant 是完整的管理和配置北向入口，围绕 source roots、index、模型 endpoint、privacy / resource profile、runtime truth、gateway status 等能力。
- Search 是完整的用户助手和检索北向入口，面向 multimodal / knowledge retrieval、自然语言问答、内容预览、引用结果和后续任务发起。
- 两者构成 WebUI 方向最重要的产品化入口：Harbor Assistant 管配置和运行态，Search 管用户使用和知识交互。
- 两者都应使用真实 `/api/beacon/*` API，不应新增 mock 或 demo shortcut。

### Knowledge Search / Preview

- 支持 knowledge search 和 preview。
- 文档、图片等内容索引已经进入主线。
- 检索结果会带 evidence 字段，用于说明命中来自内容、索引或文件名等来源。

### Model Center / Local-First Policy

- 支持配置本地模型和 OpenAI-compatible endpoint。
- 支持本地优先、fallback、runtime health / readiness 观测。
- 当前还需要 `.82` local model promotion report，才能宣称默认本地 backend 真正 promotion。

### Release / Drift / Gate 工具

仓库里有 Rust 工具支持契约和发布治理：

- `validate-contract-schemas`
- `run-e2e-suite`
- `run-drift-matrix`
- `evaluate-release-gate`
- `benchmark-local-model-backend`

这些工具帮助团队避免文档、契约、实现和发布状态漂移。

## 9. 当前阶段

当前处于 post-RC2 收口和 GA candidate 前准备。

已经完成：

- HarborBeacon、HarborGate、WebUI 相关 release train 已合并。
- RC2 已安装到 `.82`。
- Harbor Assistant live smoke 已验证。
- knowledge search / preview 已验证。
- protected turn API 的内容检索和 local-first 架构解释已验证。

当前不要急着扩大功能面，优先做：

1. 补齐 release evidence 和 rollback notes。
2. 落地 Runtime Manager 的 Harbor-managed Candle-first 路线：Candle 默认 `enabled/idle`，默认路径不依赖用户 `127.0.0.1:11434`。
3. 把约 0.5B bootstrap LLM 纳入 ISO / first-boot 路线，只用于 IM / WebUI 自然语言入口、意图分类、参数抽取和配置引导。
4. 决定 RC2 是否可以推进为 GA candidate。
5. 继续产品面硬化，但不改变 v2.0 public contract。
6. GA / Candle-first bootstrap gate 后再恢复 Home Agent Hub / AIoT MVP。

## 10. 新同事先读哪些文档

建议顺序：

1. 本文，先建立全局心智模型。
2. `README.md`，了解 repo、二进制和常用命令。
3. `HarborBeacon-Harbor-Collaboration-Contract-v3.md`，了解当前 HarborBeacon Core 架构、多 lane owner、热路径和边界。
4. `HarborBeacon-LocalAgent-Plan.md`，了解长期目标。
5. `HarborBeacon-LocalAgent-Roadmap.md`，了解当前路线和优先级。
6. `HarborBeacon-HarborGate-v2.0-Upgrade-Runbook.md`，了解当前 post-RC2 状态。
7. `docs/daily/2026-04-30.md`，了解最近一次 closeout。

如果他要改某个方向，再补读对应文档：

- IM / Gate seam：`C:\Users\beanw\OpenSource\HarborGate\HarborBeacon-HarborGate-Agent-Contract-v2.0.md`
- Cutover / rollback：`docs/im-v2.0-cutover-rollback-observability-gates.md`
- 本地模型：`docs/local-model-backend-benchmark-gate.md`
- Home Agent Hub：`docs/home-agent-hub-roadmap.md`
- Camera / AIoT：`docs/camera-domain-task-contract.md`
- Skills：`HarborBeacon-Skill-Spec-v1.md`

## 11. Repo 快速地图

| 路径 | 作用 |
|---|---|
| `src/runtime/` | turn / task API、session、业务 runtime |
| `src/control_plane/` | admin、model、runtime truth、配置面 |
| `src/orchestrator/` | router、policy、executor 编排 |
| `src/planner/` | task decomposition |
| `src/skills/` | HarborBeacon runtime skills |
| `src/connectors/` | HarborOS / 外部 connector abstraction |
| `src/domains/` | system / device 等 domain boundary |
| `src/bin/` | Rust binaries 和 release / benchmark tools |
| `frontend/harbor-assistant/` | Harbor Assistant validation shell |
| `tests/contracts/` | contract / IM seam / drift tests |
| `tests/fallback/` | route priority / fallback tests |
| `tests/policy/` | policy and governance tests |
| `docs/` | runbooks、evidence、domain docs |
| `docs/daily/` | daily sync 和 architect closeout |

## 12. 最重要的边界

这些先记住就够了：

- HarborBeacon 是业务核心，不是 IM adapter。
- HarborGate 负责 IM transport、平台凭据、route key 和 delivery。
- HarborBeacon 不直接投递 IM 平台消息。
- HarborBeacon 不长期保存或校验 IM 原始凭据。
- HarborBeacon 不解释 `route_key` 的平台语义。
- v2.0 是当前 active contract，v1.5 只是历史参考。
- 当前不做 v1.5 / v2.0 runtime dual stack。
- 当前不做 group chat。
- HarborOS system control 和 AIoT device control 要分开。

## 13. 新同事适合先接的任务

适合第一阶段上手：

- 阅读并整理某一块文档的 current status。
- 补 release evidence、rollback notes、daily closeout。
- 给现有 contract / policy / fallback 行为补测试。
- 跑 local model benchmark 并整理结果。
- 梳理 Harbor Assistant 使用的真实 API 和 evidence。
- 修小范围文档链接、术语统一和 onboarding 改善。

不建议一上来做：

- 重新设计 Beacon / Gate contract。
- 把 IM adapter 或平台凭据加回 HarborBeacon。
- 给 v2.0 加 v1.5 runtime compatibility。
- 改 release / rollback gate 语义。
- 把 AIoT device control 合并进 HarborOS system control。

## 14. 常用验证命令

最常见的本地验证：

```powershell
cargo build --release
cargo test
pytest
```

前端 validation shell：

```powershell
cd frontend/harbor-assistant
npm run build
```

发布和契约工具：

```powershell
target/release/validate-contract-schemas
target/release/run-e2e-suite
target/release/run-drift-matrix
target/release/evaluate-release-gate
```

## 15. 记住这张图

```text
用户 / 系统
   │
   ▼
北向入口：Harbor Assistant / IM / API / automation
   │
   ▼
HarborBeacon Core：
理解意图 -> 维护会话和任务 -> 规划 -> 路由 -> 策略/审批
   │
   ├─ Knowledge / RAG / Model
   ├─ HarborOS middleware / midcli
   ├─ Device / AIoT adapters
   └─ Artifact / Audit / Notification intent
   │
   ▼
结果返回：
WebUI 直接展示，IM 由 HarborGate 投递
```

如果只带走一句话：HarborBeacon 的核心价值是把自然语言入口、业务状态、智能编排、知识检索、系统控制和审计闭环连起来，同时守住 HarborGate / HarborOS / AIoT 的边界。
