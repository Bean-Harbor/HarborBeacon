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

install -d \
  "$pkg_dir/DEBIAN" \
  "$pkg_dir/usr/bin" \
  "$pkg_dir/usr/lib/harborbeacon" \
  "$pkg_dir/usr/lib/harboros-beacon" \
  "$pkg_dir/usr/lib/systemd/system" \
  "$pkg_dir/usr/share/doc/harboros-beacon" \
  "$pkg_dir/usr/share/harboros"
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
install -m 0755 debian/ensure-beacon-data-layout \
  "$pkg_dir/usr/lib/harborbeacon/ensure-data-layout"
install -m 0644 debian/harboros-beacon.service \
  "$pkg_dir/usr/lib/systemd/system/harboros-beacon.service"
install -m 0644 debian/first-party-rights.json \
  "$pkg_dir/usr/share/doc/harboros-beacon/first-party-rights.json"
install -m 0644 debian/FIRST_PARTY_RIGHTS.txt \
  "$pkg_dir/usr/share/doc/harboros-beacon/FIRST_PARTY_RIGHTS.txt"
sed -e "s/VERSION_PLACEHOLDER/${DEBIAN_VERSION}/g" \
  -e "s/ARCH_PLACEHOLDER/${deb_arch}/g" debian/control \
  | sed 's/^Depends: .*/Depends: libc6, ca-certificates, adduser, init-system-helpers, harboros-system (>= 0.1.0~evt.1), harboros-system (<< 0.2), harborlink (>= 0.1.0~evt.1), harborlink (<< 0.2), harboros-model-runtime (>= 0.1.0~evt.1), harboros-model-runtime (<< 0.2), python3, python3-opencv, python3-spacemit-ort/' \
  > "$pkg_dir/DEBIAN/control"
sed 's/\r$//' debian/postinst > "$pkg_dir/DEBIAN/postinst"
sed 's/\r$//' debian/prerm > "$pkg_dir/DEBIAN/prerm"
chmod 0755 "$pkg_dir/DEBIAN/postinst" "$pkg_dir/DEBIAN/prerm"
sed "s/SOURCE_COMMIT_PLACEHOLDER/${source_commit}/g" \
  debian/component-contract-beacon.json.in \
  > "$pkg_dir/usr/share/harboros/component-contract.json"

python3 scripts/generate_k3_supply_chain.py \
  --package harboros-beacon \
  --cargo-lock "$repo_root/Cargo.lock" \
  --cargo-metadata "$build_root/cargo-metadata.json" \
  --input-file "$repo_root/debian/first-party-rights.json" \
  --input-file "$repo_root/debian/FIRST_PARTY_RIGHTS.txt" \
  --binary "$pkg_dir/usr/bin/harboros-beacon" \
  --version "$DEBIAN_VERSION" \
  --target "$target" \
  --arch "$deb_arch" \
  --source-commit "$source_commit" \
  --source-date-epoch "$SOURCE_DATE_EPOCH" \
  --container-digest "$HARBORBEACON_BUILD_CONTAINER_DIGEST" \
  --debian-snapshot "$HARBORBEACON_DEBIAN_SNAPSHOT" \
  --output-dir "$pkg_dir/usr/share/doc/harboros-beacon"

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
  --sbom-spdx "$pkg_dir/usr/share/doc/harboros-beacon/sbom.spdx.json" \
  --sbom-cyclonedx "$pkg_dir/usr/share/doc/harboros-beacon/sbom.cdx.json" \
  --build-provenance "$pkg_dir/usr/share/doc/harboros-beacon/build-provenance.json" \
  --package-provenance "$out_dir/${material_prefix}.package-provenance.json" \
  --output-dir "$out_dir"
printf '%s\n' "$artifact"
