# N2 Execution Ownership: Option B Batch 2

Date: 2026-09-06. Source implementation and local verification only. This is
not a RISC-V package, installed-system acceptance or removal of the existing
restart/generation barriers. See `n2-capability-startup.md` for the B1 baseline.

## Decision

The fixed model runtime owns both N2 LLM execution and the recording classifier's
pure probe/inference execution. Its process-local scheduler now arbitrates their
shared AI cluster. Beacon keeps business requests, prompts, frame selection,
thresholds, aggregation, events, recording and persistent state. CPU embedding
execution has cancellation/child ownership but does not acquire an AI lease.

The Beacon model proxy no longer holds an LLM lease in its own process. N2
classifier entrypoints use only the private runtime adapter, with no local CLI
fallback on RPC failure. N1 retains its existing embedded/direct execution paths.
No Gate v2.0 envelope, public API, RAG UI or camera business algorithm changes.

## Execution Lifetime

1. Runtime registers an execution before body admission or queuing. Callers use
   a fresh UUID in `X-Harbor-Execution-Id`; absent IDs are generated locally.
2. A queued request observes cancellation and its admission-time deadline before
   starting. The scheduler also checks cancellation while waiting for a lease.
3. LLM acquires the runtime lease before lazy child startup. The classifier
   validates and stages its frames before acquiring the same runtime scheduler.
4. Execution completion or confirmed child termination releases the lease before
   sending the HTTP response. An unconfirmed owner drop quarantines the resource.
   Classifier timeouts and recognized EP/runtime failures retain quarantine even
   when the OS child has exited, preserving the previous failure policy.
5. A caller timeout/read failure sends best-effort cancellation for that UUID.
   Caller disappearance cannot release the runtime lease. Cancellation delivery
   is never itself a stop acknowledgement.

Chat/embedding admission deadline is 90 seconds. Classifier admission deadline
is 80 seconds, with a maximum 60-second scheduler wait and a 15-second command
budget. Cleanup may extend beyond those deadlines: classifier TERM grace is up
to 5 seconds, forced reap up to 5 seconds, and pipe draining up to 500 ms.
These are not hard upload socket timeouts.

Execution registry capacity is 64, with 128 retained completion records.
Duplicate retained IDs conflict; the protocol is not permanent idempotency.
Each chat/embedding/classifier admission queue is bounded to four pending jobs.
The job-scoped LLM cancellation monitor is joined before the next job starts.

## Private Adapter

Only the authenticated loopback listener at `127.0.0.1:8792` serves:

| Method | Path | Purpose |
| --- | --- | --- |
| POST | `/internal/ai/classifier` | Pure classifier probe or frame inference |
| GET | `/internal/ai/executions/<uuid>` | Current/retained execution status |
| POST | `/internal/ai/executions/<uuid>/cancel` | Request cancellation |

The existing model Bearer token is required. Beacon's public model proxy
allowlist does not forward these routes. Health uses the runtime scheduler's
snapshot, not Beacon's now-independent process-local scheduler.

Classifier body: a four-byte big-endian JSON manifest length, the manifest, then
concatenated raw frame bytes. The manifest contains schema version, probe flag,
expected model SHA256, threshold in ppm and indexed frame lengths. It does not
accept caller paths, commands or a model-selection URL. Content-Length is
required and must equal the declared payload. Limits are 16 KiB manifest,
1..9 frames for inference and 32 MiB per frame; probes contain no frames.
Frames are streamed unchanged into runtime-owned private temporary files.
There is no base64 expansion, resizing or shared Beacon temporary path.

The runtime's own configuration selects Python, script, model and core affinity.
It validates the existing classifier output contract and returns pure results;
Beacon still decides recording acceptance. Normal completion and pre-execution
failure remove staged files. An uncertain child exit preserves the temporary
directory until service/runtime recovery can clean it safely.

## Package And Configuration Compatibility

