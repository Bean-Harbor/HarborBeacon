# HarborBeacon HarborOS Control Collaboration Contract v1

> Superseded note (2026-04-18)  
> For the current multi-lane collaboration model, use
> `docs/HarborBeacon-Harbor-Collaboration-Contract-v3.md`
> as the primary coordination contract.  
> This file remains useful as historical HarborOS-control-specific context.

## Status

This document is the working freeze candidate for same-repo parallel
development of the HarborOS control surface.

It is intended to let three engineers work in parallel under one repo without
accidentally changing each other's boundaries:

- framework engineer
- IM engineer
- HarborOS control engineer (Engineer 3)

This document is additive. It does not replace, edit, or reinterpret the IM
Gateway contract.

## Normative Reference

The authoritative IM boundary remains the external file:

- `C:\Users\beanw\OpenSource\IM\HarborBeacon-HarborGate-Agent-Contract-v1.5.md`

Rules:

- That document remains the source of truth for:
  - `POST /api/tasks`
  - `TaskRequest` and `TaskResponse` semantics
  - `message` block semantics
  - `source.route_key` semantics
  - resumed turn semantics using `args.resume_token`
  - outbound notification delivery semantics
  - `X-Contract-Version`
- shared HTTP auth and error envelope rules
- This document governs only HarborOS control implementation inside the
  HarborBeacon repo.
- If this document conflicts with IM contract v1.5, IM contract v1.5 wins.
- HarborOS control work MUST NOT directly modify the external IM contract file.

## Purpose

Freeze the engineering boundary for HarborOS-domain execution so the HarborOS
control engineer can move fast without creating churn in IM or framework-owned
contracts.

The repository strategy for this phase is:

- one repo
- strong internal boundaries
- no unilateral changes to cross-team frozen contracts

## Scope

In scope for this document:

- HarborOS southbound connectors
- HarborOS route selection and fallback
- HarborOS service and filesystem execution mapping
- HarborOS validation, approval gate integration, and audit metadata
- HarborOS live integration plans, reports, and runbooks
- HarborOS-embedded deployment assumptions for the system domain

Out of scope for this document:

- IM platform adapters and transport logic
- IM payload formatting
- notification delivery contract evolution
- top-level task ingress contract redesign
- business conversation model redesign
- device-native control stacks for LAN AIoT devices unless they explicitly call
  into HarborOS system capabilities

## Deployment Shape

This project is not only "an assistant that operates HarborOS from outside".

The intended product shape is:

- when deployed on HarborOS, it becomes a built-in HarborOS capability
- when deployed on Debian 13 products, it becomes the same runtime hosted on a
  Debian 13 node
- northbound surfaces may include IM, Web, Mobile, or other clients
- southbound execution is domain-split, not single-route for all capabilities

HarborOS remains the platform base for:

- storage and NAS file access
- media archive and artifact persistence
- local RAG and multimodal data pipelines
- permissions, audit, and long-running governance
- first-party HarborOS service and system operations

## Southbound Domain Split

The runtime has at least two distinct southbound domains and they MUST NOT be
collapsed into one routing policy.

### 1. HarborOS System Domain

This domain covers first-party HarborOS and NAS capabilities such as:

- service lifecycle operations
- filesystem and storage operations
- HarborOS-managed media, archive, and system functions
- HarborOS-backed local data, indexing, and platform-side AI support functions

This domain keeps the established routing rule:

- `Middleware API -> MidCLI -> Browser/MCP fallback`

### 2. Home Device Domain

This domain covers LAN AIoT devices such as:

- cameras
- sensors
- other local-network devices
- vendor-specific device adapters and bridges

This domain should prefer device-native execution:

- `Native Adapter -> LAN Bridge -> HarborOS Connector -> Cloud/MCP`

Meaning:

- device control should not default to HarborOS CLI or HarborOS middleware when
  a native device adapter exists
- HarborOS may still act as storage, archive, policy, or coordination platform
  for device workflows
- camera media may be stored in NAS or HarborOS storage while device control
  itself remains device-domain work

