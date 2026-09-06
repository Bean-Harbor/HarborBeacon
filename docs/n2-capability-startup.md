# N2 Capability Startup: Option B

Status: batch 1 implemented, reviewed and locally verified, 2026-09-06. Native
packaging and device acceptance remain pending; this is not completion of all
Option B. Results include the final shared media-factory correction.

B2 source implementation and its separate acceptance boundary are recorded in
`n2-execution-ownership.md`; the test artifacts below remain the B1 baseline.

## Decision And Ownership

Keep one Beacon business core. Use explicit startup composition and isolate
optional capability configuration failures on N2. Do not split Rules, RAG, or
events into new services or repositories in this change.

Framework owns the shared startup and adapter lifecycle. Neyyeby continues to
own knowledge, intent, answers, citations and their UI. b246b continues to own
camera, media, event and DVR behavior. HarborGate owns IM transport; HarborOS
owns the authenticated edge and system lifecycle. Gate/Beacon v2.0 envelopes,
authentication, route aliases and business-state ownership remain unchanged.

## Batch 1: Startup Composition

- `HARBOR_BEACON_STARTUP_PROFILE=n1|n2` identifies the deployment policy. The
  value must match the compiled embedded/external runtime topology. Omission
  preserves the build's existing default. Packaged units set the value explicitly.
- N1 retains strict credential-file startup. N2 isolates optional Gate/model
  configuration failures. Neither profile accepts anonymous protected requests.
  The N1 standalone admin tool retains its existing environment-compatible
  inbound/outbound credential requirements; it does not acquire a new model
  credential requirement for a facade it does not host.
- Gate turn ingress and the local session/edge content adapter remain separate
  authenticated adapters over the existing business service. Missing Gate auth
  disables its verifier; it does not create a new local identity or IM account.
- Environment-compatible service credentials use a present systemd credential
  first, then an explicitly configured file, then supported environment values.
  An explicitly configured file that cannot be read is an error, not permission
  to use an environment fallback. The N1 file-only resolver remains unchanged.
- Admin construction, identity injection, listener binding and background-worker
  startup are separate steps. Production and standalone admin entrypoints use
  the deferred constructor. Background startup is idempotent across clones.
- Invalid mandatory edge identity or core state still stops startup. Invalid
  optional media/vision configuration or vision state disables those paths,
  preserves the original state and reports a stable reason code.
- N2 Home Assistant and media factories require their Link credential before
  making a request. A missing credential never triggers an anonymous fallback.
  The policy lives in the shared factories used by AdminApi, Hub discovery and
  snapshot, TaskApi and vision executors; it is not limited to WebUI startup.
  When model proxy construction fails but its credential remains valid, an
  authenticated inference request receives an unavailable response, not an
  authentication failure. Missing or incorrect authentication remains rejected.
- Existing `/healthz` includes `startup.profile` and `startup.capabilities`.
  `configured` means configuration accepted, not connected, loaded, ready or
  functionally accepted. Runtime health and actual user operations remain
  separate checks. No new public endpoint is introduced.
  The unified service reports `gate_turns`, `local_inference`, `harborlink` and
  `vision`; the standalone admin health reports only its own capability setup.

## Verification Boundary

The new black-box tests launch the actual unified product executable with
temporary local state and synthetic signed edge assertions. They exercise local
Rules, rejected Gate/model access, invalid mandatory inputs, optional failures
and persisted schedule execution. They do not substitute the standalone admin
program for the product executable.

These tests are not PAM login, real nginx/systemd installation, native RISC-V
inference, camera acceptance or IM platform acceptance. Those results must be
reported separately. A test suite skipped for missing binaries is not evidence
of successful startup isolation.

### Reproduce Product-Entry Tests

Build and preserve two different `harboros-beacon` executables:

| Profile | Build options |
| --- | --- |
| N2 | `cargo build --no-default-features --features fixed-local-models --bin harboros-beacon` |
| N1 | `cargo build --bin harboros-beacon` with the default features enabled |

Do not overwrite the N2 executable when building N1. Set both
`HARBOR_TEST_N2_SERVICE_BIN` and `HARBOR_TEST_N1_SERVICE_BIN` to their absolute
paths, then run from the Beacon repository:

```text
python -m unittest discover -s tests -p test_n2_capability_startup.py -v
```

The driver uses temporary state and synthetic credentials. The model-offline
case reserves the fixed loopback port without listening, preventing a request
to an unrelated local runtime; it skips when that port is already occupied.
Linux worker-count observation requires `/proc`; on Windows that one case skips.
N1 acceptance includes valid file credentials reaching the existing Gate input
validator, not just rejection of invalid startup configurations.

### Local Results, 2026-09-06

Both final executables were built on Windows with Rust 1.94, using `--locked
--offline` and `--config profile.dev.package.harborbeacon_local_agent.debug=0`.
The actual unified-product driver ran 17 tests: 16 passed and only the Linux
`/proc` worker-count case skipped. No physical device was accessed.