Both installed units already read `/etc/default/harboros-fixed-models`. This
provides their shared model token. Default classifier paths match the Beacon
package's installed script (0755) and model (0644), readable by `harbormodel`.
The runtime package control and canonical manifest now also declare `python3-pil`;
their exact dependency contract test is updated together. NumPy and the fixed
SpaceMIT runtime dependencies remain unchanged. The LLM vendor library path is
set only on its child, not globally on classifier Python.

Beacon-only environment/drop-in overrides are not inherited by runtime. Any
non-default classifier script/model/hash/Python/affinity configuration must be
reviewed and placed in the shared environment file before deployment; threshold
remains a caller-supplied business parameter. Do not blindly copy Beacon-only
credentials into the runtime. Script/model ownership remains in the Beacon
package until B3's component composition work.

A standalone runtime package can pass text health while classifier materials
are absent. That is a local classifier-unavailable result, not proof of full
visual capability. Native acceptance must probe the classifier as `harbormodel`
with the actual installed EP and device permissions.

## Verification And Remaining Limits

The final Windows MSVC build succeeded for the actual default N1 Beacon, fixed
N2 Beacon and fixed model-runtime executables. The local test groups cover
execution registry, scheduler, owned real children, classifier framing and
loopback transport, fixed-runtime handlers and the Beacon proxy. Real child
fixtures are test executables, not real LLM/ONNX inference.

| Check | Result |
| --- | --- |
| Execution registry and scheduler, N2 and N1 | 4 + 15 passed in each profile |
| Owned child/process tests, N2 and N1 | 10 passed in each profile, including one fixture |
| Classifier RPC, N2 | 19 passed |
| Fixed runtime facade, N2 | 13 passed, including one fixture |
| Beacon model proxy, N2 | 3 passed |
| Existing classifier affinity, N2 | 1 passed |
| Existing Python classifier / quality scripts | 10 / 9 passed |
| K3 deb packaging | 5 passed |
| Model-runtime rights / exact dependency contract | 10 / 1 passed; dependency check overlaps rights |
| Rebuilt real N1/N2 product startup driver | 16 passed, 1 Linux-only skip, 34.201 seconds |

The handler fixture terminates and reaps an independent HTTP caller process,
proves that LLM still holds the lease, and observes a classifier contender waiting
until execution completion. A separate owned HTTP-child fixture tests cancellation
interrupting blocked inference, followed by reap before reuse. The initial health
lock regression and classifier protocol stubs failed before implementation and
passed after integration. These tests do not substitute for native process-group
or actual inference tests. Response-body interruption and an unreachable cancel
endpoint are still narrower proxy test gaps.

Build/test evidence and preserved Windows binaries are under
`C:/Users/beanw/OpenSource/HarborBeacon/target/n2-execution-ownership/20260906`.
The B1 artifact directory remains separate and unchanged.

| Binary | SHA256 |
| --- | --- |
| `harboros-beacon-n1.exe` | `3ffba3ed8b5358f8d5d4d08c4905bfdc2f3403fcbe5512d89716bf7e0c06827f` |
| `harboros-beacon-n2.exe` | `ff9ce8c7ab4163ddb830f80fb9ae4c4d2a8185dda9f74c1be831d857cc24a0e0` |
| `harbor-fixed-model-api.exe` | `93a1c4e99aada04b1c65b726077aa07177419732ed984df86d71fa0cf90ebbfa` |

Use Cargo 1.94 with `--offline -j 1` and
`--config profile.dev.package.harborbeacon_local_agent.debug=0` to limit Windows
page-file pressure. N2 uses `--no-default-features --features fixed-local-models`;
N1 uses default features. Run library groups with `cargo test --lib <group>` and
the facade/proxy with their explicit `--bin` target, all with
`-- --test-threads=1`. The product driver is
`python -m unittest discover -s tests -p test_n2_capability_startup.py -v`, with
`HARBOR_TEST_N1_SERVICE_BIN` and `HARBOR_TEST_N2_SERVICE_BIN` set to those preserved
executables. It never substitutes a standalone Admin mock for the product.

