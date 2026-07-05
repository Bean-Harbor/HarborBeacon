# HarborBeacon Core Architecture And Harbor Collaboration Contract v3

## Status

This document is the current HarborBeacon core architecture and Harbor
collaboration contract as of 2026-06-12.

It updates `HarborBeacon-Harbor-Collaboration-Contract-v2.md` for the current
HarborBeacon shape, where Model Center, RAG, Privacy Gateway, workflow compiler
shadow mode, readiness, and evaluation tooling have become first-class
HarborBeacon framework surfaces.

This document does not replace the external HarborBeacon <-> HarborGate IM
contract. The active external IM contract remains v2.0.

If this document conflicts with the HarborGate v2.0 contract on cross-repo
interface semantics, the HarborGate v2.0 contract wins.

## Normative References

Authoritative external IM boundary:

- `C:\Users\beanw\OpenSource\HarborGate\docs\HarborBeacon-HarborGate-Agent-Contract-v2.0.md`

HarborBeacon side drift and rollback reference:

- `docs/HarborBeacon-HarborGate-v2.0-Upgrade-Runbook.md`

Prior Harbor collaboration contract:

- `docs/HarborBeacon-Harbor-Collaboration-Contract-v2.md`

Current local-agent planning references:

- `docs/HarborBeacon-LocalAgent-Roadmap.md`
- `docs/HarborBeacon-LocalAgent-Plan.md`
- `docs/local-model-backend-benchmark-gate.md`

## Purpose

Freeze the current HarborBeacon-centered architecture so new capability layers
can be added without re-coupling HarborGate, HarborOS system control, AIoT
device control, model runtime, RAG, and audit semantics.

The core rule for this phase is:

```text
HarborBeacon may grow governance layers, but each user turn must keep a short,
route-specific hot path.
```

This contract exists to make two things explicit:

1. where HarborBeacon sits in the Harbor system
2. which HarborBeacon internal layers are governance/control plane versus
   required runtime hot path

## Level 1 Architecture: Harbor System Boundary

```text
+-------------------------------------------------------------------+
|                    User / Family / Operator                       |
+-----------------------------------+-------------------------------+
                                    |
                                    v
+-------------------------------------------------------------------+
|        Surfaces: IM / WebUI / Admin API / CLI / Eval CLI           |
+-----------------------+-------------------------------------------+
                        |
                        v
+-------------------------------+        HTTP/JSON v2.0
| HarborGate / HarborNAS WebUI  | --------------------------------+
| - IM transport                |                                 |
| - route_key / credentials     |                                 |
| - delivery formatting         |                                 |
| - operator surfaces           |                                 |
+-------------------------------+                                 |
                                                                  v
                                                   +-------------------------------+
                                                   | HarborBeacon Core             |
                                                   | - business truth             |
                                                   | - conversation/task state    |
                                                   | - routing/planning           |
                                                   | - policy/approval/audit      |
                                                   | - model/RAG/privacy gates    |
                                                   +---------------+---------------+
                                                                   |
                  +------------------------------------------------+------------------------------------------------+
                  |                                                |                                                |
                  v                                                v                                                v
+-------------------------------+              +-------------------------------+              +-------------------------------+
| Model Runtime Layer           |              | HarborOS System Domain        |              | Home Device / AIoT Domain     |
| - local-first runtime         |              | - system readiness            |              | - cameras / RTSP / ONVIF      |
| - route policy                |              | - middleware / MidCLI         |              | - HA / LAN devices            |
| - controlled cloud fallback   |              | - OS/system operations        |              | - home sensing/control        |
+-------------------------------+              +-------------------------------+              +-------------------------------+
```

### Level 1 Rules

- HarborBeacon is the business-core repo.
- HarborGate owns transport, route registry, platform credentials, and outbound
  delivery formatting.
- HarborNAS WebUI may surface HarborBeacon state and actions, but it does not
  own HarborBeacon business truth.
- HarborBeacon owns task state, workflow state, approval state, artifact state,
  audit state, memory/readiness evidence, route policy, RAG semantics, and model
  runtime policy.
- HarborOS System Domain and Home Device / AIoT Domain are separate southbound
  domains and must not be silently collapsed.

## Level 2 Architecture: HarborBeacon Core

