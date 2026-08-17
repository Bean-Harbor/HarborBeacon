#!/usr/bin/env bash
set -euo pipefail

output="${1:?usage: build_cat_vision_runtime_lifecycle_fixture.sh OUTPUT_DEB}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work_root="$(mktemp -d /tmp/harbor-cat-vision-fixture.XXXXXX)"
trap 'rm -rf -- "$work_root"' EXIT
pkg_root="$work_root/root"
version="0.1.0~evt.1+lifecycle"
model_root="$pkg_root/usr/share/harboros-cat-vision-runtime/models"

install -d \
  "$pkg_root/DEBIAN" \
  "$model_root/detection" \
  "$pkg_root/usr/lib/harboros-cat-vision-runtime" \
  "$pkg_root/usr/share/doc/harboros-cat-vision-runtime"
printf '%s\n' 'fixture-yolo' > "$model_root/detection/yolo.onnx"
printf '%s\n' 'cat' > "$model_root/detection/labels.txt"

manifest="$pkg_root/usr/share/harboros-cat-vision-runtime/cat-vision-materials.json"
{
  printf '%s\n' '{'
  printf '%s\n' '  "schema_version": 1,'
  printf '%s\n' '  "release_id": "lifecycle-fixture",'
  printf '%s\n' '  "materials": ['
  index=0
  for relative in detection/yolo.onnx detection/labels.txt; do
    file="$model_root/$relative"
    [ "$index" -eq 0 ] || printf '%s\n' ','
    printf '    {"id":"fixture-%s","state":"locked","files":[{"package_path":"%s","size":%s,"sha256":"%s"}]}' \
      "$index" "$relative" "$(stat -c %s "$file")" "$(sha256sum "$file" | cut -d ' ' -f 1)"
    index=$((index + 1))
  done
  printf '%s\n' ''
  printf '%s\n' '  ]'
  printf '%s\n' '}'
} > "$manifest"
printf '%s\n' '{}' \
  > "$pkg_root/usr/share/doc/harboros-cat-vision-runtime/vision-runtime-evidence.json"

install -m 0755 "$repo_root/debian/ensure-cat-vision-runtime-data-layout" \
  "$pkg_root/usr/lib/harboros-cat-vision-runtime/ensure-data-layout"
install -m 0755 "$repo_root/scripts/verify_k3_model_release.py" \
  "$pkg_root/usr/lib/harboros-cat-vision-runtime/verify-release"
printf '%s\n' '#!/usr/bin/env python3' 'raise SystemExit(0)' \
  > "$pkg_root/usr/lib/harboros-cat-vision-runtime/verify-evidence"
chmod 0755 "$pkg_root/usr/lib/harboros-cat-vision-runtime/verify-evidence"
sed -e "s/VERSION_PLACEHOLDER/${version}/g" \
  -e 's/ARCH_PLACEHOLDER/riscv64/g' \
  "$repo_root/debian/cat-vision-runtime-control.in" > "$pkg_root/DEBIAN/control"
sed -e "s/VERSION_PLACEHOLDER/${version}/g" \
  -e 's/ARCH_PLACEHOLDER/riscv64/g' \
  "$repo_root/debian/cat-vision-runtime-postinst" > "$pkg_root/DEBIAN/postinst"
install -m 0755 "$repo_root/debian/cat-vision-runtime-prerm" "$pkg_root/DEBIAN/prerm"
chmod 0755 "$pkg_root/DEBIAN/postinst"

mkdir -p "$(dirname "$output")"
dpkg-deb --root-owner-group --build "$pkg_root" "$output" >/dev/null
printf '%s\n' "$output"
