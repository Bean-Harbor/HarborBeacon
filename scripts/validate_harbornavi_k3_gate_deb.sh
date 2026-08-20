#!/usr/bin/env bash
set -euo pipefail

gate_deb="${1:-}"
extract_root="${2:-}"
service_auth_abi=1

fail() {
  echo "error: $*" >&2
  exit 2
}

if [[ -z "$gate_deb" || -z "$extract_root" ]]; then
  fail "usage: $0 <harboros-im-gate.deb> <empty-extract-directory>"
fi

if [[ -L "$gate_deb" || ! -f "$gate_deb" ]]; then
  fail "Gate companion package must be a regular, non-symlink file: ${gate_deb}"
fi

if [[ -e "$extract_root" || -L "$extract_root" ]]; then
  fail "Gate companion extraction path must not already exist: ${extract_root}"
fi

command -v dpkg-deb >/dev/null || fail "dpkg-deb is required"
command -v bash >/dev/null || fail "bash is required"
command -v tar >/dev/null || fail "GNU tar is required"
command -v awk >/dev/null || fail "awk is required"
command -v stat >/dev/null || fail "GNU stat is required"
tar_version="$(tar --version 2>/dev/null || true)"
[[ "$tar_version" == *"GNU tar"* ]] || fail "GNU tar is required"

read_control_field() {
  local field="$1"
  local value
  if ! value="$(dpkg-deb -f "$gate_deb" "$field" 2>/dev/null)"; then
    fail "Gate companion package is missing control field ${field}"
  fi
  printf '%s' "$value"
}

package_name="$(read_control_field Package)"
package_version="$(read_control_field Version)"
package_arch="$(read_control_field Architecture)"
package_provides="$(read_control_field Provides)"

[[ "$package_name" == "harboros-im-gate" ]] ||
  fail "Gate companion Package must be harboros-im-gate, got ${package_name}"
[[ -n "$package_version" ]] || fail "Gate companion Version must not be empty"
[[ "$package_arch" == "riscv64" ]] ||
  fail "Gate companion Architecture must be riscv64, got ${package_arch}"

abi_pattern="(^|,)[[:space:]]*harboros-service-auth-abi[[:space:]]*\\(=[[:space:]]*${service_auth_abi}\\)[[:space:]]*(,|$)"
if ! [[ "$package_provides" =~ $abi_pattern ]]; then
  fail "Gate companion must Provide harboros-service-auth-abi (= ${service_auth_abi})"
fi

if ! archive_listing="$(
  dpkg-deb --fsys-tarfile "$gate_deb" |
    LC_ALL=C tar --numeric-owner --list --verbose --file=-
)"; then
  fail "Gate companion data archive could not be inspected"
fi

require_archive_root_owner() {
  local archive_path="$1"
  local archive_record
  local archive_mode
  local archive_owner
  if ! archive_record="$(
    printf '%s\n' "$archive_listing" |
      awk -v expected_path="$archive_path" '
        {
          member_path = $NF
          sub(/^\.\//, "", member_path)
          if (member_path == expected_path) {
            count += 1
            member_mode = $1
            member_owner = $2
          }
        }
        END {
          if (count != 1) { exit 1 }
          printf "%s %s\n", member_mode, member_owner
        }
      '
  )"; then
    fail "Gate companion data archive must contain exactly one regular ${archive_path} member"
  fi
  read -r archive_mode archive_owner <<<"$archive_record"
  [[ "$archive_mode" == -* ]] ||
    fail "Gate companion archive member must be a regular file: ${archive_path}"
  [[ "$archive_owner" == "0/0" ]] ||
    fail "Gate companion archive member must have uid=0 gid=0: ${archive_path}"
}

require_archive_root_owner 'usr/bin/harboros-im-gate'
require_archive_root_owner 'usr/lib/harboros-im-gate/ensure-harborbeacon-token-env'
require_archive_root_owner 'etc/systemd/system/harboros-im-gate.service'
require_archive_root_owner 'etc/systemd/system/harboros-service-auth-recovery.service'

mkdir -p -- "$extract_root"
chmod 0700 "$extract_root"
dpkg-deb -x "$gate_deb" "$extract_root"

gate_binary="$extract_root/usr/bin/harboros-im-gate"
main_unit="$extract_root/etc/systemd/system/harboros-im-gate.service"
recovery_unit="$extract_root/etc/systemd/system/harboros-service-auth-recovery.service"
credential_helper="$extract_root/usr/lib/harboros-im-gate/ensure-harborbeacon-token-env"

for artifact in "$gate_binary" "$credential_helper" "$main_unit" "$recovery_unit"; do
  if [[ -L "$artifact" || ! -f "$artifact" ]]; then
    fail "Gate companion artifact must be a regular, non-symlink file: ${artifact#"$extract_root"}"
  fi