```text
+-------------------------------------------------------------------+
| Northbound Contract Layer                                          |
| POST /api/web/turns                                                |
| TaskTurnEnvelope / conversation.handle / transport.route_key        |
+-----------------------------------+-------------------------------+
                                    |
                                    v
+-------------------------------------------------------------------+
| Turn / Session / Active Frame                                      |
| conversation continuity / task state / workflow frame              |
| approval refs / artifact refs / audit correlation                  |
+-----------------------------------+-------------------------------+
                                    |
                                    v
+-------------------------------------------------------------------+
| Message Controller Layer                                           |
| general message controller / task controller / admin handlers       |
+-----------------------------------+-------------------------------+
                                    |
                                    v
+-------------------------------------------------------------------+
| Routing / Planning Layer                                           |
|                                                                   |
|  deterministic router        semantic.router                       |
|  stable rules                local-only intent routing             |
|                                                                   |
|  workflow compiler shadow                                          |
|  candidate plan only; no direct execution                          |
+-----------------------------------+-------------------------------+
                                    |
                                    v
+-------------------------------------------------------------------+
| Policy / Risk / Approval Gate                                      |
| route policy / privacy level / read-only vs executable action      |
| approval requirement / audit evidence                              |
+-----------------------------------+-------------------------------+
                                    |
                                    v
+-------------------------------------------------------------------+
| Capability Domains                                                 |
|                                                                   |
|  RAG / retrieval.answer       System readiness / EVT               |
|  Privacy Gateway before cloud diagnostics / preflight              |
|                                                                   |
|  Model Center                Home Guardian / Family Memory         |
|  local/cloud policy          vision and household context          |
|                                                                   |
|  HarborOS System Domain      Home Device / AIoT Domain             |
+-----------------------------------+-------------------------------+
                                    |
                                    v
+-------------------------------------------------------------------+
| Observability / Readiness / Evaluation                             |
| audit_records / readiness APIs / benchmark CLIs / release gates     |
+-------------------------------------------------------------------+
```

## Business Source Of Truth

HarborBeacon remains the source of truth for:

- business conversation continuity
- `conversation.handle`
- task/session state
- active dialogue frames
- resumable workflow state
- approvals
- artifacts
- audit records and evidence bundles
- route policy and privacy policy decisions
- model endpoint policy and runtime-readiness projections
- RAG citations, answer semantics, and privacy-gateway evidence
- Home Guardian / family memory state

HarborGate remains the source of truth for:

- IM adapters
- route registry and `transport.route_key` lifecycle
- platform credentials
- inbound transport normalization
- outbound platform delivery and retry mapping
- redacted gateway status

HarborBeacon must treat `transport.route_key` as opaque metadata.

## Frozen External Interfaces

The following remain frozen by the active HarborGate v2.0 contract:

- `POST /api/web/turns`
- `POST /api/turns` only as a deprecated HarborBeacon compatibility alias
- `TaskTurnEnvelope`
- `conversation.handle`
- `transport.route_key`
- `active_frame`
- `continuation`
- `delivery_hints`
- HarborGate `POST /api/notifications/deliveries`
- `X-Contract-Version: 2.0`
- shared HTTP auth and non-200 error-envelope rules

HarborOS packaged deployments may route HarborBeacon behind a fixed nginx
prefix such as `/api/harbor-beacon/*`. HarborBeacon may accept internal
strip-prefix aliases such as `/web/turns` and `/turns` so that the same v2 turn
semantics survive that packaging shape. These aliases are deployment
compatibility paths only; they do not replace the public v2.0 contract paths,
and they must keep the same bearer-auth and `X-Contract-Version: 2.0`
requirements.

Changing these requires explicit sign-off from:

- `harbor-architect`
- `harbor-framework`
- `harbor-im-gateway`

## Hot Path Contract

HarborBeacon has governance layers, but a normal user turn must not be forced
through every layer synchronously.

### Required Hot Path

Every user turn may pass through:

```text
input
  -> northbound envelope validation
  -> turn/session/active-frame lookup
  -> message controller
  -> deterministic route or selected router path
  -> required policy gate
  -> selected capability handler
  -> response
```

### Conditional Path

Only route-specific requests may enter:

- `semantic.router`
- RAG retrieval and answer synthesis
- Privacy Gateway
- Model Center runtime selection
- HarborOS System Domain execution
- Home Device / AIoT execution
- Home Guardian / family memory handlers

### Cold Path And Control Plane

These must not become mandatory per-turn dependencies:

- workflow compiler evaluation packs
- privacy gateway evaluation packs
- readiness aggregation
- release gates
- benchmark CLIs
- evidence/report generation
- document or research artifacts

### Performance Rules

