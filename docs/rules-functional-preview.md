# Rules Functional Preview

Status: local N2 + Rules integration and verification, 2026-09-05. This increment has
not been deployed to `192.168.3.147`. It does not establish real HA/device support
or completion of all AUT/ACT product requirements.

The implementation is in `src/runtime/automation.rs` and
`src/bin/rules_admin_api.rs`. Rules have their own store and execution history;
they do not reuse Home Guardian reviews or write Home Assistant automations.
HarborGate v2.0 turns, continuation, notification delivery, and ownership remain
unchanged. RAG, natural-language planning, camera/DVR, and IM delivery are not
implemented by this increment.

The integration base is `6dab1156555277810d2991c6a41bd9f763102933`;
the original Rules increment is `6eec817be1fc6100e699711cef7949a0e4378045`.
The base's N1/N2 model profiles, HarborLink southbound ownership, camera workers,
and signed-edge verification remain in place.

## API

The standalone admin prefix is `/api/automation/rules`. Packaged HarborOS uses
the same-origin `/api/harbor-beacon/automation/rules` prefix. Both reach the same
Beacon-owned handlers and existing local-admin/management authorization; do not
invent forwarded identity headers or expose the standalone process publicly.

All successful responses use HTTP 200. In the table, an empty suffix means the
prefix itself, without a trailing slash.

| Method | Suffix | JSON request | Response |
| --- | --- | --- | --- |
| GET | empty | none | `{ "rules": [RuleRecord] }` |
| POST | empty | RuleDefinition | `{ "rule": RuleRecord }` |
| PUT | `/{id}` | RuleDefinition fields plus `revision` at the top level | `{ "rule": RuleRecord }` |
| POST | `/{id}/preview` | `{ "revision": 1 }` | RulePreview |
| POST | `/{id}/enable` | `{ "revision": 1 }` | `{ "rule": RuleRecord }` |
| POST | `/{id}/pause` | `{ "revision": 1 }` | `{ "rule": RuleRecord }` |
| POST | `/{id}/delete` | `{ "revision": 1 }` | `{ "rule": RuleRecord }` |
| POST | `/{id}/run` | `{ "revision": 1, "trigger_id": "manual-1" }` | `{ "run": RuleRun }` |
| GET | `/{id}/runs` | none | `{ "runs": [RuleRun] }`, newest first |
| POST | `/events` | `{ "event_id": "event-1", "event_type": "test.signal" }` | `{ "runs": [RuleRun] }` |

Request schemas reject unknown fields. JSON/schema parsing errors return 400,
authorization failures 403, missing resources/routes 404, revision or lifecycle
conflicts 409, and semantic validation errors 422. Storage errors return 500
without exposing filesystem paths. An accepted run can return 200 with a failed,
partial, skipped, or unknown result; HTTP success does not mean a device acted.

### RuleDefinition

This example needs no HA, IM, camera, or model:

```json
{
  "name": "Local execution check",
  "trigger": { "kind": "manual" },
  "conditions": { "match_mode": "all", "items": [] },
  "actions": [{ "kind": "record", "message": "Local execution recorded" }],
  "expires_at": null
}
```

`expires_at` is an optional/null future epoch-second value. The trigger variants
are exactly:

```json
{ "kind": "manual" }
```

```json
{ "kind": "event", "event_type": "test.signal" }
```

```json
{ "kind": "state", "entity_id": "switch.test", "to": "on" }
```

```json
{ "kind": "schedule", "interval_seconds": 60 }
```

Conditions use `match_mode: "all" | "any"` and items shaped as
`{ "entity_id": "sensor.temperature", "operator": "gt", "value": "20" }`.
Operators are `eq`, `ne`, `gt`, `gte`, `lt`, and `lte`; ordered comparisons require
finite numbers. An empty condition list matches. Missing, unknown, or unavailable
entity states never satisfy a condition, including `ne`. HTTP callers cannot
supply condition context; preview/execution obtain HA state server-side.

The second action variant is:

