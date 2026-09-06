# ProductJob Functional Preview

Date: 2026-09-06. Framework implementation for JOB-01..03 / HOS-06 / WEB-04.
Branch: `codex/n2-product-jobs-20260906`, based on deployed backend `722dfe5`.

## Delivered Flow

Settings -> Advanced Tasks -> Export rules history creates a persistent job.
The real executor snapshots the existing Rules history once and writes a JSON
download. The page shows status, measured record progress, cancellation, retry,
result download and history after refresh. It does not execute another rule.

The first job kind is `rules_history_export`. This is a first consumer of shared
product task semantics, not adoption by every existing background operation.
No model, knowledge, camera or DVR executor was moved into this implementation.

## Ownership And Interface

Beacon owns task state, execution and output. HarborOS owns the existing signed
session edge and CSRF enforcement; its WebUI consumes HTTP/JSON. The UI package
must include the new product-jobs path in the signed-edge location. Both the
backend and UI must be delivered together. Gate/Beacon v2.0 remains unchanged.

Public same-origin prefix: `/api/harbor-beacon/product-jobs`.

| Method/path | Request/result |
| --- | --- |
| GET collection | `{jobs: ProductJob[]}`, only authenticated actor/home |
| POST collection | `{job_type: "rules_history_export", idempotency_key}`; 202 new / 200 replay |
| GET `/{id}` | `{job: ProductJob}`, with derived `can_cancel/can_retry` |
| POST `/{id}/cancel` | Persist cancellation request; return current job |
| POST `/{id}/retry` | `{idempotency_key}`; create a distinct job referencing the failed/cancelled/interrupted source |
| GET `/{id}/result` | Authenticated JSON attachment, successful jobs only |

Unknown request fields/types are rejected. Actor and home come from the existing
verified edge/service principal. A second administrator cannot list, cancel or
download another actor's job. Creation/cancellation/retry require `AdminManage`;
read/download require `AdminReadState` and matching job ownership. There is no
query/body actor override or local-owner fallback on these routes.

## Persistence And Execution

`<admin-state stem>.product-jobs/` contains a versioned `state.json`, an exclusive
writer lock and job-ID-named partial/final outputs. State replacement is atomic;
the directory is private on Unix. The store opens lazily, so a damaged task store
does not stop the product or Rules. Invalid existing state is preserved.

States are `queued -> running -> succeeded | failed | cancelled | interrupted`.
Cancellation is a request until the worker has stopped and removed its partial
output. Export checks cancellation after snapshotting and between 64-row batches.
The initial snapshot is not interruptible; a pending request stays running during
that read. The final flush/commit phase is explicitly non-cancellable. The same
store lock serializes cancel/finalize so only one terminal result wins.

After process restart, the first task-store access marks unfinished jobs
`interrupted`, removes unpublished outputs and retains successful history. Work
is never automatically replayed after a restart. Retry uses a new job and the
then-current Rules snapshot, while retaining the original terminal record.

Create, start, finalization, cancel request, retry and terminal audit events are
persisted with the job in the same transaction. Progress is measured in rows;
no synthetic delays or simulated progress are used in the product executor.

The JSON export contains run/rule identifiers, revisions, times, normalized
status and action counts. It excludes executor messages, payloads, model names,
endpoints and internal filesystem paths. It is a status-history export, not a
full rule-definition backup or an archive of action message text.

Limits: four active exports, 128 retained jobs, 32 MiB per export, 256 MiB total
reserved/retained export capacity and 4 MiB task metadata. Reaching a limit gives
a capacity error. Automatic history retention/result deletion is not implemented
in this first consumer; no existing job or replay key is silently evicted.

## Verification

- ProductJob library: 5/5 fixed-local-model tests and 5/5 default-feature tests.
- Real compiled N2 unified service: 7/7 HTTP cases, including concurrent replay,
  signed actor isolation, export/download, running cancel, subsequent work,
  process termination/recovery, damaged task storage and repaired-source retry.
- Existing N2 startup suite: 12 passed, one Linux `/proc` case skipped on Windows.
  N1 startup class was not run in that invocation; its library tests above passed.
- WebUI: 8 new task component/API tests plus 24 existing Rules tests passed;
  production build passed. Nginx static contract/lifecycle checks passed under
  Git Bash; actual Debian package checks skipped because dpkg-deb was unavailable.
- Playwright used the real compiled Beacon with a disposable local session.
  It confirmed running before cancellation, retried that exact job, downloaded
  30,000 fixture rows, refreshed history, filtered tasks and checked 1440/390/320px
  layouts. No page JavaScript errors or mobile document overflow were observed.

Commands: `cargo test --lib product_jobs` (also with
`--no-default-features --features fixed-local-models`), build `harboros-beacon`,
set `HARBOR_TEST_N2_SERVICE_BIN` and run `tests/test_product_jobs_entrypoint.py`.
The existing `test_n2_capability_startup.py` remains the product startup driver.
WebUI uses its normal npm build/tests and `tests/product-jobs.browser.cjs`.

`tests/serve_product_jobs_preview.py` starts a loopback-only preview with fresh
temporary state and fixture identity. It must not serve installed device data.
Large history fixtures are derived from one real rule execution; 30,000 rows
are not 30,000 independently executed or accepted Rules runs.

## Delivery And Rollback

This change has not been packaged for RISC-V or installed on `.147`; its installed
four-package version remains `.20260906.2`. Local executable/UI validation does
not replace installed Linux, real PAM/nginx or package rollback acceptance.

The next device batch must package this backend plus the matching signed-edge
WebUI in the existing same-generation whole-set composition. Include the new
task directory in the fresh private backup. Rollback restores the prior package
set; the old binary ignores this added directory. Preserve it for evidence and
later recovery rather than replaying work. No existing schema was migrated.

B3.2 remains deferred and the model restart protection remains. Export-worker
cancellation does not establish model inference cancellation, AI contention or
installed runtime crash recovery. Neyyeby retains NSR/knowledge ownership;
b246b retains camera/media/event/DVR ownership. Full JOB-01..03 adoption across
upload, parse, index, delete, update and recovery remains future integration.
