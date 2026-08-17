#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

target="${RUST_TARGET:-riscv64gc-unknown-linux-gnu}"
deb_arch="${DEB_ARCH:-riscv64}"
[[ "${target}:${deb_arch}" == "riscv64gc-unknown-linux-gnu:riscv64" ]] || {
  echo "error: K3 package requires riscv64gc-unknown-linux-gnu/riscv64" >&2
  exit 2
}
: "${DEBIAN_VERSION:?DEBIAN_VERSION is required}"
: "${SOURCE_DATE_EPOCH:?SOURCE_DATE_EPOCH is required}"
: "${HARBORBEACON_BUILD_CONTAINER_DIGEST:?HARBORBEACON_BUILD_CONTAINER_DIGEST is required}"
: "${HARBORBEACON_DEBIAN_SNAPSHOT:?HARBORBEACON_DEBIAN_SNAPSHOT is required}"
[[ "$SOURCE_DATE_EPOCH" =~ ^[0-9]+$ ]] || { echo "error: invalid SOURCE_DATE_EPOCH" >&2; exit 2; }
dpkg --validate-version "$DEBIAN_VERSION"
[[ "$HARBORBEACON_BUILD_CONTAINER_DIGEST" =~ ^sha256:[0-9a-f]{64}$ ]] || {
  echo "error: HARBORBEACON_BUILD_CONTAINER_DIGEST must be a sha256 digest" >&2
  exit 2
}
[[ "$HARBORBEACON_DEBIAN_SNAPSHOT" =~ ^[0-9]{8}T[0-9]{6}Z$ ]] || {
  echo "error: HARBORBEACON_DEBIAN_SNAPSHOT must be an immutable snapshot timestamp" >&2
  exit 2
}

source_commit="${SOURCE_COMMIT:-$(git rev-parse HEAD)}"
[[ "$source_commit" =~ ^[0-9a-f]{40}$ && "$source_commit" == "$(git rev-parse HEAD)" ]] || {
  echo "error: SOURCE_COMMIT must match the checked out full commit" >&2
  exit 2
}
if [[ -n "$(git status --porcelain --untracked-files=all)" ]]; then
  echo "error: HarborBeacon release build requires a clean worktree" >&2
  exit 2
fi
for command_name in cargo dpkg-deb python3 riscv64-linux-gnu-gcc sha256sum touch; do
  command -v "$command_name" >/dev/null || { echo "error: ${command_name} is required" >&2; exit 2; }
done
export CARGO_TARGET_RISCV64GC_UNKNOWN_LINUX_GNU_LINKER="${CARGO_TARGET_RISCV64GC_UNKNOWN_LINUX_GNU_LINKER:-riscv64-linux-gnu-gcc}"
export CARGO_INCREMENTAL=0

out_dir="${OUT_DIR:-${repo_root}/dist/harbornavi-k3-debs}"
work_parent="${PACKAGE_WORK_ROOT:-${TMPDIR:-/tmp}}"
mkdir -p "$out_dir" "$work_parent"
build_root="$(mktemp -d "${work_parent%/}/harborbeacon-deb.XXXXXX")"
trap 'rm -rf -- "$build_root"' EXIT
pkg_dir="${build_root}/root"
cargo_target_dir="${CARGO_TARGET_DIR:-${repo_root}/target}"
mkdir -p "$cargo_target_dir"
cargo_target_dir="$(cd "$cargo_target_dir" && pwd -P)"
export RUSTFLAGS="${RUSTFLAGS:+${RUSTFLAGS} }--remap-path-prefix=${cargo_target_dir}=./target --remap-path-prefix=${repo_root}=."

cargo build --locked --release --target "$target" \
  --no-default-features --features external-model-runtime \
  --bin harboros-beacon \
  --bin cat-sampling-plan \
  --bin harbornavi-k3-local-vision-smoke \
  --bin harbornavi-k3-multi-vision-smoke \
  --bin harbornavi-ha-mqtt-event-contract-smoke