## Hard Boundary Rules

- HarborOS control work MUST NOT modify
  `C:\Users\beanw\OpenSource\IM\HarborBeacon-HarborGate-Agent-Contract-v1.5.md`.
- HarborOS control work MUST NOT redefine or silently widen the meaning of:
  - `task_id`
  - `trace_id`
  - `source.route_key`
  - `message.message_id`
  - `args.resume_token`
  - `TaskResponse.status`
  - notification request or response fields
- HarborOS control work MUST NOT introduce IM-platform credentials into
  HarborBeacon control code.
- HarborOS control work MUST NOT directly deliver IM platform messages.
- HarborOS control work MUST NOT import IM Gateway repo code, adapter code, or
  platform SDK-specific runtime code as part of HarborOS execution.
- HarborOS control work MUST stay behind Router, policy, and approval gates.
- HarborOS-domain actions MUST continue to prefer HarborOS-native execution
  routes over browser or MCP fallbacks unless the framework owner explicitly
  expands the routing rules.
- HarborOS control work MUST NOT silently absorb Home Device Domain ownership by
  treating all southbound integrations as HarborOS system work.

## Ownership Split

- Framework engineer owns:
  - top-level task ingress and business flow shape
  - shared runtime lifecycle
  - approval model semantics
  - audit/event persistence model
  - task session model
  - shared release gate policy
  - translation from executor result into final business response
- IM engineer owns:
  - IM-facing adapters, bridge, and transport behavior
  - message normalization into task requests
  - channel/webhook/websocket/long-poll concerns
  - outbound delivery integration and platform formatting
  - IM-facing setup and connectivity UX
- HarborOS control engineer (Engineer 3) owns:
  - HarborOS connector implementations
  - HarborOS route mapping and fallback behavior
  - HarborOS service/files execution contracts
  - HarborOS validation and control-path tests
  - HarborOS live integration fixtures, plans, and runbooks
  - HarborOS System Domain implementation boundary

The HarborOS control engineer does not automatically own:

- Home Device Domain adapters
- camera/native device protocol integrations
- IM bridge behavior
- notification delivery behavior

## Write Scope

The HarborOS control engineer may independently change these areas:

- `src/connectors/harboros.rs`
- `src/orchestrator/executors/harbor_ops.rs`
- `src/domains/system.rs`
- `scripts/harbor_integration.py`
- `tools/harbor_cli_shim.py`
- `tests/contracts/test_harbor_integration.py`
- `tests/test_orchestrator/test_harbor_ops.py`
- HarborOS-specific plans under `plans/`
- HarborOS-specific contract/runbook docs

The HarborOS control engineer may change these areas only with framework-owner
coordination:

- `src/main.rs`
- `src/orchestrator/contracts.rs`
- `src/orchestrator/router.rs`
- `src/orchestrator/policy.rs`
- `src/runtime/task_api.rs`
- `src/runtime/task_session.rs`
- `src/control_plane/tasks.rs`

The HarborOS control engineer must treat these areas as not owned by HarborOS
control and avoid editing them for HarborOS-only work:

- `harborbeacon/`
- IM bridge code paths
- notification delivery models and services
- device-native adapter stacks unless explicitly assigned
- external IM repo files
- external IM contract files

## Northbound Freeze For HarborOS Work

For HarborOS control work, the northbound contract is frozen at the normalized
action boundary plus the existing task/response semantics already accepted by
framework and IM.

HarborOS control may consume:

- normalized `Action`
- approval context
- task and trace identifiers
- dry-run flag
- business-owned route metadata as pass-through context only

HarborOS control may produce:

- executor result payloads
- structured internal errors
- audit fields such as executor name, fallback usage, and duration

HarborOS control MUST NOT on its own:

- add new required fields to `POST /api/tasks`
- change the requiredness or semantics of `message`
- change `source.route_key` handling rules
- change resumed-turn behavior
- change notification request or delivery semantics
- change cross-repo contract headers or shared HTTP error rules

## Southbound HarborOS Responsibilities

