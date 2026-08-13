#!/usr/bin/env bash
set -euo pipefail

output="${1:?usage: build_model_runtime_lifecycle_fixture.sh OUTPUT_DEB}"
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work_root="$(mktemp -d /tmp/harbor-model-fixture.XXXXXX)"
trap 'rm -rf -- "$work_root"' EXIT
pkg_root="$work_root/root"
version="0.1.0~evt.1+lifecycle"
model_root="$pkg_root/usr/share/harboros-model-runtime/models"

install -d \
  "$pkg_root/DEBIAN" \
  "$model_root/chat" \
  "$pkg_root/usr/lib/harboros-model-runtime" \
  "$pkg_root/usr/lib/systemd/system"
printf '%s\n' '{"model_type":"qwen2"}' > "$model_root/chat/config.json"
printf '%s\n' '{"version":"1.0"}' > "$model_root/chat/tokenizer.json"
printf '%s\n' 'fixture-weights' > "$model_root/chat/model.safetensors"

manifest="$pkg_root/usr/share/harboros-model-runtime/model-materials.json"
{
  printf '%s\n' '{'
  printf '%s\n' '  "schema_version": 1,'
  printf '%s\n' '  "release_id": "lifecycle-fixture",'
  printf '%s\n' '  "materials": ['
  printf '%s\n' '    {'
  printf '%s\n' '      "id": "lifecycle-model",'
  printf '%s\n' '      "role": "test",'
  printf '%s\n' '      "state": "locked",'
  printf '%s\n' '      "files": ['
  index=0
  for relative in chat/config.json chat/tokenizer.json chat/model.safetensors; do
    file="$model_root/$relative"
    [ "$index" -eq 0 ] || printf '%s\n' ','
    printf '        {"package_path":"%s","size":%s,"sha256":"%s"}' \
      "$relative" "$(stat -c %s "$file")" "$(sha256sum "$file" | cut -d ' ' -f 1)"
    index=$((index + 1))
  done
  printf '%s\n' ''
  printf '%s\n' '      ]'
  printf '%s\n' '    }'
  printf '%s\n' '  ]'
  printf '%s\n' '}'
} > "$manifest"

install -m 0755 "$repo_root/debian/ensure-model-runtime-data-layout" \
  "$pkg_root/usr/lib/harboros-model-runtime/ensure-data-layout"
install -m 0755 "$repo_root/debian/wait-model-runtime-health" \
  "$pkg_root/usr/lib/harboros-model-runtime/wait-health"
install -m 0755 "$repo_root/scripts/verify_k3_model_release.py" \
  "$pkg_root/usr/lib/harboros-model-runtime/verify-release"
install -m 0644 "$repo_root/debian/harboros-model-runtime.service" \
  "$pkg_root/usr/lib/systemd/system/harboros-model-runtime.service"
install -m 0644 "$repo_root/debian/harboros-vlm-runtime.service" \
  "$pkg_root/usr/lib/systemd/system/harboros-vlm-runtime.service"
sed -e "s/VERSION_PLACEHOLDER/${version}/g" \
  -e 's/ARCH_PLACEHOLDER/all/g' \
  "$repo_root/debian/model-runtime-control.in" > "$pkg_root/DEBIAN/control"
sed "s/VERSION_PLACEHOLDER/${version}/g" \
  "$repo_root/debian/model-runtime-postinst" > "$pkg_root/DEBIAN/postinst"
install -m 0755 "$repo_root/debian/model-runtime-prerm" "$pkg_root/DEBIAN/prerm"
chmod 0755 "$pkg_root/DEBIAN/postinst"

mkdir -p "$(dirname "$output")"
dpkg-deb --root-owner-group --build "$pkg_root" "$output" >/dev/null
printf '%s\n' "$output"