if cargo tree --locked --target "$target" \
  --no-default-features --features external-model-runtime \
  | grep -Eq '(^| )candle-(core|nn|transformers) '; then
  echo "error: K3 Beacon dependency tree still contains the embedded model runtime" >&2
  exit 2
fi
cargo metadata --locked --offline --format-version 1 \
  --filter-platform "$target" \
  --no-default-features --features external-model-runtime \
  > "$build_root/cargo-metadata.json"

model_source_dir="config/harbornavi-k3/vision-models/mobilenetv2-cat-binary-v2-20260806"
model_install_dir="$pkg_dir/usr/share/harboros-beacon/vision-models/mobilenetv2-cat-binary-v2-20260806"
install -d \
  "$pkg_dir/DEBIAN" \
  "$pkg_dir/usr/bin" \
  "$pkg_dir/usr/lib/harborbeacon" \
  "$pkg_dir/usr/lib/harboros-beacon" \
  "$pkg_dir/usr/lib/systemd/system" \
  "$pkg_dir/usr/share/doc/harboros-beacon" \
  "$pkg_dir/usr/share/harboros" \
  "$model_install_dir"
for binary in \
  harboros-beacon \
  harbornavi-k3-local-vision-smoke \
  harbornavi-k3-multi-vision-smoke \
  harbornavi-ha-mqtt-event-contract-smoke; do
  install -m 0755 "$cargo_target_dir/$target/release/$binary" "$pkg_dir/usr/bin/$binary"
done
install -m 0755 scripts/harbornavi_k3_yolov8_analyzer.py \
  "$pkg_dir/usr/lib/harboros-beacon/harbornavi_k3_yolov8_analyzer.py"
install -m 0755 scripts/harbornavi_k3_yolo_stream_worker.py \
  "$pkg_dir/usr/lib/harboros-beacon/harbornavi_k3_yolo_stream_worker.py"
install -m 0755 scripts/harbornavi_k3_cat_recording_classifier.py \
  "$pkg_dir/usr/lib/harboros-beacon/harbornavi_k3_cat_recording_classifier.py"
install -m 0755 scripts/harbornavi_k3_cat_quality_runner.py \
  "$pkg_dir/usr/lib/harboros-beacon/cat-quality-runner"
install -m 0755 "$cargo_target_dir/$target/release/cat-sampling-plan" \
  "$pkg_dir/usr/lib/harboros-beacon/cat-sampling-plan"
install -m 0755 debian/ensure-beacon-data-layout \
  "$pkg_dir/usr/lib/harborbeacon/ensure-data-layout"
install -m 0755 debian/migrate-cat-activity-state \
  "$pkg_dir/usr/lib/harborbeacon/migrate-cat-activity-state"
install -m 0755 debian/verify-beacon-k3-generation \
  "$pkg_dir/usr/lib/harborbeacon/verify-k3-generation"
install -m 0644 debian/harboros-beacon.service \
  "$pkg_dir/usr/lib/systemd/system/harboros-beacon.service"
install -m 0644 \
  "$model_source_dir/mobilenetv2_cat_binary_int8.onnx" \
  "$model_source_dir/runtime-contract.json" \
  "$model_source_dir/first-party-provenance.json" \
  "$model_install_dir/"
install -m 0644 debian/first-party-rights.json \
  "$pkg_dir/usr/share/doc/harboros-beacon/first-party-rights.json"
install -m 0644 debian/FIRST_PARTY_RIGHTS.txt \
  "$pkg_dir/usr/share/doc/harboros-beacon/FIRST_PARTY_RIGHTS.txt"
python3 scripts/generate_cargo_license_sidecar.py \
  --package harboros-beacon \
  --source-commit "$source_commit" \
  --root-manifest "$repo_root/Cargo.toml" \
  --cargo-lock "$repo_root/Cargo.lock" \
  --cargo-metadata "$build_root/cargo-metadata.json" \
  --output "$pkg_dir/usr/share/doc/harboros-beacon/third-party-licenses.json"