```json
{
  "kind": "home_assistant",
  "entity_id": "light.test",
  "domain": "light",
  "service": "turn_on",
  "fields": {}
}
```

Only `light/switch/input_boolean` `turn_on/turn_off` and `scene.turn_on` are
accepted. Toggle, high-risk domains/services, and target overrides in `fields`
are rejected. The HA connector also checks the configured connection, exposed
domain, and actual entity. HA requests go through HarborLink's
`/v1/home-assistant/*` contract with the existing local service credential.
HarborLink owns the HA connection and HA secrets; Beacon stores only the
enabled/exposed-domain orchestration settings. Do not configure a direct HA URL
or HA bearer token in the Rules store. A `record` action persists a local execution-history
entry only; it is not an IM notification or simulated HA success.

### Returned Records

`RuleRecord` has `rule_id`, `revision`, `previewed_revision`, `status`,
`definition`, `created_at`, `updated_at`, `next_run_at`, and `activation_id`.
The last field is a server-owned enable-cycle identifier, not an input field.
Statuses are `draft`, `enabled`, `paused`, `expired`, and `deleted`.

`RulePreview` has `rule_id`, `revision`, `conditions_matched`, `actions`, and
`warnings`. Editing returns the rule to draft, advances revision, and invalidates
the previous preview. Enable requires a preview of the current revision.

`RuleRun` has `run_id`, `rule_id`, `revision`, `trigger_id`, `trigger_kind`,
`status`, `reason`, `started_at`, `ended_at`, `conditions_matched`, and `actions`.
Run statuses are `completed`, `partial`, `failed`, `skipped`, and `unknown`.
Action results have `index`, `status`, and `message`, with status
`succeeded/failed/skipped/unknown`. Times are epoch seconds; `ended_at` is null
for an interrupted/incomplete execution. Unknown HA outcomes are not retried or
reported as confirmed device success.

## Local Run

Run from the repository root with Rust/Cargo available. Use a free loopback
port and a new synthetic state directory; never point this example at deployment
state. The existing standalone local-owner fallback is a developer surface, not
a replacement for normal packaged product login. Use a fresh developer shell
without deployment credential-file overrides. The synthetic directional tokens
below satisfy the existing startup contract; they do not connect this example
to HarborGate or bypass packaged authentication. Camera automation and recording
validation are disabled only for this isolated local example.

```powershell
$rulesDemoRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("harbor-rules-demo-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $rulesDemoRoot | Out-Null
$env:HARBOR_GATE_TO_BEACON_TOKEN = [Guid]::NewGuid().ToString('N') + [Guid]::NewGuid().ToString('N')
$env:HARBOR_BEACON_TO_GATE_TOKEN = [Guid]::NewGuid().ToString('N') + [Guid]::NewGuid().ToString('N')
$env:HARBORBEACON_SOUTHBOUND_MODE = 'harborlink'
$env:HARBORLINK_MEDIA_API_URL = 'http://127.0.0.1:9'
$env:HARBORLINK_LOCAL_API_TOKEN = [Guid]::NewGuid().ToString('N')
$env:HARBOR_K3_CAT_AUTO_RECORD_ENABLED = 'false'
$env:HARBOR_K3_CAT_RECORDING_VALIDATION_MODE = 'off'
$env:HARBOR_K3_CAT_ACTIVITY_POLICY_PATH = "$rulesDemoRoot/cat-policy.json"
$env:HARBOR_K3_CAT_RECORDING_RECONCILIATION_PATH = "$rulesDemoRoot/cat-reconciliation.json"
$env:HARBOR_K3_CAT_RECORDING_VALIDATION_STORE_PATH = "$rulesDemoRoot/cat-validation.jsonl"
cargo run --no-default-features --features fixed-local-models --bin agent-hub-admin-api -- --bind 127.0.0.1:43174 --admin-state "$rulesDemoRoot/admin.json" --device-registry "$rulesDemoRoot/devices.json" --conversations "$rulesDemoRoot/conversations.json" --public-origin http://127.0.0.1:43174
```

