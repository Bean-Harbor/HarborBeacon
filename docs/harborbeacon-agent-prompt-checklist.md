# HarborBeacon Agent Prompt Checklist

## Purpose

This file is a reusable prompt pack for working in HarborBeacon with Codex or
similar coding agents.

Use it together with [AGENTS.md](/C:/Users/beanw/OpenSource/HarborBeacon/AGENTS.md).

`AGENTS.md` sets the project-wide rules.

This checklist gives task-specific prompt templates you can paste and fill in.

## Default Use Pattern

For most HarborBeacon tasks, start with this shell:

```text
先按 AGENTS.md 工作。

任务：

边界：

不要做：

验证：
```

Minimum rule:

- always state the task
- always state the boundary
- always state what must not change
- always state how to verify

## Quick Rules

Use these defaults unless the task clearly needs something else:

- If the task touches HarborBeacon <-> HarborGate HTTP contracts, say so explicitly.
- If the task removes legacy behavior, require rollback notes or a feature flag.
- If the task fixes a bug, require a failing test first.
- If the task changes shared runtime behavior, ask for the narrowest verification first, then widen if needed.
- If the task touches planner, router, policy, approvals, audit, or skills runtime, treat it as contract-first work.

## Core Templates

### 1. Cross-Repo Contract Work

Use when changing `assistant_task_api`, `runtime/task_api`, notification
delivery intent, `route_key`, `message` block handling, or shared error
envelopes.

```text
先按 AGENTS.md 工作。

任务：
检查 HarborBeacon 对 HarborGate v2.0 contract 的实现情况，并完成最小必要改动。

边界：
先读 HarborBeacon-Harbor-Collaboration-Contract-v3.md、HarborBeacon-LocalAgent-Roadmap.md、HarborBeacon-LocalAgent-Plan.md，以及 HarborGate 仓库里的 HarborBeacon-HarborGate-Agent-Contract-v2.0.md。
不要扩大 contract surface。
不要修改 HarborGate 仓库。

不要做：
不要顺手重构无关模块。
不要把平台语义重新带回 HarborBeacon。
不要重新引入 IM 凭据 ownership。

验证：
先补或更新 contract tests。
至少跑相关 pytest contract 用例。
如果改到共享行为，再补 cargo test。
```

### 2. Legacy Path Removal / Cutover

Use when removing direct IM send paths, old bridge code, old credentials flow,
or old fallback paths.

```text
先按 AGENTS.md 工作。

任务：
梳理并收敛 HarborBeacon 中的旧路径，只保留符合 v2.0 cutover 目标的实现。

边界：
必须保留迁移期 feature flag 或清晰的回滚点。
先列出当前调用点，再做最小迁移。

不要做：
不要直接硬删旧路径而不补回滚说明。
不要把多个迁移目标混成一次改动。

验证：
补迁移相关回归测试。
跑相关 pytest 用例。
如涉及 release gate，再跑 target/release/validate-contract-schemas。
```

### 3. Bug Fix / Root Cause Debugging

Use when tests fail, E2E breaks, or runtime behavior regresses.

```text
先按 AGENTS.md 工作。

任务：
修复下面这个问题，并按 root-cause 方式处理，不要做症状修复。

问题：
[粘贴错误、日志、失败测试名或复现步骤]

边界：
先复现，再定位失败层：middleware、midcli、planner/router、policy、runtime、frontend、tests。

不要做：
不要跳过失败测试。
不要靠放宽断言、跳过逻辑或关闭门禁来“修复”。

验证：
先让失败用例稳定复现。
修完先跑对应单测或 pytest 用例。
如果改动触达共享路径，再补 cargo test 或 build。
```

### 4. Skills Runtime / Planner / Router / Policy

Use when changing `src/skills/`, `skills/`, planner, router, policy,
approval, audit, or runtime contracts.

```text
先按 AGENTS.md 工作。

任务：
修改 HarborBeacon skills runtime / planner / router / policy 的相关实现，保持 contract-first。

边界：
先读 HarborBeacon-Skill-Spec-v1.md 和 HarborBeacon-LocalAgent-V2-Assistant-Skills-Roadmap.md。
保留 middleware -> midcli -> browser -> MCP 的优先级。
不要越过 lane 边界。

不要做：
不要把开发流程类 skills 混进产品 runtime skills。
不要引入自由拼接 shell 的危险执行方式。
不要改动与当前 capability 无关的 manifest 字段。

验证：
补 tests/test_skills、tests/test_orchestrator、tests/policy、tests/fallback 中相关用例。
必要时跑 cargo test。
```

### 5. HarborOS System Domain Work

Use when changing middleware integration, `midcli`, system operations, or
control-plane-first routing.

