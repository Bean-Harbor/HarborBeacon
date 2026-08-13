#!/usr/bin/env bash
set -euo pipefail

artifact="${1:?usage: test_model_runtime_deb_lifecycle.sh MODEL_RUNTIME_DEB}"
[[ -f "$artifact" ]] || { echo "error: package not found: $artifact" >&2; exit 2; }
[[ "$(id -u)" == 0 ]] || { echo "error: lifecycle test requires root" >&2; exit 2; }
[[ -f /.dockerenv && "${HARBOR_MODEL_LIFECYCLE_DISPOSABLE:-}" == 1 ]] || {
  echo "error: lifecycle test is restricted to an explicitly disposable container" >&2
  exit 2
}
[[ ! -e /data/models ]] || {
  echo "error: disposable container already has /data/models" >&2
  exit 2
}

work_root="$(mktemp -d /tmp/harbor-model-lifecycle.XXXXXX)"
trap 'rm -rf -- "$work_root"' EXIT
dpkg-deb --control "$artifact" "$work_root/control"
dpkg-deb --extract "$artifact" "$work_root/root"

control="$work_root/control/control"
for dependency in \
  'llama.cpp-tools-spacemit (= 0.1.1)' \
  'spacemit-onnxruntime (= 2.0.3+3)' \
  'spacemit-tcm (= 3.0.0+3)'; do
  grep -F -- "$dependency" "$control" >/dev/null
done
for unit in harboros-model-runtime.service harboros-vlm-runtime.service; do
  test -f "$work_root/root/usr/lib/systemd/system/$unit"
  grep -F -- "$unit" "$work_root/control/postinst" >/dev/null
  grep -F -- "$unit" "$work_root/control/prerm" >/dev/null
done
grep -F -- 'ReadOnlyPaths=/data/models/current' \
  "$work_root/root/usr/lib/systemd/system/harboros-vlm-runtime.service" >/dev/null

package_version="$(dpkg-deb --field "$artifact" Version)"
install -d /usr/share/harboros-model-runtime /usr/lib/harboros-model-runtime /usr/lib/systemd/system
cp -a "$work_root/root/usr/share/harboros-model-runtime/." /usr/share/harboros-model-runtime/
cp -a "$work_root/root/usr/lib/harboros-model-runtime/." /usr/lib/harboros-model-runtime/
cp -a "$work_root/root/usr/lib/systemd/system/harboros-model-runtime.service" \
  "$work_root/root/usr/lib/systemd/system/harboros-vlm-runtime.service" \
  /usr/lib/systemd/system/
"$work_root/control/postinst" configure
test "$(readlink /data/models/current)" = "releases/$package_version"
test -f /data/models/current/vlm/Qwen3.5-0.8B/qwen3_5vl_0.8b-text-q41.gguf
test -f /data/models/current/vlm/Qwen3.5-0.8B/qwen3_5vl_0.8b-vision-384-op23.f16.onnx
test ! -e /data/models/current/vlm/Qwen3.5-0.8B/qwen3_5vl_0.8b-vision-224-op23.f16.onnx
test ! -e /data/models/current/vlm/Qwen3.5-0.8B/qwen3_5vl_0.8b-vision-768-op23.f16.onnx

"$work_root/control/postinst" configure
test "$(readlink /data/models/current)" = "releases/$package_version"
"$work_root/control/prerm" remove
test "$(readlink /data/models/current)" = "releases/$package_version"
test -f "/data/models/releases/$package_version/vlm/Qwen3.5-0.8B/config.json"

rm -rf -- /data/models
rm -rf -- /usr/share/harboros-model-runtime /usr/lib/harboros-model-runtime
rm -f -- \
  /usr/lib/systemd/system/harboros-model-runtime.service \
  /usr/lib/systemd/system/harboros-vlm-runtime.service
echo "model runtime install/reinstall/remove lifecycle passed"