- Deterministic router must short-circuit when it has a confident route.
- `semantic.router` is only a fallback/assist path and remains local-only.
- Workflow compiler remains shadow mode until explicitly promoted; shadow
  mismatch may be traced, but must not block or replace the final plan.
- Privacy Gateway runs only before `retrieval.answer` cloud fallback.
- Model calls are the expensive part; route/policy/evidence layers should avoid
  model calls unless the selected route needs them.
- Audit evidence should be metadata-first and may be made asynchronous when the
  user-visible response path would otherwise be delayed.

## Routing And Planning Contract

The routing layer may produce candidates, but policy owns execution admission.

Allowed planning producers:

- deterministic router
- local `semantic.router`
- workflow compiler shadow candidate
- task planner / decomposer

Rules:

- Deterministic routes are preferred for stable, well-known intents.
- `semantic.router` must not use cloud fallback.
- Workflow compiler output is advisory unless a future contract explicitly
  promotes it out of shadow mode.
- Workflow compiler candidates must map to existing plan/action abstractions.
- No planner may directly execute HarborOS, AIoT, file, camera, restart, delete,
  or credential-affecting actions.
- Read-only diagnostics and executable actions must remain separate.

## Workflow Compiler Contract

The current workflow compiler layer is a HarborBeacon-native shadow layer.

Current allowed first workflow:

- `system_diagnostics_v1`

Current allowed target plans:

- `SystemReadiness`
- `EvtReadiness`
- `EvtPreflight`
- `EvtEvidenceBundle`

Rules:

- It may emit candidates, confidence, reasons, and shadow evidence.
- It may compare its candidate with the final plan.
- It must not change `POST /api/web/turns` semantics.
- It must not change HarborGate contract semantics.
- It must not produce execution actions such as service restart, camera
  connection, file write, or file deletion.
- It must be verifiable through `evaluate-workflow-compiler`.

## Model Center And Cloud Fallback Contract

Model execution is a shared HarborBeacon capability layer, not a business
domain.

Rules:

- Product default is local-first.
- Harbor-managed Candle-first local runtime is the preferred default lane, but
  Candle is not the frozen API contract.
- OpenAI-compatible endpoints are advanced/external endpoints governed through
  Model Center and route policy.
- Cloud fallback is allowed only for explicitly enabled routes.
- Current controlled fallback scope is limited to `retrieval.answer`.
- `semantic.router` remains local-only.
- HarborOS command execution, AIoT control, OCR, VLM, and embedding routes do
  not use cloud fallback by default.
- Endpoint secrets must be persisted server-side and returned only in redacted
  form.
- Fallback audit must record endpoint choice, attempted endpoints, fallback
  reason, and policy evidence without plaintext secrets or full sensitive
  prompts.

## Privacy Gateway Contract

Privacy Gateway is the RAG cloud preflight gate for `retrieval.answer`.

It evaluates information flow, not only PII. The current CI fields are:

- sender role
- subject role
- recipient kind
- information types
- purpose
- consent basis
- destination
- privacy level
- decision
- policy version

Policy defaults:

- `strict_local`: cloud blocked
- `allow_redacted_cloud`: cloud allowed only with a task-minimal semantic capsule
- `allow_cloud`: cloud allowed, but evidence is still recorded

Rules:

- Do not store raw query, raw paths, source paths, URLs, credentials, or full
  citation previews in Privacy Gateway evidence.
- `allow_redacted_cloud` may upload only the semantic capsule.
- If capsule generation fails or risk gates block the flow, do not call cloud;
  degrade to local citation summary.
- Audit action for this path is `privacy_gateway.rag_answer.evaluate`.
- Readiness exposure must remain metadata-only.
- Runtime behavior must be verifiable through `evaluate-privacy-gateway`.

## RAG And Knowledge Contract

HarborBeacon owns retrieval semantics and answer assembly.

Rules:

- Retrieval citations belong to HarborBeacon.
- `retrieval.answer` may use local model/runtime first.
- Cloud fallback for `retrieval.answer` must pass Privacy Gateway first.
- Citation paths, source paths, URLs, credential-like strings, and long previews
  must not leak into cloud prompts under `allow_redacted_cloud`.
- RAG readiness may expose route coverage, policy version, counts, warning
  counts, recent transform ids, and recent audit time, but not raw user/source
  content.

## Southbound Domain Contract

Do not collapse every southbound path into one abstraction.

### HarborOS System Domain

Owner:

- `harbor-hos-control`

Preferred route:

```text
Middleware API -> MidCLI -> Browser/MCP fallback
```

