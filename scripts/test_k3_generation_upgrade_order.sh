#!/usr/bin/env bash
set -euo pipefail

[[ "$(id -u)" == 0 ]] || { echo "error: generation test requires root" >&2; exit 2; }
[[ -f /.dockerenv && "${HARBOR_GENERATION_LIFECYCLE_DISPOSABLE:-}" == 1 ]] || {
  echo "error: generation test is restricted to an explicitly disposable container" >&2
  exit 2
}
[[ ! -e /run/harboros-k3-generation \
  && ! -e /usr/lib/harborbeacon \
  && ! -e /usr/lib/harboros-model-runtime \
  && ! -e /usr/lib/harboros-cat-vision-runtime \
  && ! -e /data/models \
  && ! -e /data/vision-models ]] || {
  echo "error: disposable container has pre-existing Harbor generation state" >&2
  exit 2
}

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work_root="$(mktemp -d /tmp/harbor-generation-order.XXXXXX)"
cleanup() {
  rm -rf -- \
    "$work_root" \
    /run/harboros-k3-generation \
    /usr/lib/harborbeacon \
    /usr/lib/harboros-model-runtime \
    /usr/lib/harboros-cat-vision-runtime \
    /usr/share/harboros-model-runtime \
    /usr/share/harboros-cat-vision-runtime \
    /usr/share/doc/harboros-cat-vision-runtime \
    /data/models \
    /data/vision-models
  rmdir /run/systemd/system 2>/dev/null || true
}
trap cleanup EXIT
mock_bin="$work_root/mock-bin"
events="$work_root/events.log"
service_state="$work_root/beacon.active"
install -d "$mock_bin" /run/systemd/system /usr/lib/harborbeacon
printf '%s\n' 1 > "$service_state"
: > "$events"

cat > "$mock_bin/systemctl" <<'SH'
#!/bin/sh
set -eu
printf 'systemctl %s\n' "$*" >> "$HARBOR_TEST_EVENTS"
case "${1:-}" in
  is-active) [ "$(cat "$HARBOR_TEST_BEACON_STATE")" = 1 ] ;;
  stop) printf '%s\n' 0 > "$HARBOR_TEST_BEACON_STATE" ;;
  restart) printf '%s\n' 1 > "$HARBOR_TEST_BEACON_STATE" ;;
esac
exit 0
SH
cat > "$mock_bin/dpkg-query" <<'SH'
#!/bin/sh
set -eu
format=""
for argument in "$@"; do
  case "$argument" in
    -f=*) format="${argument#-f=}" ;;
    *) package="$argument" ;;
  esac
done
case "$package" in
  harboros-beacon)
    if [ "$format" = '${Version}' ]; then
      printf '%s\n' "$HARBOR_TEST_GENERATION"
    else
      printf 'install ok installed|%s\n' "$HARBOR_TEST_GENERATION"
    fi
    ;;
  harboros-model-runtime)
    if [ "$format" = '${Status} ${Version}' ]; then
      printf 'install ok installed %s\n' "$HARBOR_TEST_GENERATION"
    else
      printf 'install ok installed|%s\n' "$HARBOR_TEST_GENERATION"
    fi
    ;;
  harboros-cat-vision-runtime)
    version="${HARBOR_TEST_VISION_GENERATION:-$HARBOR_TEST_GENERATION}"
    if [ "$format" = '${Status} ${Version}' ]; then
      printf 'install ok installed %s\n' "$version"
    else
      printf 'install ok installed|%s\n' "$version"
    fi
    ;;
  *) exit 1 ;;