```text
先按 AGENTS.md 工作。

任务：
处理 HarborOS System Domain 的相关改动，保持 middleware first、midcli second。

边界：
归属 harbor-hos-control。
如果改动碰到 shared orchestration seam，按 harbor-architect 口径守边界。

不要做：
不要让 browser/MCP 抢在 API/CLI 前面。
不要绕过 approval/risk policy 直接执行高风险操作。

验证：
补相关 contract/fallback/policy tests。
必要时跑 target/release/run-e2e-suite。
```

### 6. AIoT / Camera / Device-Native Work

Use when changing ONVIF, RTSP, vendor bridge, camera hub, discovery, device
control, or media/control separation.

```text
先按 AGENTS.md 工作。

任务：
处理 Home Device Domain / camera / AIoT 的相关实现。

边界：
归属 harbor-aiot。
设备原生控制不能静默折叠进 HarborOS system control。
保持 media/control separation。

不要做：
不要把 AIoT 路由默认改成 HarborOS CLI。
不要把设备协议逻辑塞进 IM 或 business-state 层。

验证：
先跑最相关的 contract 或 regression 用例。
如果变更影响共享边界，再扩大验证范围。
```

### 7. Frontend / Harbor Assistant Admin Work

Use when changing `frontend/harbor-assistant`, `harborbeacon/webui`, model admin,
gateway status pages, or runtime truth surfaces.

```text
先按 AGENTS.md 工作。

任务：
修改 Harbor Assistant / WebUI 管理面相关逻辑。

边界：
保持 HarborBeacon 不长期拥有 IM 平台原始凭据。
优先消费 redacted status 或 runtime truth read-model。

不要做：
不要把管理面临时投影误写成系统真相。
不要引入与当前页面无关的 UI 重构。

验证：
至少跑前端 build：cd frontend/harbor-assistant && npm run build
如涉及后端接口语义，再补相应测试。
```

### 8. CI / Release Gate / Automation

Use when changing GitHub Actions, release checks, drift matrix logic, or
merge gates.

```text
先按 AGENTS.md 工作。

任务：
完善 HarborBeacon 的 CI/CD 和 release gate，保证 contract/fallback/policy/release checks 一致。

边界：
只做最小必要改动，不要为了通过而降低门禁。

不要做：
不要删检查。
不要把失败测试改成 skip。
不要把 release gate 改成只报 warning 而没有依据。

验证：
跑相关 pytest 用例、cargo build/test，以及所改动的 target/release 工具。
```

### 9. Release Readiness Review

Use when you want the agent to assess whether a lane or a cutover is ready.

```text
先按 AGENTS.md 工作。

任务：
从 harbor-architect 视角评审当前改动是否达到 cutover / release readiness。

边界：
重点看 frozen interfaces、rollback gate、observability gate、lane ownership、残留旧路径。

不要做：
不要只给泛泛总结。
先列风险和 blocker，再给结论。

验证：
引用实际代码、测试、contract 文档和 gate 工具结果。
```

### 10. Code Review Request

Use when you want findings first, not an implementation.

```text
先按 AGENTS.md 工作。

请对这组改动做代码评审。
重点找：
- contract 破坏
- lane boundary 污染
- rollback 风险
- 测试缺口
- 行为回归

不要做：
不要先讲优点。
先给 findings，按严重度排序。
```

## Verification Cheatsheet

Use the smallest relevant set first:

- Python tests: `pytest`
- Contract lane: `pytest tests/contracts -q`
- Fallback lane: `pytest tests/fallback -q`
- Policy lane: `pytest tests/policy -q`
- Skills lane: `pytest tests/test_skills -q`
- Orchestrator lane: `pytest tests/test_orchestrator -q`
- Rust build: `cargo build --release`
- Rust tests: `cargo test`
- Frontend build: `cd frontend/harbor-assistant && npm run build`
- Contract tool: `target/release/validate-contract-schemas`
- E2E tool: `target/release/run-e2e-suite`
- Drift tool: `target/release/run-drift-matrix`
- Release gate: `target/release/evaluate-release-gate <report>`

## Good Prompt Habits

- One task per prompt is better than three half-related tasks.
- Name the exact file, module, or lane when you know it.
- If you already know the contract doc that governs the work, name it.
- Say what must not change. This is often more important than saying what to build.
- For risky work, require tests before implementation and rollback notes before removal.

## Bad Prompt Patterns

Avoid prompts like these:

- `帮我顺手把这里也优化一下`
- `把这块都整理一下`
- `你看着改`
- `能跑就行`
- `测试先不用`

These usually cause scope creep, boundary drift, or unverifiable edits.

## Recommended Daily Pattern

For normal HarborBeacon coding sessions:

1. Start with `先按 AGENTS.md 工作。`
2. Paste one template from this file.
3. Fill in the exact task, boundary, and verification.
4. Let the agent inspect the relevant docs and files first.
5. If the task changes after investigation, start a fresh prompt instead of stacking unrelated goals into the same thread.
