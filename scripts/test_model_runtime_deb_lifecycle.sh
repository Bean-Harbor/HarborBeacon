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
cleanup() {
  rm -rf -- "$work_root" /data/models
  rm -rf -- /usr/share/harboros-model-runtime /usr/lib/harboros-model-runtime
  rm -f -- \
    /usr/lib/systemd/system/harboros-model-runtime.service \
    /usr/lib/systemd/system/harboros-vlm-runtime.service
  rm -rf -- /run/harboros-model-runtime
  rmdir /run/systemd/system 2>/dev/null || true
}
trap cleanup EXIT
dpkg-deb --control "$artifact" "$work_root/control"
dpkg-deb --extract "$artifact" "$work_root/root"

for script in \
  "$work_root/control/postinst" \
  "$work_root/control/prerm" \
  "$work_root/root/usr/lib/harboros-model-runtime/ensure-data-layout" \
  "$work_root/root/usr/lib/harboros-model-runtime/wait-health"; do
  sh -n "$script"
done
python3 -m py_compile "$work_root/root/usr/lib/harboros-model-runtime/verify-release"

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
  grep -F -- 'ReadOnlyPaths=/data/models' \
    "$work_root/root/usr/lib/systemd/system/$unit" >/dev/null
done
grep -F -- 'TimeoutStartSec=75' \
  "$work_root/root/usr/lib/systemd/system/harboros-model-runtime.service" >/dev/null
grep -F -- 'TimeoutStartSec=315' \
  "$work_root/root/usr/lib/systemd/system/harboros-vlm-runtime.service" >/dev/null

package_version="$(dpkg-deb --field "$artifact" Version)"
release_root="/data/models/releases/$package_version"
manifest="/usr/share/harboros-model-runtime/model-materials.json"
verifier="/usr/lib/harboros-model-runtime/verify-release"
install -d /usr/share/harboros-model-runtime /usr/lib/harboros-model-runtime /usr/lib/systemd/system
cp -a "$work_root/root/usr/share/harboros-model-runtime/." /usr/share/harboros-model-runtime/
cp -a "$work_root/root/usr/lib/harboros-model-runtime/." /usr/lib/harboros-model-runtime/
cp -a "$work_root/root/usr/lib/systemd/system/harboros-model-runtime.service" \
  "$work_root/root/usr/lib/systemd/system/harboros-vlm-runtime.service" \
  /usr/lib/systemd/system/

# Chroot-style configure installs both core units without attempting to start them.
"$work_root/control/postinst" configure
test "$(readlink /data/models/current)" = "releases/$package_version"
"$verifier" --manifest "$manifest" --root "$release_root"
test "$(stat -c '%U:%G:%a' /data/models)" = "root:root:755"
test "$(stat -c '%U:%G:%a' /data/models/releases)" = "root:root:755"
test "$(stat -c '%U:%G' /data/models/current)" = "root:root"
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
(( ${#locked_paths[@]} >= 3 ))
first_path="${locked_paths[0]}"
second_path="${locked_paths[1]}"
third_path="${locked_paths[2]}"

# The runtime account cannot alter a release, delete a locked file, or repoint current.
if runuser -u harbormodel -- sh -c "printf x >> '$release_root/$first_path'" 2>/dev/null; then
  echo "error: harbormodel modified a locked model file" >&2
  exit 1
fi
if runuser -u harbormodel -- rm -f -- "$release_root/$second_path" 2>/dev/null; then
  echo "error: harbormodel deleted a locked model file" >&2
  exit 1
fi
if runuser -u harbormodel -- ln -s releases/attacker /data/models/current.new 2>/dev/null; then
  echo "error: harbormodel created current.new" >&2
  exit 1
fi
if runuser -u harbormodel -- ln -s /tmp/attacker \
  "/data/models/releases/.${package_version}.install.attacker" 2>/dev/null; then
  echo "error: harbormodel created a predictable staging entry" >&2
  exit 1
fi

# Reinstall atomically heals tampering, deletion, extra paths, and symlink substitution.
canary="$work_root/symlink-canary"
printf '%s\n' untouched > "$canary"
printf '%s\n' tampered > "$release_root/$first_path"
rm -f -- "$release_root/$second_path" "$release_root/$third_path"
ln -s "$canary" "$release_root/$third_path"
install -d "$release_root/unexpected-empty-directory"
printf '%s\n' extra > "$release_root/unexpected-file"
"$work_root/control/postinst" configure
"$verifier" --manifest "$manifest" --root "$release_root"
test "$(cat "$canary")" = untouched

# A stale current.new fails before state capture and cannot remove a valid current.
current_before="$(readlink /data/models/current)"
ln -s releases/attacker /data/models/current.new
if "$work_root/control/postinst" configure; then
  echo "error: postinst accepted stale current.new" >&2
  exit 1
fi
test "$(readlink /data/models/current)" = "$current_before"
rm -f -- /data/models/current.new

mock_bin="$work_root/mock-bin"
systemctl_log="$work_root/systemctl.log"
install -d "$mock_bin" /run/systemd/system
cat > "$mock_bin/systemctl" <<'SH'
#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$HARBOR_TEST_SYSTEMCTL_LOG"
case "${1:-}" in
  is-active)
    case "${3:-}" in
      harboros-model-runtime.service) [ "${HARBOR_TEST_MODEL_ACTIVE:-0}" = 1 ] ;;
      harboros-vlm-runtime.service) [ "${HARBOR_TEST_VLM_ACTIVE:-0}" = 1 ] ;;
      *) exit 3 ;;
    esac
    ;;
  restart)
    if [ "${2:-}" = harboros-vlm-runtime.service ] \
      && [ -n "${HARBOR_TEST_FAIL_VLM_ONCE:-}" ] \
      && [ ! -e "$HARBOR_TEST_FAIL_VLM_ONCE" ]; then
      : > "$HARBOR_TEST_FAIL_VLM_ONCE"
      exit 1
    fi
    ;;