esac
SH
cat > "$mock_bin/dpkg" <<'SH'
#!/bin/sh
set -eu
[ "$*" = --print-architecture ]
printf '%s\n' riscv64
SH
cat > "$mock_bin/deb-systemd-helper" <<'SH'
#!/bin/sh
exit 0
SH
cat > /usr/lib/harborbeacon/ensure-data-layout <<'SH'
#!/bin/sh
set -eu
[ "$(cat "$HARBOR_TEST_BEACON_STATE")" = 0 ] || {
  echo "error: Beacon was active during data-layout preparation" >&2
  exit 1
}
printf '%s\n' ensure-data-layout >> "$HARBOR_TEST_EVENTS"
SH
cat > /usr/lib/harborbeacon/migrate-cat-activity-state <<'SH'
#!/bin/sh
set -eu
[ "$(cat "$HARBOR_TEST_BEACON_STATE")" = 0 ] || {
  echo "error: Beacon was active during cat-activity migration" >&2
  exit 1
}
printf '%s\n' migrate-cat-activity-state >> "$HARBOR_TEST_EVENTS"
[ "${HARBOR_TEST_MIGRATE_FAIL:-0}" != 1 ] || exit 42
SH
chmod 0755 \
  "$mock_bin/systemctl" \
  "$mock_bin/dpkg-query" \
  "$mock_bin/dpkg" \
  "$mock_bin/deb-systemd-helper" \
  /usr/lib/harborbeacon/ensure-data-layout \
  /usr/lib/harborbeacon/migrate-cat-activity-state

export HARBOR_TEST_BEACON_STATE="$service_state"
export HARBOR_TEST_EVENTS="$events"
export HARBOR_TEST_GENERATION="0.1.0~evt.1+fixture"

# An active Beacon is stopped by prerm and restarted only after exact runtimes.
PATH="$mock_bin:$PATH" "$repo_root/debian/prerm" upgrade
test "$(cat "$service_state")" = 0
state=/run/harboros-k3-generation/beacon-was-active
test "$(stat -c '%U:%G:%a:%s' "$state")" = root:root:600:24
test "$(cat "$state")" = harboros-beacon.service
PATH="$mock_bin:$PATH" "$repo_root/debian/postinst" configure
test "$(cat "$service_state")" = 1
test ! -e "$state"
grep -Fx -- 'migrate-cat-activity-state' "$events" >/dev/null
stop_line="$(grep -n -m1 '^systemctl stop harboros-beacon.service$' "$events" | cut -d: -f1)"
restart_line="$(grep -n -m1 '^systemctl restart harboros-beacon.service$' "$events" | cut -d: -f1)"
migrate_line="$(grep -n -m1 '^migrate-cat-activity-state$' "$events" | cut -d: -f1)"
test "$stop_line" -lt "$migrate_line"
test "$migrate_line" -lt "$restart_line"

# A previously inactive Beacon remains inactive and is not spuriously restarted.
printf '%s\n' 0 > "$service_state"
restart_count="$(grep -Fc 'systemctl restart harboros-beacon.service' "$events")"
PATH="$mock_bin:$PATH" "$repo_root/debian/prerm" upgrade
test ! -e "$state"
PATH="$mock_bin:$PATH" "$repo_root/debian/postinst" configure
test "$(cat "$service_state")" = 0
test "$(grep -Fc 'systemctl restart harboros-beacon.service' "$events")" = "$restart_count"

# A mixed runtime generation cannot consume the active marker or restart Beacon.
printf '%s\n' 1 > "$service_state"
PATH="$mock_bin:$PATH" "$repo_root/debian/prerm" upgrade
export HARBOR_TEST_VISION_GENERATION="0.1.0~evt.1+wrong"
if PATH="$mock_bin:$PATH" "$repo_root/debian/postinst" configure; then
  echo "error: Beacon postinst accepted a mixed runtime generation" >&2
  exit 1
fi
test "$(cat "$service_state")" = 0
test -f "$state"
test "$(grep -Fc 'systemctl restart harboros-beacon.service' "$events")" = "$restart_count"

# Restoring the exact three-package generation completes rollback and consumes state.
unset HARBOR_TEST_VISION_GENERATION
PATH="$mock_bin:$PATH" "$repo_root/debian/postinst" configure
test "$(cat "$service_state")" = 1
test ! -e "$state"
test "$(grep -Fc 'systemctl restart harboros-beacon.service' "$events")" = \
  "$((restart_count + 1))"

