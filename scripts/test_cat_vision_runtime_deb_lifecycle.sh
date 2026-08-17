#!/usr/bin/env bash
set -euo pipefail

artifact="${1:?usage: test_cat_vision_runtime_deb_lifecycle.sh CAT_VISION_RUNTIME_DEB}"
[[ -f "$artifact" ]] || { echo "error: package not found: $artifact" >&2; exit 2; }
[[ "$(id -u)" == 0 ]] || { echo "error: lifecycle test requires root" >&2; exit 2; }
[[ -f /.dockerenv && "${HARBOR_CAT_VISION_LIFECYCLE_DISPOSABLE:-}" == 1 ]] || {
  echo "error: lifecycle test is restricted to an explicitly disposable container" >&2
  exit 2
}
[[ ! -e /data/vision-models ]] || {
  echo "error: disposable container already has /data/vision-models" >&2
  exit 2
}

work_root="$(mktemp -d /tmp/harbor-cat-vision-lifecycle.XXXXXX)"
cleanup() {
  rm -rf -- "$work_root" /data/vision-models
  rm -rf -- \
    /usr/share/harboros-cat-vision-runtime \
    /usr/lib/harboros-cat-vision-runtime
  rm -rf -- /run/harboros-k3-generation
  rmdir /run/systemd/system 2>/dev/null || true
}
trap cleanup EXIT
dpkg-deb --control "$artifact" "$work_root/control"
dpkg-deb --extract "$artifact" "$work_root/root"

for script in \
  "$work_root/control/postinst" \
  "$work_root/control/prerm" \
  "$work_root/root/usr/lib/harboros-cat-vision-runtime/ensure-data-layout"; do
  sh -n "$script"
done
python3 -m py_compile \
  "$work_root/root/usr/lib/harboros-cat-vision-runtime/verify-release" \
  "$work_root/root/usr/lib/harboros-cat-vision-runtime/verify-evidence"

control="$work_root/control/control"
for dependency in \
  'python3-spacemit-ort (= 2.0.3+3)' \
  'spacemit-onnxruntime (= 2.0.3+3)' \
  'spacemit-tcm (= 3.0.0+3)'; do
  grep -F -- "$dependency" "$control" >/dev/null
done
test -z "$(find "$work_root/root" -path '*/systemd/system/*' -type f -print -quit)"
! grep -R -E -- 'ExecStart=|127\.0\.0\.1:|0\.0\.0\.0:' "$work_root/root" >/dev/null

package_version="$(dpkg-deb --field "$artifact" Version)"
release_root="/data/vision-models/releases/$package_version"
manifest="/usr/share/harboros-cat-vision-runtime/cat-vision-materials.json"
verifier="/usr/lib/harboros-cat-vision-runtime/verify-release"
install -d /usr/share/harboros-cat-vision-runtime /usr/lib/harboros-cat-vision-runtime
cp -a "$work_root/root/usr/share/harboros-cat-vision-runtime/." \
  /usr/share/harboros-cat-vision-runtime/
cp -a "$work_root/root/usr/lib/harboros-cat-vision-runtime/." \
  /usr/lib/harboros-cat-vision-runtime/

"$work_root/control/postinst" configure
test "$(readlink /data/vision-models/current)" = "releases/$package_version"
"$verifier" --manifest "$manifest" --root "$release_root"
test "$(stat -c '%U:%G:%a' /data/vision-models)" = "root:root:755"
test "$(stat -c '%U:%G:%a' /data/vision-models/releases)" = "root:root:755"
test -z "$(find "$release_root" \( ! -user root -o ! -group root \) -print -quit)"
test -z "$(find "$release_root" -type f ! -perm 0444 -print -quit)"

mapfile -t locked_paths < <(python3 - "$manifest" <<'PY'
import json
import sys
for material in json.load(open(sys.argv[1], encoding="utf-8"))["materials"]:
    for entry in material["files"]:
        print(entry["package_path"])
PY
)
test "${#locked_paths[@]}" = 2
first_path="${locked_paths[0]}"
second_path="${locked_paths[1]}"