esac
exit 0
SH
chmod 0755 "$mock_bin/systemctl"

# The actual prerm upgrade path must synchronously stop both units.
: > "$systemctl_log"
PATH="$mock_bin:$PATH" HARBOR_TEST_SYSTEMCTL_LOG="$systemctl_log" \
  HARBOR_TEST_MODEL_ACTIVE=1 HARBOR_TEST_VLM_ACTIVE=0 \
  "$work_root/control/prerm" upgrade
grep -Fx -- 'stop harboros-model-runtime.service harboros-vlm-runtime.service' \
  "$systemctl_log" >/dev/null
grep -Fx -- 'harboros-model-runtime.service' \
  /run/harboros-model-runtime/upgrade-active >/dev/null
if grep -Fx -- 'harboros-vlm-runtime.service' \
  /run/harboros-model-runtime/upgrade-active >/dev/null; then
  echo "error: prerm recorded an inactive unit as active" >&2
  exit 1
fi

# Failed startup restores the previous release/current and only units active before upgrade.
install -d /data/models/releases/predecessor
printf '%s\n' predecessor > /data/models/releases/predecessor/marker
rm -f -- /data/models/current
ln -s releases/predecessor /data/models/current
printf '%s\n' old-release > "$release_root/rollback.marker"
: > "$systemctl_log"
fail_once="$work_root/fail-vlm-once"
PATH="$mock_bin:$PATH" HARBOR_TEST_SYSTEMCTL_LOG="$systemctl_log" \
  HARBOR_TEST_MODEL_ACTIVE=1 HARBOR_TEST_VLM_ACTIVE=0 \
  "$work_root/control/prerm" upgrade
if PATH="$mock_bin:$PATH" \
  HARBOR_TEST_SYSTEMCTL_LOG="$systemctl_log" \
  HARBOR_TEST_MODEL_ACTIVE=0 HARBOR_TEST_VLM_ACTIVE=0 \
  HARBOR_TEST_FAIL_VLM_ONCE="$fail_once" \
  "$work_root/control/postinst" configure; then
  echo "error: postinst ignored a core unit startup failure" >&2
  exit 1
fi
test "$(readlink /data/models/current)" = releases/predecessor
test "$(cat "$release_root/rollback.marker")" = old-release
test ! -e /run/harboros-model-runtime/upgrade-active
model_restart_count="$(grep -Fc 'restart harboros-model-runtime.service' "$systemctl_log" || true)"
vlm_restart_count="$(grep -Fc 'restart harboros-vlm-runtime.service' "$systemctl_log" || true)"
test "$model_restart_count" = 2
test "$vlm_restart_count" = 1
test "$(tail -n 1 "$systemctl_log")" = 'restart harboros-model-runtime.service'