This starts the real admin API and Rules worker, not a frontend dev server.
The example selects the N2 feature profile, but it does not execute or qualify
an N2 model runtime. For N1/default-feature verification, omit
`--no-default-features --features fixed-local-models`. The deliberately unused
HarborLink endpoint keeps this record-only example independent of real devices;
HA actions and HA state triggers require a separately configured HarborLink.
If that port is occupied, use another loopback port consistently. In a second
PowerShell terminal:

```powershell
$rulesBase = 'http://127.0.0.1:43174/api/automation/rules'
$rulesBody = @{
    name = 'Local execution check'
    trigger = @{ kind = 'manual' }
    conditions = @{ match_mode = 'all'; items = @() }
    actions = @(@{ kind = 'record'; message = 'Local execution recorded' })
    expires_at = $null
} | ConvertTo-Json -Depth 8
$createdRule = Invoke-RestMethod -Method Post -Uri $rulesBase -ContentType 'application/json' -Body $rulesBody
$ruleUri = "$rulesBase/$($createdRule.rule.rule_id)"
$revisionBody = @{ revision = $createdRule.rule.revision } | ConvertTo-Json
Invoke-RestMethod -Method Post -Uri "$ruleUri/preview" -ContentType 'application/json' -Body $revisionBody
Invoke-RestMethod -Method Post -Uri "$ruleUri/enable" -ContentType 'application/json' -Body $revisionBody
$runBody = @{ revision = $createdRule.rule.revision; trigger_id = 'manual-1' } | ConvertTo-Json
Invoke-RestMethod -Method Post -Uri "$ruleUri/run" -ContentType 'application/json' -Body $runBody
Invoke-RestMethod -Method Get -Uri "$ruleUri/runs"
```

Reusing the same trigger ID returns the same stored run. Changing the rule
requires PUT with its current revision plus the complete RuleDefinition,
followed by preview and enable of the returned revision. Pause/delete use the
same revision request shape. Stop the developer process with Ctrl+C; its temporary
state remains available for inspection, and this example does not delete it.

## Storage And Scheduling Limits

- The store is adjacent to admin state: `admin.json` produces `admin.rules.json`.
  RulesStore clones share a mutex; all runtime writes must use those clones.
  Do not run two independent processes against the same store file.
- Writes use a temporary JSON file, synchronization, and atomic replacement.
  Each action is persisted as unknown before the adapter call; a crash or failed
  result write cannot cause automatic replay. Deleted rules retain history and
  dedupe evidence. Successful actions are not replayed after another action fails.
- At most 100 non-deleted rules, 20 actions and 20 conditions per rule. Names
  are at most 128 bytes, record messages 1024 bytes, entity IDs 128 bytes,
  condition/state values 256 bytes, and HA fields 8192 bytes. Text input rejects
  control characters. HTTP event/trigger identifiers are 1-128 ASCII identifier
  characters.
- New runs are refused when store size reaches 48 MiB; 64 MiB is the read/write
  ceiling. The reserve allows accepted results and rule management to persist.
  No history or replay protection is silently discarded. Long-term archive and
  retention are not implemented by this preview.
- Schedule intervals range from 10 to 31536000 seconds. This is interval
  scheduling, not cron, timezone/DST, or calendar semantics. Downtime does not
  replay every missed interval; a due run advances the next time from now.
- The worker polls every two seconds. State triggers need a live prior HA
  observation; first snapshot, reconnect, and re-enable establish a new baseline.
  Events come only from the explicit administrator `/events` endpoint, not a
  connected camera event bus. Pause prevents subsequent triggers; it does not
  promise to interrupt an action already executing.

## Verification

### Historical Rules Baseline

The original Rules commit `6eec817` passed 16 core tests and 8 admin
API/HTTP/worker tests. Its full serial regression passed 518 library tests and
131 admin-bin tests. These counts belong to that older source base, not the
N2 integration base `6dab115` or this worktree. Default parallel runs on that
older train were not clean: its original baseline also had model-center
failures, and the Rules worktree observed knowledge-test failures. Do not reuse
those historical counts or failure classifications as current integration proof.

### N2 Integration Verification