Belongs here:

- service state
- system readiness
- system files and storage operations
- HarborOS middleware integration
- HarborOS command execution mapping

### Home Device / AIoT Domain

Owner:

- `harbor-aiot`

Preferred route:

```text
Native Adapter -> LAN Bridge -> HarborOS Connector -> Cloud/MCP fallback
```

Belongs here:

- cameras
- RTSP / ONVIF / device discovery
- HA / LAN device control
- media/control separation
- vendor/device protocol adapters

Shared framework must coordinate the handoff, but must not hide the domain
split.

## Observability, Readiness, And Evaluation

Observability is a contract surface, not only debugging output.

Preserve these anchors when available:

- `turn.turn_id`
- `turn.trace_id`
- `conversation.handle`
- `transport.route_key`
- `message_id`
- `notification_id`
- `delivery.idempotency_key`
- route policy id
- privacy policy version
- model endpoint id or redacted endpoint label
- audit action
- transform id

Current HarborBeacon readiness/eval surfaces include:

- `GET /api/rag/readiness`
- `GET /api/feature-availability`
- model/readiness admin surfaces
- workflow compiler shadow traces
- `evaluate-workflow-compiler`
- `evaluate-privacy-gateway`
- contract/drift/release-gate tooling

Readiness responses may summarize status, counts, policies, and warnings.
They must not emit raw sensitive prompts, credentials, raw source paths, URLs,
or citation previews.

## Ownership Map

### `harbor-architect`

Owns:

- architecture boundary governance
- cross-lane conflict resolution
- release, rollback, and acceptance gates
- frozen contract changes
- final acceptance for cross-lane changes

### `harbor-framework`

Owns:

- HarborBeacon shared runtime
- northbound contract integration
- turn/session/active-frame semantics
- planner, router, workflow compiler, and policy contracts
- approval, artifact, audit, and event semantics
- Model Center policy and runtime abstraction
- Privacy Gateway framework semantics
- RAG answer and retrieval semantics
- readiness/eval admin surfaces

### `harbor-im-gateway`

Owns:

- external HarborGate repo
- platform adapters
- route registry
- platform credentials
- inbound normalization
- outbound delivery formatting and retries

### `harbor-hos-control`

Owns:

- HarborOS System Domain
- middleware and MidCLI control paths
- HarborOS service/files execution mapping
- HarborOS validation and smoke tests

### `harbor-aiot`

Owns:

- Home Device / AIoT Domain
- camera/device native adapters
- RTSP / ONVIF / HA / LAN device integration
- device media/control separation

## Change Control

### Lane-Local Change

Allowed when:

- no frozen interface changes
- no shared semantic reinterpretation
- no route ownership moves
- local tests for the lane pass

### Shared Runtime Change

Requires `harbor-framework` sign-off when touching:

- turn/session semantics
- router/planner/policy behavior
- approval/artifact/audit semantics
- Model Center policy
- Privacy Gateway policy
- RAG answer semantics
- readiness/eval surfaces

### Cross-Lane Routing Change

Requires:

- `harbor-architect`
- `harbor-framework`
- affected lane owner or owners

Examples:

- moving camera control into HarborOS system control
- moving system commands into AIoT/device abstraction
- changing where cloud fallback is allowed
- promoting workflow compiler from shadow to active routing

### Frozen External Contract Change

Requires:

- `harbor-architect`
- `harbor-framework`
- `harbor-im-gateway`
- corresponding HarborGate contract update
- rollback notes
- contract regression tests

## Release Gate

A release touching HarborBeacon core architecture is allowed only when:

- frozen HarborGate v2.0 contract tests still pass when applicable
- no repo import or runtime-state sharing violation is introduced
- HarborBeacon does not regain IM platform credential ownership
- HarborGate does not gain HarborBeacon business truth ownership
- HarborOS System Domain and AIoT Device Domain remain separate
- cloud fallback remains route-policy gated
- `semantic.router` remains local-only
- `retrieval.answer` cloud fallback passes Privacy Gateway
- audit/readiness evidence is metadata-only where sensitive sources are involved
- workflow compiler shadow mode cannot alter execution semantics
- relevant eval CLI and regression tests are run for touched areas

## Working Principle

HarborBeacon is allowed to become smarter, but not blurrier.

The core stays stable by separating:

- transport from business truth
- candidate planning from execution admission
- model runtime from business domain
- RAG cloud fallback from raw source content
- HarborOS system control from Home Device / AIoT control
- hot path from readiness/evaluation/control plane