The broader packaging-contract failures documented in B1 are historical baseline
results, not a full B2 suite pass. This batch synchronizes the newly added Pillow
dependency and fixes canonical LF only for the changed manifest template; it
does not weaken dependency validation or repair unrelated packaging assertions.

Windows proves direct-child kill/reap and bounded pipe handling, not Unix process
groups or systemd cgroups. Unix uses a process group; Linux also installs a
direct-child parent-death signal and checks the parent race. The four Unix/Linux
tests were subsequently executed in the native fixture acceptance below.
Escaped sessions/descendants are not a complete process-tree guarantee. Keep
`KillMode=control-group`, restart and generation validation in place.

The existing tiny_http transport has blocking body reads and synchronous draining
when a rejected/early-response request is dropped. Separate bounded admission
keeps accepted body parsing out of the main loop, but does not guarantee health
or cancellation responsiveness for every slow or malformed request. Cancellation
checks prevent later execution once a stalled read returns; they cannot interrupt
the read itself. Socket-level deadlines remain an explicit transport follow-up.

Official N2 configuration disallows custom local VLM endpoints. Manually built
in-process model states and the offline classifier quality script still bypass
this ownership boundary. Do not run those tools concurrently with the shared AI
runtime and do not claim all arbitrary executables are coordinated.

No device, model, credentials, account, camera, IM platform, commit or GitHub push
was changed in the implementation batch. The user subsequently approved a
same-generation whole-set trial deployment and deferred B3.2 independent package
upgrades. Actual installed startup, Rules and model calls must pass before trial
acceptance; broader crash/cancel/timeout recovery remains required before
removing existing runtime restart protection.

## Linux Native Fixture Acceptance: 2026-09-06

On the same-day user-confirmed build host `192.168.3.78`, a disposable amd64
container ran the frozen dirty B1/B2 source snapshot, including the new modules
and vendor directory. It used independent Cargo/TMP directories, copied dependency
cache, no network, a non-root user and no host product-service mounts. Rust/Cargo
were 1.96.1 with explicit target `x86_64-unknown-linux-gnu` and `-j 1`.

All six groups passed: owned processes 13, execution registry 4, scheduler 15,
classifier RPC 19, fixed runtime facade 13 and model proxy 3. Total: 67, with
no platform skips. Counts include test-process fixtures; the proxy's nested
child-test summary is not counted a second time. All commands exited zero and
the temporary container was removed.

The Linux parent-death and process-group tests are now executed evidence, not
Windows-only assumptions. Their bounds still matter: a parent-death test treats
a zombie as no longer running, and an inherited-process-group case permits
`exit_confirmed=false`. Passing therefore does not prove that PID 1 reaped every
orphan or that every process tree was fully recovered. The tests correctly retain
uncertain-exit quarantine rather than declaring the resource reusable.

Snapshot SHA256:
`dde321255d6f8e8ec0259472cc4b164c3dcdc9b020ab38814160dc1835be2b58`.
Image ID:
`sha256:285c215aac471c5d3037838b2d75d98cd7eef6f7535845a56da8c0008ce6ae90`.
Local evidence is in HarborNavi
`artifacts/v1/n2-option-b-native/2026-09-06/evidence`, including the toolchain,
per-group commands/results and logs. The remote isolated test root is
`/mnt/database_backup/harbor-builds/harbor-innovations/work/n2-b2-native.sbjEb1xu`.

This is Linux kernel/HTTP process-fixture evidence, not RISC-V, actual
SpaceMIT/LLM inference, installed-user permissions or systemd/cgroup acceptance.
It does not remove the existing model restart precondition. The snapshot predates
the B3.1 component-contract declarations and does not qualify later package
composition code or release artifacts.