done

require_safe_executable_mode() {
  local artifact="$1"
  local label="$2"
  local raw_mode
  local mode_value
  raw_mode="$(stat -c '%a' "$artifact")"
  [[ "$raw_mode" =~ ^[0-7]{3,4}$ ]] || fail "${label} has an unreadable file mode"
  mode_value=$((8#$raw_mode))
  if (( (mode_value & 06022) != 0 || (mode_value & 0500) != 0500 )); then
    fail "${label} must be owner-readable/executable without setuid, setgid, group-write, or world-write bits"
  fi
}

require_safe_unit_mode() {
  local artifact="$1"
  local label="$2"
  local raw_mode
  local mode_value
  raw_mode="$(stat -c '%a' "$artifact")"
  [[ "$raw_mode" =~ ^[0-7]{3,4}$ ]] || fail "${label} has an unreadable file mode"
  mode_value=$((8#$raw_mode))
  if (( (mode_value & 06022) != 0 || (mode_value & 0111) != 0 || (mode_value & 0400) == 0 )); then
    fail "${label} must be owner-readable/non-executable without setuid, setgid, group-write, or world-write bits"
  fi
}

require_effective_unit_dependency() {
  local unit="$1"
  local directive="$2"
  local target_unit="$3"
  if ! awk -v directive="$directive" -v target_unit="$target_unit" '
    {
      line = $0
      sub(/^[[:space:]]+/, "", line)
      sub(/[[:space:]]+$/, "", line)
    }
    line ~ /^\[/ { section = line }
    section == "[Unit]" && line ~ ("^" directive "[[:space:]]*=") {
      value = line
      sub("^" directive "[[:space:]]*=[[:space:]]*", "", value)
      if (value == target_unit) { active = 1; exact = 1 }
      if (value == "") { active = 0 }
    }
    END { exit !(exact == 1 && active == 1) }
  ' "$unit"; then
    fail "Gate main unit must effectively contain ${directive}=${target_unit}"
  fi
}

require_single_service_assignment() {
  local unit="$1"
  local directive="$2"
  local expected_value="$3"
  if ! awk -v directive="$directive" -v expected_value="$expected_value" '
    {
      line = $0
      sub(/^[[:space:]]+/, "", line)
      sub(/[[:space:]]+$/, "", line)
    }
    line ~ /^\[/ { section = line }
    section == "[Service]" && line ~ ("^" directive "[[:space:]]*=") {
      value = line
      sub("^" directive "[[:space:]]*=[[:space:]]*", "", value)
      count += 1
      valid = (value == expected_value)
    }
    END { exit !(count == 1 && valid == 1) }
  ' "$unit"; then
    fail "Gate recovery unit must contain exactly one ${directive}=${expected_value} assignment"
  fi
}

require_safe_remain_after_exit() {
  local unit="$1"
  if ! awk '
    {
      line = $0
      sub(/^[[:space:]]+/, "", line)
      sub(/[[:space:]]+$/, "", line)
    }
    line ~ /^\[/ { section = line }
    section == "[Service]" && line ~ /^RemainAfterExit[[:space:]]*=/ {
      value = line
      sub(/^RemainAfterExit[[:space:]]*=[[:space:]]*/, "", value)
      configured = 1
      effective_value = value
    }
    END { exit !(configured != 1 || effective_value == "no") }
  ' "$unit"; then
    fail "Gate recovery unit RemainAfterExit must be absent or effectively set to no"
  fi
}

require_safe_executable_mode "$gate_binary" "Gate companion binary"
require_safe_executable_mode "$credential_helper" "Gate credential helper"
require_safe_unit_mode "$main_unit" "Gate main unit"
require_safe_unit_mode "$recovery_unit" "Gate recovery unit"
bash -n "$credential_helper"

recovery_unit_name='harboros-service-auth-recovery.service'
require_effective_unit_dependency "$main_unit" Requires "$recovery_unit_name"
require_effective_unit_dependency "$main_unit" After "$recovery_unit_name"
require_single_service_assignment "$recovery_unit" Type oneshot
require_safe_remain_after_exit "$recovery_unit"
require_single_service_assignment \
  "$recovery_unit" \
  ExecStart \
  '/usr/lib/harboros-im-gate/ensure-harborbeacon-token-env recover'

printf 'gate_companion_package=%s\n' "$package_name"
printf 'gate_companion_version=%s\n' "$package_version"
printf 'gate_companion_arch=%s\n' "$package_arch"
printf 'gate_companion_service_auth_abi=%s\n' "$service_auth_abi"