| Check | Result |
| --- | --- |
| Service-auth resolver unit tests | 23 passed |
| Startup profile unit tests, N2 | 5 passed |
| Deferred admin startup, N2 | 5 passed |
| Admin Rules regression, N2 | 12 passed |
| Rules automation store, N2 | 16 passed |
| Admin recording / reconciliation, N2 | 12 / 6 passed; one test overlaps |
| Recording validation store, N2 | 32 passed |
| HarborLink media connector | 17 passed in N2 and 17 in default N1 |
| Home Assistant connector | 10 passed in N2 and 10 in default N1 |
| TaskApi HA / shared camera fixtures | 8 passed in N2; 11 passed in default N1 |
| Unified-service model-unavailable / explicit CLI token | 2 / 1 passed in N2 |
| K3 deb packaging tests | 5 passed |

The final read-only review found no remaining blocking issue within B1. It
identified a media-factory bypass in Hub/TaskApi/vision paths, which was closed
centrally and re-tested before both final product executables were rebuilt.
The new factory tests reproduced two failures with the old dispatch; the fixed
N2 dispatch rejects those configurations without any outbound mock request.
The actual product also passes a missing-Link-credential Rules execution case.

The separate N2 NSP-to-HA test did not pass: its fixture tries to change the
model endpoint and receives the existing `LOCAL_MODELS_FIXED` error before
executing HA. The same failure is recorded in the historical 2026-09-05 N2
baseline (`6dab1156555277810d2991c6a41bd9f763102933`), at
`HarborNavi/artifacts/v1/n2-rules-integration/2026-09-05/baseline-n2-lib-serial.log:1055`.
This is historical evidence, not a new clean-`8600584` rerun of that test. It is
not counted as passed. The default N1 run does exercise the NSP HA request path.

The modified source-invariant test
`runtime_has_no_predictable_model_token_fallback` passed after compiling the
original `tests/service_auth_hardening.rs` with `rustc --test` on Windows. Its
Cargo integration invocation tried to build all binaries and hit Windows
page-file exhaustion (OS error 1455); that invocation is not counted as passed.

The broader `test_k3_packaging_contract.py` suite has 9 passes, 9 failures and
1 platform skip on both this change and a clean `8600584` worktree. The failed
test IDs and assertions match; there are no newly introduced failures in that
comparison. Existing template/unit/schema assumptions and Windows line-ending
checks still need reconciliation before package acceptance; they were not
weakened or rewritten to report success in this batch.

The local evidence directory is
`C:/Users/beanw/OpenSource/HarborBeacon/target/n2-capability-startup/20260906`;
it contains `verify-product-startup.log`, build logs and both executables.
These Windows executables are test artifacts, not N2 deployment packages.

| Executable | SHA256 |
| --- | --- |
| `harboros-beacon-n2.exe` | `32bf40771b8c3c8d6fa1c9c63e2f68c2dc1807e6e04deb8122d0acc5102f192e` |
| `harboros-beacon-n1.exe` | `820766dcc4c9166784639dbe1c5557dde3ea19b5bd3abb467a61fe70d37a2d0d` |

## Preserved Constraints And Next Batches

Batch 1 does not remove `ExecStartPre` model-runtime restart or package-generation
validation. The currently installed unit still has pre-process environment-file,
credential, generation and model-start prerequisites. Therefore binary-level
failure isolation does not yet prove arbitrary installed-stack startup failure
isolation. Do not deploy this as a fully completed Option B migration.
Construction and worker startup are separated; this batch is not a complete
runtime supervision or graceful-shutdown redesign. Existing visual-thread
startup limits and execution-resource ownership still need the later lifecycle
work before broader runtime isolation can be claimed.

Batch 2 now implements runtime-owned LLM and classifier execution, admission,
children and confirmed termination; see `n2-execution-ownership.md`. Beacon keeps
business requests, priority policy and results. The private classifier adapter
does not change the Gate v2.0 API or visual business ownership. Native crash,
cancel, timeout and shared AI-resource acceptance is still pending, so the old
restart protection remains.

Batch 3 must replace same-version component coupling with a tested deployment
composition: package identities, compatibility requirements, model/embedding
identities, state read/write versions and restart/rollback groups. Update Depends,
Breaks, postinst and generation validation together; loosening one dependency
alone is not a migration. Initially keep the text runtime and its model assets
as one tested unit; independently version UI and visual assets.

## Rollback

This source change does not intentionally migrate product state or model assets.
Retain the previous package composition and take the normal state snapshot
before an eventual deployment. Do not combine this change with index conversion,
credential rotation or model replacement. If a later batch changes data formats,
define the corresponding data rollback before deployment.

Until native packaging and installed-stack acceptance are complete, `.147`
remains outside this change's verification claim. HTTPS, partition/recovery,
production signatures and market gates remain in V1.5.