Local integration into `6dab115` is verified, with known baseline failures
preserved. Current evidence belongs under
`HarborNavi/artifacts/v1/n2-rules-integration/2026-09-05`.

| Check | Result |
| --- | --- |
| N1/default Rules | 16 core + 12 admin/worker/integration tests passed |
| N2/fixed Rules | 16 core + 12 admin/worker/integration tests passed |
| N1/N2 dependency isolation | 2 tests passed; N2 RISC-V dependency graph excludes Candle and hf-hub |
| N2-profile Windows binaries | `agent-hub-admin-api` and `harboros-beacon` built; not RISC-V packages |
| N2 full library, clean baseline | 665 passed, 60 failed, 1 ignored |
| N2 full library, integration | 681 passed, the same 60 failed, 1 ignored |
| N2 full admin binary, clean baseline | 192 passed, 5 failed |
| N2 full admin binary, integration | 204 passed, the same 5 failed |
| Integrated WebUI | 44 tests / 7 suites, typecheck, production build, joint browser regression passed |
| Actual local Rules + integrated UI | Create, preview, enable, run, reload, pause, edit, delete and retained history passed |
| Live worker without HA/model | Two scheduled record runs completed, then the rule expired |

The full-suite failure-name sets match exactly between the clean source and
integration. Fixed-model fixtures still attempt legacy model edits or expect
legacy catalogs, and a source-string assertion depends on LF line endings;
the complete logs also retain downstream poisoned-lock failures. These are not
new Rules regressions, but the full suites are **not green**. Default-feature
verification was focused, not a whole-library N1 rerun.

The integrated UI source is `HarborNavi/harboros` commit
`6c0f74f2b18942125c511bec0307406ea872166d`, based on `4d990d8`.
Its joint fixed-model/Rules browser suite uses fixtures. The separate real Rules
browser check uses the integrated N2-profile backend and persisted Rules state;
only product session/state are fixtures. Both cover 1440x1000 and 390x844 with
no page errors or horizontal overflow. Neither verifies `.147` login, real
Home Assistant effects, model readiness, IM, or a camera.

Required local checks include Rules core/API/worker behavior in both profiles,
the integrated service build, lifecycle and authorization ordering, the
HarborLink HA contract fixture, and `tests/test_n2_fixed_feature_isolation.py`.
Run the focused checks serially:

```powershell
cargo test --lib runtime::automation::tests -- --test-threads=1
cargo test --bin agent-hub-admin-api rules_ -- --test-threads=1
cargo test --no-default-features --features fixed-local-models --lib runtime::automation::tests -- --test-threads=1
cargo test --no-default-features --features fixed-local-models --bin agent-hub-admin-api rules_ -- --test-threads=1
```

When using the existing Windows build cache, append
an explicitly coordinated `--target-dir` to each command. The N2 integration
cache is `C:/Users/beanw/OpenSource/harborbeacon-n2-fixed-models/target`;
the N1 cache is `C:/Users/beanw/OpenSource/HarborBeacon/target`.
Cargo can reuse the wrong root-package test artifact across same-version
worktrees. Force a distinct root-only build configuration for each comparison
(for example, `--config 'profile.dev.package.harborbeacon_local_agent.debug=0'`
for the clean baseline and a different root debug setting for integration),
verify the compile source path and test executable, and compare actual test
names/counts. Do not require the historical 518/502 totals on the newer base.
HTTP tests start loopback services and
temporary stores; HA response/worker cases use local fixtures, not real HA
hardware. The tests cover lifecycle, strict schemas, partial/unknown results,
replay, persistence failure, state re-priming, and bounded storage admission.

Real HA/IR/device effects, live camera event ingress, model-generated plans,
multi-member product authorization, full risk qualification, and an N2 package
containing this Rules integration remain separate work. The earlier read-only
`.147` probe found no Cargo/Rust compiler
on PATH or Cargo at `/home/harbor/.cargo/bin/cargo`; it did not exhaust all build
options. No Rules deployment, package upgrade, or device acceptance is implied
by the local test baseline.