sed -e "s/VERSION_PLACEHOLDER/${DEBIAN_VERSION}/g" \
  -e "s/ARCH_PLACEHOLDER/${deb_arch}/g" debian/control \
  | sed "s/^Depends: .*/Depends: libc6, openssl, ca-certificates, adduser, init-system-helpers, harboros-system (>= 0.1.0~evt.1), harboros-system (<< 0.2), harborlink (>= 0.1.0~evt.1), harborlink (<< 0.2), harboros-model-runtime (= ${DEBIAN_VERSION}), harboros-cat-vision-runtime (= ${DEBIAN_VERSION}), ffmpeg, python3, python3-numpy, python3-pil, python3-opencv/" \
  > "$pkg_dir/DEBIAN/control"
sed 's/\r$//' debian/postinst > "$pkg_dir/DEBIAN/postinst"
sed 's/\r$//' debian/prerm > "$pkg_dir/DEBIAN/prerm"
chmod 0755 "$pkg_dir/DEBIAN/postinst" "$pkg_dir/DEBIAN/prerm"
sed "s/SOURCE_COMMIT_PLACEHOLDER/${source_commit}/g" \
  debian/component-contract-beacon.json.in \
  > "$pkg_dir/usr/share/harboros/component-contract.json"

cat > "$pkg_dir/usr/share/doc/harboros-beacon/harbornavi-k3-package.txt" <<EOF
HarborNavi K3 local vision event package
debian_version=${DEBIAN_VERSION}
rust_target=${target}
deb_arch=${deb_arch}
analyzer=/usr/lib/harboros-beacon/harbornavi_k3_yolov8_analyzer.py
stream_worker=/usr/lib/harboros-beacon/harbornavi_k3_yolo_stream_worker.py
cat_recording_validator=mobilenet_v2_int8
cat_recording_classifier=/usr/lib/harboros-beacon/harbornavi_k3_cat_recording_classifier.py
cat_quality_runner=/usr/lib/harboros-beacon/cat-quality-runner
cat_sampling_plan=/usr/lib/harboros-beacon/cat-sampling-plan
cat_recording_classifier_model=/usr/share/harboros-beacon/vision-models/mobilenetv2-cat-binary-v2-20260806/mobilenetv2_cat_binary_int8.onnx
cat_recording_classifier_sha256=d0c1bdcf973ca7f6efc6e62af764ff59300e0d27abbc75c20c7f86515769d825
cat_recording_classifier_policy=up_to_9_frames_at_least_3_positive
single_runner=/usr/bin/harbornavi-k3-local-vision-smoke
multi_runner=/usr/bin/harbornavi-k3-multi-vision-smoke
ha_mqtt_runner=/usr/bin/harbornavi-ha-mqtt-event-contract-smoke
model_runtime_service=harboros-model-runtime.service
model_api=http://127.0.0.1:8792/v1
default_model=/data/vision-models/current/detection/yolov8n_192x320.q.onnx
default_labels=/data/vision-models/current/detection/label.txt
capture_modes=oneshot_ffmpeg,persistent_ffmpeg,local_restream
fixed_rate_scheduler=enabled
default_four_channel_phase_offsets=0ms,2500ms,5000ms,7500ms
persistent_capture_root=/run/harbornavi/capture
EOF