HarborOS control is responsible for the full southbound path from normalized
action to HarborOS execution result.

That includes:

- middleware HTTP integration
- middleware WebSocket integration
- midcli integration and fallback
- HarborOS operation mapping tables
- HarborOS-side validation
- service name and filesystem path guardrails
- retry and timeout behavior for HarborOS connectors
- execution telemetry for HarborOS actions

HarborOS control should prefer extending adapters and mapping tables instead of
changing IM-facing request or response shapes.

HarborOS control is specifically the owner of the HarborOS System Domain path,
not the owner of every device-control path in the larger Home Hub architecture.

## Route And Policy Rules

- HarborOS-domain execution MUST preserve the route priority already established
  by HarborOS docs and tests.
- For HarborOS service/files domains, the preferred route remains:
  - `middleware_api`
  - `midcli`
- Browser and MCP are not valid default routes for HarborOS-domain control work
  unless explicitly approved at the framework level.
- HIGH and CRITICAL HarborOS mutations MUST continue to require approval.
- `dry_run=true` MUST remain side-effect free.

## Error Boundary Rules

- HarborOS control may define internal executor error codes for HarborOS
  adapters, such as method-mapping failures, auth failures, transport failures,
  invalid service names, or denied paths.
- HarborOS control MUST NOT unilaterally change the shared non-200 HTTP error
  envelope frozen by IM contract v1.5.
- HarborOS control MUST NOT leak IM-platform or notification-delivery concerns
  into HarborOS adapter error models.
- Translation from HarborOS executor failure into top-level business response is
  framework-owned unless explicitly delegated.

## Change Control

The following changes are safe for HarborOS control to land independently:

- add or refine HarborOS connector logic
- add or refine HarborOS execution mapping tables
- tighten HarborOS validation
- improve HarborOS fallback behavior
- add HarborOS-specific tests
- add HarborOS-specific docs, plans, and runbooks

The following changes require framework-owner sign-off before implementation:

- new domain or operation names visible outside HarborOS adapters
- any change to approval semantics
- any change to task persistence shape
- any change to task session behavior
- any change to top-level runtime envelopes
- any change to shared orchestrator contracts used outside HarborOS control

The following changes require both framework-owner and IM-owner sign-off before
implementation:

- any change to `POST /api/tasks`
- any change to `TaskRequest` or `TaskResponse` semantics visible to IM callers
- any change to `message` block semantics
- any change to `source.route_key` semantics
- any change to resumed-turn semantics
- any change to notification request, response, retry, or idempotency behavior
- any change to `X-Contract-Version`
- any change to shared HTTP auth or error-envelope rules

## Collaboration Workflow

- If HarborOS work is blocked by a shared file, prefer adding a HarborOS-local
  adapter or wrapper before changing the shared file.
- If a shared-file change is truly necessary, pause and align with the owner of
  that boundary before editing.
- HarborOS control changes should be reviewable as HarborOS control changes,
  not mixed with IM bridge refactors or contract redesign.
- If a change would require updating the external IM contract, that change is
  automatically out of scope for a HarborOS-only implementation task.

## Minimum Test Cases

1. HarborOS service query works through the preferred route.
2. HarborOS service mutation honors approval requirements.
3. HarborOS fallback to midcli works when middleware path fails.
4. Invalid service names are rejected before execution.
5. Invalid filesystem paths are rejected before execution.
6. `dry_run=true` returns preview data without side effects.
7. HarborOS execution responses include route/audit metadata.
8. HarborOS control changes do not require changing IM contract v1.5.

## Release Gate

A HarborOS control release is allowed only when:

- HarborOS contract tests pass
- HarborOS fallback tests pass
- HarborOS approval-path tests pass
- HarborOS live integration evidence exists for the intended route
- no IM contract file was modified as part of HarborOS-only work
- no IM credential ownership leaked into HarborOS control code
- no direct IM platform delivery dependency was introduced into HarborOS control

## Working Principle

Move HarborOS control fast, but keep the northbound contract still.