# A post-move verification failure and TERM both restore the sole previous tree.
rm -f -- "$release_root/rollback.marker"
rm -f -- "$fail_once"
rm -f -- /run/harboros-model-runtime/upgrade-active
PATH="$mock_bin:$PATH" HARBOR_TEST_SYSTEMCTL_LOG="$systemctl_log" \
  HARBOR_TEST_MODEL_ACTIVE=0 HARBOR_TEST_VLM_ACTIVE=0 \
  "$work_root/control/postinst" configure
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
  rm -f -- /data/models/current
  ln -s releases/predecessor /data/models/current
  count_file="$work_root/verify-$fault.count"
  if PATH="$mock_bin:$PATH" \
    HARBOR_TEST_SYSTEMCTL_LOG="$systemctl_log" \
    HARBOR_TEST_MODEL_ACTIVE=0 HARBOR_TEST_VLM_ACTIVE=0 \
    HARBOR_TEST_VERIFY_COUNT="$count_file" HARBOR_TEST_VERIFY_FAULT="$fault" \
    "$work_root/control/postinst" configure; then
    echo "error: postinst ignored verifier $fault injection" >&2
    exit 1
  fi
  test "$(readlink /data/models/current)" = releases/predecessor
  test "$(cat "$release_root/rollback.marker")" = "old-$fault"
done
mv -f -- "${verifier}.real" "$verifier"
rm -f -- "$release_root/rollback.marker" /data/models/current
ln -s "releases/$package_version" /data/models/current
"$verifier" --manifest "$manifest" --root "$release_root"

# Cold-start polling succeeds after transient failures and stops at a deadline.
health_count="$work_root/health-success.count"
cat > "$mock_bin/curl-success" <<'SH'
#!/bin/sh
set -eu
count=0
[ ! -f "$HARBOR_TEST_HEALTH_COUNT" ] || count="$(cat "$HARBOR_TEST_HEALTH_COUNT")"
count=$((count + 1))
printf '%s\n' "$count" > "$HARBOR_TEST_HEALTH_COUNT"
[ "$count" -ge 3 ]
SH
cat > "$mock_bin/curl-fail" <<'SH'
#!/bin/sh
exit 1
SH
cat > "$mock_bin/date-deadline" <<'SH'
#!/bin/sh
set -eu
count=0
[ ! -f "$HARBOR_TEST_DATE_COUNT" ] || count="$(cat "$HARBOR_TEST_DATE_COUNT")"
count=$((count + 1))
printf '%s\n' "$count" > "$HARBOR_TEST_DATE_COUNT"
if [ "$count" -le 2 ]; then printf '%s\n' 100; else printf '%s\n' 106; fi
SH
cat > "$mock_bin/no-sleep" <<'SH'
#!/bin/sh
exit 0
SH
chmod 0755 "$mock_bin/curl-success" "$mock_bin/curl-fail" \
  "$mock_bin/date-deadline" "$mock_bin/no-sleep"
HARBOR_MODEL_HEALTH_CURL="$mock_bin/curl-success" \
  HARBOR_MODEL_HEALTH_SLEEP="$mock_bin/no-sleep" \
  HARBOR_TEST_HEALTH_COUNT="$health_count" \
  /usr/lib/harboros-model-runtime/wait-health http://127.0.0.1:8080/health 30
test "$(cat "$health_count")" = 3
if HARBOR_MODEL_HEALTH_CURL="$mock_bin/curl-fail" \
  HARBOR_MODEL_HEALTH_DATE="$mock_bin/date-deadline" \
  HARBOR_MODEL_HEALTH_SLEEP="$mock_bin/no-sleep" \
  HARBOR_TEST_DATE_COUNT="$work_root/date.count" \
  /usr/lib/harboros-model-runtime/wait-health http://127.0.0.1:8080/health 5; then
  echo "error: health helper ignored its deadline" >&2
  exit 1
fi

PATH="$mock_bin:$PATH" HARBOR_TEST_SYSTEMCTL_LOG="$systemctl_log" \
  "$work_root/control/prerm" remove
test "$(readlink /data/models/current)" = "releases/$package_version"
test -f "$release_root/$first_path"
echo "model runtime transactional lifecycle passed"
