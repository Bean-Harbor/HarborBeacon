# HarborBeacon Local Agent 文档索引（V2 + Home Agent Hub）

## 1. 目标类文档

1. HarborBeacon-LocalAgent-Plan.md
: 全局架构与能力边界（个人助手/多模态RAG/智能编排）。
2. HarborBeacon-LocalAgent-Roadmap.md
: 平台主干路线图与任务分配（长期骨干，不再单独代表当前唯一产品执行线）。
3. HarborBeacon-LocalAgent-V2-Assistant-Skills-Roadmap.md
: V2 技术路线补充与执行优先级说明。
4. [docs/platform-home-agent-hub-unified-architecture.md](docs/platform-home-agent-hub-unified-architecture.md)
: 平台主干 + Home Agent Hub 首个垂直域的统一架构与迁移路线。

## 2. 执行与落地类文档

1. HarborBeacon-LocalAgent-QuickRef.md
: 框图、路由规则、统一任务对象、每周交付要求。
2. HarborBeacon-LocalAgent-LaunchChecklist.md
: 启动阶段分周检查清单（T+0/T+7/T+14/T+30）。
3. HarborBeacon-LocalAgent-MeetingGuide.md
: 评审会议流程与讨论问题。
4. [docs/home-agent-hub-roadmap.md](docs/home-agent-hub-roadmap.md)
: Home Agent Hub MVP 当前产品执行线（摄像头发现 -> 抓图 -> AI 检测 -> IM 闭环）。
5. [docs/home-agent-hub-phase-backlog.md](docs/home-agent-hub-phase-backlog.md)
: Home Agent Hub MVP 分阶段 backlog。
6. [docs/camera-domain-task-contract.md](docs/camera-domain-task-contract.md)
: 首批 `camera.*` domain action 与 Task API 最小契约。

## 3. 契约与治理类文档

1. HarborBeacon-Harbor-Collaboration-Contract-v3.md
2. HarborBeacon-Harbor-Collaboration-Contract-v2.md
3. HarborBeacon-Skill-Spec-v1.md
4. HarborBeacon-Middleware-Endpoint-Contract-v1.md
5. HarborBeacon-Files-BatchOps-Contract-v1.md
6. HarborBeacon-Planner-TaskDecompose-Contract-v1.md
7. HarborBeacon-Contract-E2E-Test-Plan-v1.md
8. HarborBeacon-CI-Contract-Pipeline-Checklist-v1.md
9. HarborBeacon-GitHub-Actions-Workflow-Draft-v1.md

## 4. HarborBeacon 与 IM 接入层

HarborBeacon 是基于 ZeroClaw 二次开发的 AI 助手，预装在 HarborOS 中（与 HarborOS 运行在同一台机器上）。
用户通过 IM（飞书 / 企业微信 / Telegram / Discord / 钉钉 / Slack / MQTT）与 HarborBeacon 交互，HarborBeacon 通过 CLI、MCP、API 等形式控制 HarborOS 各项功能。

代码包: `harborbeacon/`

| 模块 | 职责 |
|---|---|
| `channels.py` | IM 通道注册、消息收发、意图路由 |
| `mcp_adapter.py` | MCP 工具列表 / 调用适配器（ReadOnly 守卫、逐次审批令牌） |
| `autonomy.py` | 自主级别 (ReadOnly / Supervised / Full) 与风险映射 |
| `tool_descriptions.py` | 将 skill manifest 转换为 MCP / TOML 工具描述 |
| `api/` | Settings REST API（GET/PUT /settings、连通性测试、路由状态） |
| `webui/` | Angular 21 Settings 页面（HarborOS WebUI 集成模块） |

## 5. 当前版本口径

- 长期北极星: HarborOS 个人助手 + 多模态RAG + 智能编排平台。
- 当前产品执行线: Home Agent Hub 摄像头 MVP。
- 统一目标入口: IM / Web / Mobile → HarborBeacon → Orchestrator Runtime → Domain Skills。
- 路由规则: `middleware API > midcli > browser > MCP`。
- 自主级别: ReadOnly（只读安全）/ Supervised（需审批）/ Full（完全自主）。
- 发布门禁: contract/e2e/drift/release gate 必须可执行。
- 模型架构: Runtime Manager + Model Center 是共享能力层，默认走 Harbor-managed Candle-first local runtime；cloud fallback 只进入 `semantic.router` 与 `retrieval.answer`。

## 5.1 当前模型架构权威入口

1. HarborBeacon-Harbor-Collaboration-Contract-v3.md
: 当前 HarborBeacon Core 架构契约，冻结 Model Center、RAG、Privacy Gateway、workflow compiler shadow、热路径与多 lane 边界。
2. HarborBeacon-Harbor-Collaboration-Contract-v2.md
: 上一版多 lane 协作契约，保留为历史/过渡参考。
3. HarborBeacon-LocalAgent-Plan.md
: 记录 Harbor-managed Candle-first、ISO bootstrap LLM、SiliconFlow fallback 与 HF mirror 的当前实现决策。
4. HarborBeacon-LocalAgent-Roadmap.md
: 记录 Runtime Manager、Candle / sidecar / external endpoint 均通过 Model Center 与 route policy 收敛。
5. docs/local-model-backend-benchmark-gate.md
: 记录本地 runtime promotion gate、bootstrap model gate 与“Candle 是默认主力但不是唯一方向”。
6. README.md 与 docs/webui-information-architecture.md
: 记录 HarborDesk 配置入口、endpoint secret redaction、HF mirror 操作位。

## 6. 阅读顺序（新成员）

1. [docs/harborbeacon-new-collaborator-brief.md](docs/harborbeacon-new-collaborator-brief.md)
: 当前协作者快速现状说明，先读这个再进入细分文档。
2. HarborBeacon-Harbor-Collaboration-Contract-v3.md
: 当前 HarborBeacon Core 架构与协作边界。
3. [docs/platform-home-agent-hub-unified-architecture.md](docs/platform-home-agent-hub-unified-architecture.md)
4. [docs/home-agent-hub-roadmap.md](docs/home-agent-hub-roadmap.md)
5. [docs/camera-domain-task-contract.md](docs/camera-domain-task-contract.md)
6. HarborBeacon-LocalAgent-Roadmap.md
7. HarborBeacon-LocalAgent-Plan.md
8. 契约与治理文档组