# A migration failure is also fail closed and can be retried without losing intent.
printf '%s\n' 1 > "$service_state"
PATH="$mock_bin:$PATH" "$repo_root/debian/prerm" upgrade
restart_count="$(grep -Fc 'systemctl restart harboros-beacon.service' "$events")"
if HARBOR_TEST_MIGRATE_FAIL=1 PATH="$mock_bin:$PATH" \
  "$repo_root/debian/postinst" configure; then
  echo "error: Beacon postinst ignored a cat-activity migration failure" >&2
  exit 1
fi
test "$(cat "$service_state")" = 0
test -f "$state"
test "$(grep -Fc 'systemctl restart harboros-beacon.service' "$events")" = "$restart_count"
PATH="$mock_bin:$PATH" "$repo_root/debian/postinst" configure
test "$(cat "$service_state")" = 1
test ! -e "$state"
test "$(grep -Fc 'systemctl restart harboros-beacon.service' "$events")" = \
  "$((restart_count + 1))"

# A boot has no /run active-intent marker. The package-owned ExecStartPre still
# rejects a mixed generation, pointer drift, or failed byte verifier.
generation="$HARBOR_TEST_GENERATION"
install -d \
  /usr/lib/harboros-model-runtime \
  /usr/lib/harboros-cat-vision-runtime \
  /usr/share/harboros-model-runtime \
  /usr/share/harboros-cat-vision-runtime \
  /usr/share/doc/harboros-cat-vision-runtime \
  "/data/models/releases/$generation" \
  "/data/vision-models/releases/$generation"
cp "$repo_root/debian/verify-beacon-k3-generation" \
  /usr/lib/harborbeacon/verify-k3-generation
printf '{}\n' > /usr/share/harboros-model-runtime/model-materials.json
printf '{}\n' > /usr/share/harboros-cat-vision-runtime/cat-vision-materials.json
printf '{}\n' > /usr/share/doc/harboros-cat-vision-runtime/vision-runtime-evidence.json
cat > /usr/lib/harboros-model-runtime/verify-release <<'SH'
#!/bin/sh
[ "${HARBOR_TEST_MODEL_VERIFY_FAIL:-0}" != 1 ]
SH
cat > /usr/lib/harboros-cat-vision-runtime/verify-release <<'SH'
#!/bin/sh
[ "${HARBOR_TEST_VISION_VERIFY_FAIL:-0}" != 1 ]
SH
cat > /usr/lib/harboros-cat-vision-runtime/verify-evidence <<'SH'
#!/bin/sh
[ "${HARBOR_TEST_EVIDENCE_VERIFY_FAIL:-0}" != 1 ]
SH
chmod 0755 \
  /usr/lib/harborbeacon/verify-k3-generation \
  /usr/lib/harboros-model-runtime/verify-release \
  /usr/lib/harboros-cat-vision-runtime/verify-release \
  /usr/lib/harboros-cat-vision-runtime/verify-evidence
ln -s "releases/$generation" /data/models/current
ln -s "releases/$generation" /data/vision-models/current
rm -rf -- /run/harboros-k3-generation
PATH="$mock_bin:$PATH" /usr/lib/harborbeacon/verify-k3-generation

export HARBOR_TEST_VISION_GENERATION="0.1.0~evt.1+interrupted"
if PATH="$mock_bin:$PATH" /usr/lib/harborbeacon/verify-k3-generation; then
  echo "error: reboot verifier accepted a mixed package generation" >&2
  exit 1
fi
unset HARBOR_TEST_VISION_GENERATION

rm -f -- /data/vision-models/current
ln -s "releases/0.1.0~evt.1+interrupted" /data/vision-models/current
if PATH="$mock_bin:$PATH" /usr/lib/harborbeacon/verify-k3-generation; then
  echo "error: reboot verifier accepted a mismatched vision pointer" >&2
  exit 1
fi
rm -f -- /data/vision-models/current
ln -s "releases/$generation" /data/vision-models/current

if HARBOR_TEST_MODEL_VERIFY_FAIL=1 PATH="$mock_bin:$PATH" \
  /usr/lib/harborbeacon/verify-k3-generation; then
  echo "error: reboot verifier ignored a failed model byte verification" >&2
  exit 1
fi
PATH="$mock_bin:$PATH" /usr/lib/harborbeacon/verify-k3-generation

echo "K3 generation upgrade order passed"
