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
: "${MODEL_BUNDLE_ROOT:?MODEL_BUNDLE_ROOT is required}"
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
  echo "error: HarborBeacon cat-vision-runtime release build requires a clean worktree" >&2
  exit 2
fi
for command_name in cargo dpkg-deb python3 riscv64-linux-gnu-gcc sha256sum touch; do
  command -v "$command_name" >/dev/null || {
    echo "error: ${command_name} is required" >&2
    exit 2
  }
done

out_dir="${OUT_DIR:-${repo_root}/dist/harbornavi-k3-debs}"
work_parent="${PACKAGE_WORK_ROOT:-${TMPDIR:-/tmp}}"
mkdir -p "$out_dir" "$work_parent"
build_root="$(mktemp -d "${work_parent%/}/harborcatvision-deb.XXXXXX")"
trap 'rm -rf -- "$build_root"' EXIT
pkg_dir="$build_root/root"
model_stage="$pkg_dir/usr/share/harboros-cat-vision-runtime/models"

python3 scripts/validate_k3_model_materials.py \
  --manifest models/k3-evt1-cat-vision-materials.json \
  --bundle-root "$MODEL_BUNDLE_ROOT" \
  --stage "$model_stage"
cargo metadata --locked --offline --format-version 1 \
  --filter-platform "$target" > "$build_root/cargo-metadata.json"

install -d \
  "$pkg_dir/DEBIAN" \
  "$pkg_dir/usr/lib/harboros-cat-vision-runtime" \
  "$pkg_dir/usr/share/doc/harboros-cat-vision-runtime" \
  "$pkg_dir/usr/share/harboros/component-contracts"
install -m 0755 debian/ensure-cat-vision-runtime-data-layout \
  "$pkg_dir/usr/lib/harboros-cat-vision-runtime/ensure-data-layout"
install -m 0755 scripts/verify_k3_model_release.py \
  "$pkg_dir/usr/lib/harboros-cat-vision-runtime/verify-release"
install -m 0755 scripts/verify_cat_vision_runtime_evidence.py \
  "$pkg_dir/usr/lib/harboros-cat-vision-runtime/verify-evidence"
sed -e "s/VERSION_PLACEHOLDER/${DEBIAN_VERSION}/g" \
  -e "s/ARCH_PLACEHOLDER/${deb_arch}/g" \
  debian/cat-vision-runtime-control.in > "$pkg_dir/DEBIAN/control"
sed -e "s/VERSION_PLACEHOLDER/${DEBIAN_VERSION}/g" \
  -e "s/ARCH_PLACEHOLDER/${deb_arch}/g" \
  debian/cat-vision-runtime-postinst > "$pkg_dir/DEBIAN/postinst"
sed 's/\r$//' debian/cat-vision-runtime-prerm > "$pkg_dir/DEBIAN/prerm"
chmod 0755 "$pkg_dir/DEBIAN/postinst" "$pkg_dir/DEBIAN/prerm"
sed "s/SOURCE_COMMIT_PLACEHOLDER/${source_commit}/g" \
  debian/component-contract-cat-vision-runtime.json.in \
  > "$pkg_dir/usr/share/harboros/component-contracts/harboros-cat-vision-runtime.json"
sed "s/SOURCE_COMMIT_PLACEHOLDER/${source_commit}/g" \
  debian/cat-vision-runtime-manifest.json.in \
  > "$pkg_dir/usr/share/doc/harboros-cat-vision-runtime/runtime-manifest.json"
sed -e "s/SOURCE_COMMIT_PLACEHOLDER/${source_commit}/g" \
  -e "s/VERSION_PLACEHOLDER/${DEBIAN_VERSION}/g" \
  -e "s/ARCH_PLACEHOLDER/${deb_arch}/g" \
  debian/cat-vision-runtime-evidence.json.in \
  > "$pkg_dir/usr/share/doc/harboros-cat-vision-runtime/vision-runtime-evidence.json"
install -m 0644 models/k3-evt1-cat-vision-materials.json \
  "$pkg_dir/usr/share/harboros-cat-vision-runtime/cat-vision-materials.json"
install -m 0644 debian/first-party-rights.json \
  "$pkg_dir/usr/share/doc/harboros-cat-vision-runtime/first-party-rights.json"
install -m 0644 debian/FIRST_PARTY_RIGHTS.txt \
  "$pkg_dir/usr/share/doc/harboros-cat-vision-runtime/FIRST_PARTY_RIGHTS.txt"

python3 scripts/verify_cat_vision_runtime_evidence.py \
  --architecture "$deb_arch" \
  --evidence "$pkg_dir/usr/share/doc/harboros-cat-vision-runtime/vision-runtime-evidence.json" \
  --model-root "$model_stage" \
  --package-version "$DEBIAN_VERSION"
python3 scripts/generate_cargo_license_sidecar.py \
  --package harboros-cat-vision-runtime \
  --no-dependencies \
  --source-commit "$source_commit" \
  --root-manifest "$repo_root/Cargo.toml" \
  --cargo-lock "$repo_root/Cargo.lock" \
  --cargo-metadata "$build_root/cargo-metadata.json" \
  --output "$pkg_dir/usr/share/doc/harboros-cat-vision-runtime/third-party-licenses.json"