canary="$work_root/symlink-canary"
printf '%s\n' untouched > "$canary"
printf '%s\n' tampered > "$release_root/$first_path"
rm -f -- "$release_root/$second_path"
ln -s "$canary" "$release_root/$second_path"
install -d "$release_root/unexpected-empty-directory"
printf '%s\n' extra > "$release_root/unexpected-file"
"$work_root/control/postinst" configure
"$verifier" --manifest "$manifest" --root "$release_root"
test "$(cat "$canary")" = untouched

current_before="$(readlink /data/vision-models/current)"
ln -s releases/attacker /data/vision-models/current.new
if "$work_root/control/postinst" configure; then
  echo "error: postinst accepted stale current.new" >&2
  exit 1
fi
test "$(readlink /data/vision-models/current)" = "$current_before"
rm -f -- /data/vision-models/current.new

# A live Beacon is stopped and recorded before any successful pointer rewrite.
mock_bin="$work_root/mock-bin"
systemctl_log="$work_root/systemctl.log"
beacon_state="$work_root/beacon.active"
install -d "$mock_bin" /run/systemd/system
printf '%s\n' 1 > "$beacon_state"
cat > "$mock_bin/systemctl" <<'SH'
#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$HARBOR_TEST_SYSTEMCTL_LOG"
case "${1:-}" in
  is-active) [ "$(cat "$HARBOR_TEST_BEACON_STATE")" = 1 ] ;;
  stop) printf '%s\n' 0 > "$HARBOR_TEST_BEACON_STATE" ;;
esac
exit 0
SH
chmod 0755 "$mock_bin/systemctl"
PATH="$mock_bin:$PATH" \
  HARBOR_TEST_SYSTEMCTL_LOG="$systemctl_log" \
  HARBOR_TEST_BEACON_STATE="$beacon_state" \
  "$work_root/control/postinst" configure
test "$(cat "$beacon_state")" = 0
test "$(stat -c '%U:%G:%a:%s' /run/harboros-k3-generation/beacon-was-active)" = \
  root:root:600:24
test "$(cat /run/harboros-k3-generation/beacon-was-active)" = harboros-beacon.service
grep -Fx -- 'stop harboros-beacon.service' "$systemctl_log" >/dev/null
! grep -F -- 'restart harboros-beacon.service' "$systemctl_log" >/dev/null
rmdir /run/systemd/system

install -d /data/vision-models/releases/predecessor
printf '%s\n' predecessor > /data/vision-models/releases/predecessor/marker
rm -f -- /data/vision-models/current
ln -s releases/predecessor /data/vision-models/current
cp -a "$verifier" "${verifier}.real"
cat > "$verifier" <<'SH'
#!/bin/sh
set -eu
"${0}.real" "$@"
count_file="${HARBOR_TEST_VERIFY_COUNT:?}"
count=0
[ ! -f "$count_file" ] || count="$(cat "$count_file")"
count=$((count + 1))
printf '%s\n' "$count" > "$count_file"
if [ "$count" -eq 3 ]; then
  case "${HARBOR_TEST_VERIFY_FAULT:-}" in
    fail) exit 42 ;;
    term) kill -TERM "$PPID"; exit 143 ;;
  esac
fi
SH
chmod 0755 "$verifier"

for fault in fail term; do
  printf '%s\n' "old-$fault" > "$release_root/rollback.marker"
  count_file="$work_root/verify-$fault.count"
  if HARBOR_TEST_VERIFY_COUNT="$count_file" HARBOR_TEST_VERIFY_FAULT="$fault" \
    "$work_root/control/postinst" configure; then
    echo "error: postinst ignored verifier $fault injection" >&2
    exit 1
  fi
  test "$(readlink /data/vision-models/current)" = releases/predecessor
  test "$(cat "$release_root/rollback.marker")" = "old-$fault"
done
mv -f -- "${verifier}.real" "$verifier"
rm -f -- "$release_root/rollback.marker" /data/vision-models/current
ln -s "releases/$package_version" /data/vision-models/current
"$verifier" --manifest "$manifest" --root "$release_root"

"$work_root/control/prerm" remove
test "$(readlink /data/vision-models/current)" = "releases/$package_version"
test -f "$release_root/$first_path"
echo "cat vision runtime transactional lifecycle passed"
