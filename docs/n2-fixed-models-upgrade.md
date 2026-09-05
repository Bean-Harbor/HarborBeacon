# N2 Fixed-Model Upgrade And Rollback

Status on 2026-09-05: procedure prepared; coordinated package installation and
full rollback have not yet been exercised. Native vendor and embedding probes
have passed in isolation. Do not label this procedure verified until the
installation, business loop, and rollback records are attached.

## Build Preconditions

1. Use the confirmed Linux build host and a clean committed checkout. Record
   the source SHA, actual container digest, immutable Debian snapshot, Rust
   toolchain, and package version. Do not reuse a previous package version for
   a different model generation.
2. Provide the exact model bundle and license evidence required by
   `models/k3-evt1-model-materials.json`, plus both pinned vendor archives from
   `models/n2-vendor-runtime.json`. Existing evidence/review gates stay in force.
3. Build runtime with `scripts/build_model_runtime_k3_deb.sh`, then Beacon with
   `scripts/build_harbornavi_k3_deb.sh`, and the matching HarborOS K3
   `harbornavi-assistant-webui` package.
   The existing K3 generation verifier also requires
   `harboros-cat-vision-runtime` to have the same version as Beacon. Rebuild that
   asset package for this generation with the existing vision assets and runtime
   evidence; include it in the coordinated install and rollback package set.
   Run the output-set, dependency, installed-file, license and provenance checks.
   N2 Cargo features must exclude Candle and `hf-hub` in the resolved target graph.
4. Compare package file ownership before installation. Vendor libraries belong
   only below the model runtime's private directory. The existing visual
   `spacemit-onnxruntime`, `python3-spacemit-ort`, and TCM packages must keep their
   qualified versions; stop if dependency resolution would change them.

## Device Backup

1. Record `dpkg-query` versions and `dpkg -L` file ownership for Beacon, model
   runtime, WebUI, ONNX Runtime, Python ORT, TCM, and any old llama package. Record
   enabled/active service state, drop-ins, `readlink /data/models/current`, free
   space, and current health. Keep credentials out of the published record.
2. Obtain exact old `.deb` files with matching versions and verify their hashes.
   Save them offline on the device and off-device. A version list or copied
   executable alone is insufficient for package rollback.
3. Pause imports and scheduled indexing, then stop Beacon and model runtime.
   Confirm all old service-control-group processes have exited. Stop the legacy
   VLM unit if it is installed; retain its original enabled state in the backup.
4. While services are stopped, make a root-only backup of `/data/harborbeacon`,
   the actual configured index directory (including any external index root),
   `/etc/default` model/Beacon files, service drop-ins, and product generation
   metadata. Preserve ownership, modes, symlinks and timestamps. Cloud secrets
   and conversations remain private in this backup. Original knowledge folders
   remain in place and must not be modified by the upgrade.
5. Save the old model symlink and retain the complete old release directory as
   offline rollback material. Verify archive readability and hashes before
   installing anything. Ensure space for old/new model generations and index
   backups. Record the backup paths in the device upgrade record.

## Coordinated Installation

1. Install the matching runtime, cat vision assets, Beacon and WebUI package set with dependency
   resolution constrained to the already qualified visual packages. Keep Beacon
   stopped while runtime/model generation and UI packages are changing.
2. The runtime package stages and verifies the new immutable model generation,
   switches `/data/models/current`, and creates the shared model API token.
   Verify the generation contract before starting Beacon. The runtime's health
   endpoint can be reachable while its chat model is still warming.
3. Start runtime, then Beacon. Beacon warms chat through its own scheduled
   inference facade. Confirm loopback listeners 4174, 8792, and managed child
   8793. No new external listener is allowed. Confirm Candle and retired model
   processes are absent from active services and cgroups.
4. Verify authenticated chat and embeddings, per-worker readiness, fixed model
   identities and SHA pins. Exercise child failure/retry, timeout cancellation,
   queue pressure with responsive health, and full service restart. An uncertain
   child exit must quarantine resources, not admit overlapping AI work.
5. Run the existing index job for each enabled source. Observe progress and
   cancellation, retained `pre-fixed-models` backups, and text fallback during
   rebuild. Complete import, search, and cited question answering with the new
   identity. Check cloud settings and allowed fallback without exposing API keys.
6. Check desktop/mobile readonly model status and legacy links on the installed
   WebUI. Run available YOLO/cat fixtures with text inference and record provider,
   affinity, queue wait, latency, and peak memory. Real-camera acceptance requires
   real-camera evidence and is separate from synthetic tensor probes.

## Full Rollback

1. Stop Beacon, runtime and any managed VLM process. Confirm all new runtime
   children exited before allowing any old model process to start.
2. Restore the exact old runtime, cat vision assets, Beacon and WebUI `.deb` set from the verified
   backup. Keep their services stopped until data and model identity are restored.
   Restore old service files/drop-ins and token/configuration files with their
   original ownership and permissions.
3. Move the new derived index/state into a separate root-only recovery directory;
   restore the complete old state and index generation together. Restore the old
   `/data/models/current` symlink and verify that it resolves to the backed-up
   release. Do not merge new vectors into the old index or reuse the new identity.
4. Run `systemctl daemon-reload`, restore the recorded enablement, then start the
   old runtime and Beacon in their original order. Verify exact installed versions,
   health, model identity, credentials, conversations, and a known cited query.
5. Retain upgrade and rollback logs, package hashes and both data generations
   until acceptance. Do not purge old packages or model releases during the first
   qualification cycle.

Rollback evidence must show the actual old package versions, restored index and
model identities, successful health, and a reproducible query with citations.
A unit test restoring an index manifest is only part of this verification.
