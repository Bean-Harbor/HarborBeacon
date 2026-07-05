# HarborBeacon Harbor Collaboration Contract v2

> Superseded note: for the current HarborBeacon core architecture and Harbor
> collaboration boundary, read
> `docs/HarborBeacon-Harbor-Collaboration-Contract-v3.md` first. This v2 file
> remains useful historical context, but v3 is the current HarborBeacon
> collaboration contract. The external HarborBeacon <-> HarborGate IM contract
> remains v2.0.

## Status

This document is the working freeze candidate for the current multi-lane
development model across:

- the HarborBeacon repo
- the external HarborGate repo
- the `harbor-*` skill topology used to organize ownership

It supersedes the narrower HarborOS-control-only collaboration model as the
primary coordination document for the current phase.

This document does not replace or reinterpret the external IM contract. For the
current phase, the active external IM contract is v2.0.

## Normative References

The authoritative cross-repo IM boundary remains:

- `C:\Users\beanw\OpenSource\HarborGate\HarborBeacon-HarborGate-Agent-Contract-v2.0.md`

The northbound channel-edge upgrade is:

- `C:\Users\beanw\OpenSource\HarborGate\HarborBeacon-HarborGate-Agent-Contract-v3.0.md`

The previous v1.5 contract is historical reference only during the v2.0
upgrade.

Execution planning references:

- `C:\Users\beanw\HarborBeacon-LocalAgent-Project-git\HarborBeacon-LocalAgent-Roadmap.md`
- `C:\Users\beanw\HarborBeacon-LocalAgent-Project-git\HarborBeacon-LocalAgent-Plan.md`

Historical same-repo HarborOS-only collaboration context:

- `C:\Users\beanw\HarborBeacon-LocalAgent-Project-git\HarborBeacon-HarborOS-Control-Collaboration-Contract-v1.md`

If this document conflicts with the IM contract v2.0 on cross-repo interface
semantics, the IM contract v2.0 wins.

## Purpose

Freeze the collaboration boundary so functional lanes can move in parallel
without re-coupling the system.

The intended operating model for this phase is:

- IM remains in a separate repo
- HarborBeacon remains the business-core repo
- southbound work is domain-split, not one generic adapter bucket
- each lane owns implementation inside a frozen collaboration boundary

## Model Capability Layer

Model execution is a shared capability layer, not a business domain. HarborBeacon
owns model-center orchestration, endpoint policy, redaction, and audit evidence;
HarborGate does not own model choice, model credentials, or retrieval semantics.

The current model architecture is local-first. Cloud endpoints are allowed only
as controlled fallback for explicitly cloud-enabled route policies. The first
cloud fallback scope is limited to:

- `semantic.router`
- `retrieval.answer`

Cloud fallback must not become the default path for HarborOS command execution,
AIoT control, OCR, VLM, or embedding routes. Each LLM fallback attempt must
record the selected endpoint, attempted endpoints, and fallback reason without
persisting plaintext API keys or full sensitive prompts.

## Team Topology

### `harbor-architect`

Own:

- overall architecture and repo topology
- boundary governance
- milestone sequencing and cutover order
- release, rollback, and acceptance gates
- conflict arbitration between lanes

Do not automatically own:

- day-to-day feature coding inside a single lane

### `harbor-framework`

Own:

- shared runtime and control-plane boundaries
- northbound task ingress and response semantics inside HarborBeacon
- task/session lifecycle
- approval, artifact, event, and audit semantics
- local inference runtime abstraction and provider-policy seams
- intelligent orchestration, planner, router, and executor contracts
- account, identity, permission, and workspace management

Do not automatically own:

- IM transport internals
- HarborOS system-domain implementation details
- AIoT device-native protocol stacks

### `harbor-im-gateway`

Own:

- IM adapters and platform SDK/client logic
- webhook, websocket, and long-poll transport
- route registry and `route_key` lifecycle
- outbound delivery, platform payload formatting, and delivery retries
- platform credential storage and validation
- redacted gateway status

Do not automatically own:

- HarborBeacon business state
- HarborBeacon approval, artifact, audit, or task-session truth

### `harbor-hos-control`

Own:

- HarborOS System Domain implementation
- middleware HTTP/WebSocket integration
- `midcli` fallback
- HarborOS service/files execution mapping
- HarborOS validation and control-path tests

Do not automatically own:

- IM bridge behavior
- AIoT device-native adapters
- notification delivery behavior

### `harbor-aiot`

Own:

- Home Device Domain implementation
- camera and LAN AIoT native adapters
- ONVIF, RTSP, vendor-cloud bridge, and device protocol logic
- discovery, PTZ, snapshot, stream-open, and device-control behavior
- media/control separation for device workflows

Do not automatically own:

- IM transport
- HarborOS system-domain execution
- HarborBeacon business-state ownership

## System Boundary

### Cross-Repo Boundary

- HarborGate and HarborBeacon communicate only through HTTP/JSON contracts.
- The repos MUST NOT import each other's runtime code.
- The repos MUST NOT share `.harborbeacon/*.json` or other runtime state files.

### Business Source Of Truth

HarborBeacon remains the source of truth for:

- business session state
- resumable workflow state
- approvals
- artifacts
- audit trail
- business conversation continuity

HarborGate owns transport and platform concerns only.

### Southbound Domain Split

The runtime has at least two distinct southbound domains and they MUST NOT be
collapsed into one routing policy.

#### 1. HarborOS System Domain

Preferred route:

- `Middleware API -> MidCLI -> Browser/MCP fallback`

#### 2. Home Device Domain

Preferred route:

- `Native Adapter -> LAN Bridge -> HarborOS Connector -> Cloud/MCP`

Meaning:

- device-native work should not default to HarborOS CLI or HarborOS middleware
- HarborOS may still provide storage, archive, policy, or coordination support
- media persistence may be HarborOS-backed while control remains device-domain

## Hard Boundary Rules

- HarborBeacon MUST NOT directly deliver IM platform messages after cutover.
- HarborBeacon MUST NOT become the long-term owner of IM platform credentials.
- HarborGate MUST NOT absorb HarborBeacon business semantics.
- HarborGate MAY act as the northbound channel edge for Android/Web chat and
  Beacon admin/config proxying, but MUST NOT own Beacon device, knowledge,
  model, approval, artifact, audit, or workflow truth.
- HarborOS control MUST NOT silently absorb Home Device Domain ownership.
- AIoT work MUST NOT silently collapse device-native control into HarborOS
  system control.
- Shared northbound semantics MUST NOT be widened casually for lane-local
  convenience.

## Frozen Interfaces

The following are frozen by the active external IM contract and MUST NOT change
without explicit multi-lane sign-off:

- `POST /api/web/turns`
- `POST /api/turns` only as a deprecated HarborBeacon compatibility alias
- `TaskTurnEnvelope` and turn response semantics visible to IM callers
- `conversation.handle`
- `transport.route_key`
- `active_frame`
- `continuation`
- `delivery_hints`
- outbound notification request and response semantics
- `X-Contract-Version`
- shared HTTP auth and non-200 error-envelope rules

The following northbound edge interfaces are added by the v3.0 channel-edge
contract and must preserve the same ownership split:

- `POST /api/gateway/turns`
- `/api/beacon/*` as the external proxy prefix for Beacon-owned admin/config APIs
- `/api/harbor-assistant/*` only as a deprecated migration alias

## Default Ownership Rules

Unless explicitly reassigned, the following belong to `harbor-framework`:

- local inference orchestration and provider abstraction
- planner, router, and intelligent task orchestration
- audit/event persistence model
- approval model semantics
- account management, identity binding, permissions, and workspace state
- shared task/session persistence

Unless explicitly reassigned, the following belong to `harbor-im-gateway`:

- IM transport behavior
- route key generation and lookup
- platform credentials
- platform delivery formatting

Unless explicitly reassigned, the following belong to `harbor-hos-control`:

- HarborOS middleware and `midcli` execution behavior
- HarborOS service/files mapping and validation

Unless explicitly reassigned, the following belong to `harbor-aiot`:

- camera and AIoT protocol adapters
- device discovery and control execution
- device-media/control split inside the Home Device Domain

Unless explicitly reassigned, the following belong to `harbor-architect`:

- boundary arbitration
- cutover sequencing
- release and rollback gates
- final acceptance of cross-lane changes

## Write Scope Defaults

These are default ownership examples, not a complete file ACL.

### `harbor-framework`

Usually owns first-change rights in areas such as:

- `src/runtime/task_api.rs`
- `src/runtime/task_session.rs`
- `src/control_plane/*`
- `src/orchestrator/router.rs`
- `src/orchestrator/policy.rs`
- `src/connectors/ai_provider.rs`

### `harbor-im-gateway`

Usually owns first-change rights in the external IM repo for:

- adapters
- transport entrypoints
- route registry
- delivery pipeline
- platform credential handling

### `harbor-hos-control`

Usually owns first-change rights in areas such as:

- `src/connectors/harboros.rs`
- `src/orchestrator/executors/harbor_ops.rs`
- `src/domains/system.rs`
- HarborOS-specific tests, plans, and runbooks

### `harbor-aiot`

Usually owns first-change rights in areas such as:

- device/camera discovery and media-control paths
- device-native adapters and registry-facing device logic
- camera snapshot/stream/PTZ execution paths
- device-domain tests, fixtures, and runbooks

## Change Control

### Lane-Local Changes

A lane may land changes independently when:

- the change stays within its domain boundary
- no frozen interface changes
- no shared semantic reinterpretation

### Shared Runtime Changes

Changes touching shared runtime or business semantics require
`harbor-framework` sign-off.

### Cross-Lane Routing Changes

Changes that move work between HarborOS System Domain and Home Device Domain
require:

- `harbor-framework`
- `harbor-hos-control`
- `harbor-aiot`

### Frozen Contract Changes

Changes to frozen IM-facing interfaces require:

- `harbor-architect`
- `harbor-framework`
- `harbor-im-gateway`

### Release Or Cutover Changes

Changes that alter rollout order, rollback shape, or acceptance criteria require
`harbor-architect` sign-off.

## Collaboration Workflow

When a request arrives:

1. classify whether it is framework, IM, HarborOS system, AIoT device, or
   cross-cutting work
2. assign the owning lane
3. name required collaborators only if a shared seam is touched
4. restate what is frozen before implementation starts
5. prefer adapter-local or lane-local changes before editing shared models
6. run the highest-signal validation for the affected lane plus seam tests

## Daily GitHub Sync Rule

Every working day should end with both lane-local sync and architecture-level
closeout.

### Lane-Local Sync Responsibility

- each lane owner syncs their own repo or lane changes to GitHub before ending
  the workday
- `harbor-framework` is the default daily sync owner for HarborBeacon-repo core
  work
- `harbor-im-gateway` is the daily sync owner for the external HarborGate repo
- `harbor-hos-control` syncs HarborOS System Domain changes
- `harbor-aiot` syncs AIoT and Home Device Domain changes

At minimum, the lane owner should leave behind:

- a pushed branch or updated pull request for the day's work
- a short change summary
- current validation status
- blockers, known risks, and rollback notes if the change is risky

The default reporting template lives at:

- `C:\Users\beanw\HarborBeacon-LocalAgent-Project-git\docs\daily\harbor-daily-sync-template.md`

Lane owners should not wait for `harbor-architect` to do basic commit, push, or
pull-request hygiene on their behalf.

### Architecture Closeout Responsibility

`harbor-architect` owns the end-of-day integration closeout across lanes.

This means:

- checking which lane updates are ready to merge and which must wait
- confirming whether cross-lane seams remain inside the frozen boundary
- identifying cutover, rollback, or release risks introduced that day
- publishing the daily integration view: merged, pending, blocked, and next
  actions

`harbor-architect` governs the daily closeout decision, but does not replace
lane-local GitHub ownership.

### Default Working Rule

In plain terms:

- each lane owner is responsible for pushing their own work
- `harbor-architect` is responsible for deciding whether the system is safe to
  close, merge, or carry forward to the next day

## Observability Rule

All lanes should preserve and log, when available:

- `task_id`
- `trace_id`
- `source.route_key`
- `source.conversation_id`
- `message.message_id`
- `notification_id`
- `delivery.idempotency_key`
- `destination.route_key`

## Release Gate

A cross-lane release is allowed only when:

- lane-local tests pass for the touched areas
- frozen contract tests pass when applicable
- rollback shape is documented for boundary-moving changes
- no repo import or runtime-state sharing violation was introduced
- IM credential ownership did not leak into HarborBeacon
- Beacon-owned device credentials, model secrets, camera config, knowledge roots,
  approvals, artifacts, and audit did not leak into HarborGate
- device-native ownership did not collapse into HarborOS system control

## Working Principle

Move each lane fast, but keep the boundary still.