python3 scripts/generate_k3_supply_chain.py \
  --package harboros-cat-vision-runtime \
  --no-cargo-dependencies \
  --cargo-lock "$repo_root/Cargo.lock" \
  --cargo-metadata "$build_root/cargo-metadata.json" \
  --first-party-notice "$repo_root/debian/FIRST_PARTY_RIGHTS.txt" \
  --materials "$repo_root/models/k3-evt1-cat-vision-materials.json" \
  --input-file "$repo_root/debian/cat-vision-runtime-control.in" \
  --input-file "$repo_root/debian/cat-vision-runtime-postinst" \
  --input-file "$repo_root/debian/cat-vision-runtime-prerm" \
  --input-file "$repo_root/debian/ensure-cat-vision-runtime-data-layout" \
  --input-file "$repo_root/debian/component-contract-cat-vision-runtime.json.in" \
  --input-file "$repo_root/debian/cat-vision-runtime-manifest.json.in" \
  --input-file "$repo_root/debian/cat-vision-runtime-evidence.json.in" \
  --input-file "$repo_root/debian/first-party-rights.json" \
  --input-file "$repo_root/debian/FIRST_PARTY_RIGHTS.txt" \
  --input-file "$repo_root/scripts/verify_k3_model_release.py" \
  --input-file "$repo_root/scripts/verify_cat_vision_runtime_evidence.py" \
  --model-root "$model_stage" \
  --model-installed-root /usr/share/harboros-cat-vision-runtime/models \
  --runtime-dependency "python3-spacemit-ort=2.0.3+3" \
  --runtime-dependency "spacemit-onnxruntime=2.0.3+3" \
  --runtime-dependency "spacemit-tcm=3.0.0+3" \
  --binary "$pkg_dir/usr/lib/harboros-cat-vision-runtime/verify-release" \
  --binary "$pkg_dir/usr/lib/harboros-cat-vision-runtime/verify-evidence" \
  --version "$DEBIAN_VERSION" \
  --target "$target" \
  --arch "$deb_arch" \
  --source-commit "$source_commit" \
  --source-date-epoch "$SOURCE_DATE_EPOCH" \
  --container-digest "$HARBORBEACON_BUILD_CONTAINER_DIGEST" \
  --debian-snapshot "$HARBORBEACON_DEBIAN_SNAPSHOT" \
  --output-dir "$pkg_dir/usr/share/doc/harboros-cat-vision-runtime"

find "$pkg_dir" -type d -exec chmod u-s,g-s,o-t {} +
[[ -z "$(find "$pkg_dir" -type d -perm /7000 -print -quit)" ]] || {
  echo "error: package directory retains special mode bits" >&2
  exit 2
}
find "$pkg_dir" -print0 | xargs -0 touch --no-dereference --date="@${SOURCE_DATE_EPOCH}"
artifact="${out_dir}/harboros-cat-vision-runtime_${DEBIAN_VERSION}_${deb_arch}.deb"
artifact_name="$(basename "$artifact")"
material_prefix="${artifact_name%.deb}"
SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" dpkg-deb \
  --root-owner-group --build --uniform-compression -Zxz -z9 "$pkg_dir" "$artifact"
python3 scripts/generate_package_provenance.py \
  --package harboros-cat-vision-runtime \
  --version "$DEBIAN_VERSION" \
  --arch "$deb_arch" \
  --artifact "$artifact" \
  --build-provenance "$pkg_dir/usr/share/doc/harboros-cat-vision-runtime/build-provenance.json" \
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
  --package harboros-cat-vision-runtime \
  --version "$DEBIAN_VERSION" \
  --architecture "$deb_arch" \
  --source-commit "$source_commit" \
  --root-manifest "$repo_root/Cargo.toml" \
  --cargo-lock "$repo_root/Cargo.lock" \
  --cargo-metadata "$build_root/cargo-metadata.json" \
  --no-cargo-dependencies \
  --component-contract "$pkg_dir/usr/share/harboros/component-contracts/harboros-cat-vision-runtime.json" \
  --component-contract-installed-path /usr/share/harboros/component-contracts/harboros-cat-vision-runtime.json \
  --first-party-rights "$pkg_dir/usr/share/doc/harboros-cat-vision-runtime/first-party-rights.json" \
  --first-party-notice "$pkg_dir/usr/share/doc/harboros-cat-vision-runtime/FIRST_PARTY_RIGHTS.txt" \
  --third-party-licenses "$pkg_dir/usr/share/doc/harboros-cat-vision-runtime/third-party-licenses.json" \
  --sbom-spdx "$pkg_dir/usr/share/doc/harboros-cat-vision-runtime/sbom.spdx.json" \
  --sbom-cyclonedx "$pkg_dir/usr/share/doc/harboros-cat-vision-runtime/sbom.cdx.json" \
  --build-provenance "$pkg_dir/usr/share/doc/harboros-cat-vision-runtime/build-provenance.json" \
  --package-provenance "$out_dir/${material_prefix}.package-provenance.json" \
  --model-materials "$pkg_dir/usr/share/harboros-cat-vision-runtime/cat-vision-materials.json" \
  --model-materials-installed-path /usr/share/harboros-cat-vision-runtime/cat-vision-materials.json \
  --vision-runtime-evidence "$pkg_dir/usr/share/doc/harboros-cat-vision-runtime/vision-runtime-evidence.json" \
  --vision-model-root "$model_stage" \
  --output-dir "$out_dir"
printf '%s\n' "$artifact"