python3 scripts/generate_k3_supply_chain.py \
  --package harboros-beacon \
  --cargo-lock "$repo_root/Cargo.lock" \
  --cargo-metadata "$build_root/cargo-metadata.json" \
  --first-party-notice "$repo_root/debian/FIRST_PARTY_RIGHTS.txt" \
  --input-file "$repo_root/debian/first-party-rights.json" \
  --input-file "$repo_root/debian/FIRST_PARTY_RIGHTS.txt" \
  --input-file "$repo_root/debian/verify-beacon-k3-generation" \
  --input-file "$repo_root/scripts/harbornavi_k3_cat_recording_classifier.py" \
  --input-file "$repo_root/scripts/harbornavi_k3_cat_quality_runner.py" \
  --input-file "$repo_root/src/bin/cat_sampling_plan.rs" \
  --input-file "$repo_root/src/runtime/cat_recording_sampling.rs" \
  --input-file "$repo_root/$model_source_dir/mobilenetv2_cat_binary_int8.onnx" \
  --input-file "$repo_root/$model_source_dir/runtime-contract.json" \
  --input-file "$repo_root/$model_source_dir/first-party-provenance.json" \
  --binary "$pkg_dir/usr/bin/harboros-beacon" \
  --binary "$pkg_dir/usr/lib/harboros-beacon/cat-sampling-plan" \
  --version "$DEBIAN_VERSION" \
  --target "$target" \
  --arch "$deb_arch" \
  --source-commit "$source_commit" \
  --source-date-epoch "$SOURCE_DATE_EPOCH" \
  --container-digest "$HARBORBEACON_BUILD_CONTAINER_DIGEST" \
  --debian-snapshot "$HARBORBEACON_DEBIAN_SNAPSHOT" \
  --output-dir "$pkg_dir/usr/share/doc/harboros-beacon"

find "$pkg_dir" -type d -exec chmod u-s,g-s,o-t {} +
[[ -z "$(find "$pkg_dir" -type d -perm /7000 -print -quit)" ]] || {
  echo "error: package directory retains special mode bits" >&2
  exit 2
}
find "$pkg_dir" -print0 | xargs -0 touch --no-dereference --date="@${SOURCE_DATE_EPOCH}"
artifact="${out_dir}/harboros-beacon_${DEBIAN_VERSION}_${deb_arch}.deb"
artifact_name="$(basename "$artifact")"
material_prefix="${artifact_name%.deb}"
SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" dpkg-deb \
  --root-owner-group --build --uniform-compression -Zxz -z9 "$pkg_dir" "$artifact"
python3 scripts/generate_package_provenance.py \
  --package harboros-beacon \
  --version "$DEBIAN_VERSION" \
  --arch "$deb_arch" \
  --artifact "$artifact" \
  --build-provenance "$pkg_dir/usr/share/doc/harboros-beacon/build-provenance.json" \
  --source-commit "$source_commit" \
  --source-date-epoch "$SOURCE_DATE_EPOCH" \
  --container-digest "$HARBORBEACON_BUILD_CONTAINER_DIGEST" \
  --debian-snapshot "$HARBORBEACON_DEBIAN_SNAPSHOT" \
  --output "$out_dir/${material_prefix}.package-provenance.json"
(
  cd "$out_dir"
  sha256sum "$artifact_name" > "${artifact_name}.sha256"
)
python3 scripts/generate_package_materials.py \
  --artifact "$artifact" \
  --package harboros-beacon \
  --version "$DEBIAN_VERSION" \
  --architecture "$deb_arch" \
  --source-commit "$source_commit" \
  --root-manifest "$repo_root/Cargo.toml" \
  --cargo-lock "$repo_root/Cargo.lock" \
  --cargo-metadata "$build_root/cargo-metadata.json" \
  --component-contract "$pkg_dir/usr/share/harboros/component-contract.json" \
  --component-contract-installed-path /usr/share/harboros/component-contract.json \
  --first-party-rights "$pkg_dir/usr/share/doc/harboros-beacon/first-party-rights.json" \
  --first-party-notice "$pkg_dir/usr/share/doc/harboros-beacon/FIRST_PARTY_RIGHTS.txt" \
  --third-party-licenses "$pkg_dir/usr/share/doc/harboros-beacon/third-party-licenses.json" \
  --sbom-spdx "$pkg_dir/usr/share/doc/harboros-beacon/sbom.spdx.json" \
  --sbom-cyclonedx "$pkg_dir/usr/share/doc/harboros-beacon/sbom.cdx.json" \
  --build-provenance "$pkg_dir/usr/share/doc/harboros-beacon/build-provenance.json" \
  --package-provenance "$out_dir/${material_prefix}.package-provenance.json" \
  --output-dir "$out_dir"
printf '%s\n' "$artifact"
