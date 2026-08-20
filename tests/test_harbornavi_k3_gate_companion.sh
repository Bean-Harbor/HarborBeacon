#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
validator="$repo_root/scripts/validate_harbornavi_k3_gate_deb.sh"
work_root="$(mktemp -d "${TMPDIR:-/tmp}/harbornavi-k3-gate-contract.XXXXXX")"

cleanup() {
  rm -rf -- "$work_root"
}
trap cleanup EXIT

command -v dpkg-deb >/dev/null || {
  echo "error: dpkg-deb is required" >&2
  exit 2
}
command -v tar >/dev/null || {
  echo "error: GNU tar is required" >&2
  exit 2
}
command -v ar >/dev/null || {
  echo "error: ar is required" >&2
  exit 2
}

make_gate_deb() {
  local label="$1"
  local package_name="$2"
  local architecture="$3"
  local provides="$4"
  local variant="$5"
  local package_root="$work_root/${label}-root"
  local gate_binary="$package_root/usr/bin/harboros-im-gate"
  local credential_helper="$package_root/usr/lib/harboros-im-gate/ensure-harborbeacon-token-env"
  local main_unit="$package_root/etc/systemd/system/harboros-im-gate.service"
  local recovery_unit="$package_root/etc/systemd/system/harboros-service-auth-recovery.service"
  built_deb="$work_root/${label}.deb"

  mkdir -p "$package_root/DEBIAN"
  mkdir -p "$package_root/usr/bin"
  mkdir -p "$package_root/usr/lib/harboros-im-gate"
  mkdir -p "$package_root/etc/systemd/system"

  {
    printf 'Package: %s\n' "$package_name"
    printf 'Version: 1.0.0\n'
    printf 'Section: utils\n'
    printf 'Priority: optional\n'
    printf 'Architecture: %s\n' "$architecture"
    if [[ -n "$provides" ]]; then
      printf 'Provides: %s\n' "$provides"
    fi
    printf 'Maintainer: Harbor Test\n'
    printf 'Description: focused K3 Gate companion fixture\n'
  } > "$package_root/DEBIAN/control"

  printf '#!/usr/bin/env bash\nexit 0\n' > "$gate_binary"
  cat > "$credential_helper" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
[[ "${1:-}" == "recover" ]]
EOF

  cat > "$main_unit" <<'EOF'
[Unit]
Description=HarborOS IM Gateway Service
After=network.target
Requires=harboros-service-auth-recovery.service
After=harboros-service-auth-recovery.service

[Service]
Type=simple
ExecStart=/usr/bin/harboros-im-gate
EOF

  cat > "$recovery_unit" <<'EOF'
[Unit]
Description=Recover Harbor service credentials

[Service]
Type=oneshot
ExecStart=/usr/lib/harboros-im-gate/ensure-harborbeacon-token-env recover
EOF

  chmod 0755 "$gate_binary" "$credential_helper"
  chmod 0644 "$main_unit" "$recovery_unit"

  case "$variant" in
    valid)
      ;;
    missing-main-unit)
      rm -- "$main_unit"
      ;;
    missing-main-requires)
      sed -i '/^Requires=harboros-service-auth-recovery\.service$/d' "$main_unit"
      ;;
    missing-main-after)
      sed -i '/^After=harboros-service-auth-recovery\.service$/d' "$main_unit"
      ;;
    missing-recovery-unit)
      rm -- "$recovery_unit"
      ;;
    wrong-recovery-type)
      sed -i 's/^Type=oneshot$/Type=simple/' "$recovery_unit"
      ;;
    wrong-exec-start)
      sed -i 's#^ExecStart=/usr/lib/harboros-im-gate/ensure-harborbeacon-token-env recover$#ExecStart=/bin/true#' "$recovery_unit"
      ;;
    duplicate-exec-start)
      printf '%s\n' 'ExecStart=/usr/lib/harboros-im-gate/ensure-harborbeacon-token-env recover' >> "$recovery_unit"
      ;;
    unsafe-executable-mode)
      chmod 6755 "$credential_helper"
      ;;
    unsafe-unit-mode)
      chmod 0666 "$main_unit"
      ;;
    remain-after-exit-no)
      printf '%s\n' 'RemainAfterExit=no' >> "$recovery_unit"
      ;;
    remain-after-exit-yes)
      printf '%s\n' 'RemainAfterExit=yes' >> "$recovery_unit"
      ;;
    non-root-owner)
      ;;
    *)
      echo "error: unknown fixture variant ${variant}" >&2
      exit 2
      ;;
  esac

  if [[ "$variant" == "non-root-owner" ]]; then
    local archive_root="$work_root/${label}-archive"
    mkdir -p "$archive_root"
    printf '2.0\n' > "$archive_root/debian-binary"
    tar \
      --sort=name \
      --owner=0 \
      --group=0 \
      --numeric-owner \
      -C "$package_root/DEBIAN" \
      -czf "$archive_root/control.tar.gz" \
      .
    tar \
      --sort=name \
      --owner=123 \
      --group=456 \
      --numeric-owner \
      --exclude='./DEBIAN' \
      --exclude='./DEBIAN/*' \
      -C "$package_root" \
      -czf "$archive_root/data.tar.gz" \
      .
    (
      cd "$archive_root"
      ar rc "$built_deb" debian-binary control.tar.gz data.tar.gz
    )
  else
    dpkg-deb --root-owner-group --build "$package_root" "$built_deb" >/dev/null
  fi
}

expect_failure() {
  local label="$1"
  shift
  if "$@" >"$work_root/${label}.stdout" 2>"$work_root/${label}.stderr"; then
    echo "error: ${label} unexpectedly passed" >&2
    exit 1
  fi
}

make_gate_deb \
  valid \
  harboros-im-gate \
  riscv64 \
  'harboros-service-auth-abi (= 1)' \
  valid
valid_deb="$built_deb"
bash "$validator" "$valid_deb" "$work_root/valid-extract" >/dev/null

make_gate_deb \
  remain-after-exit-no \
  harboros-im-gate \
  riscv64 \
  'harboros-service-auth-abi (= 1)' \
  remain-after-exit-no
bash "$validator" "$built_deb" "$work_root/remain-after-exit-no-extract" >/dev/null

ln -s "$valid_deb" "$work_root/valid-symlink.deb"
expect_failure symlink \
  bash "$validator" "$work_root/valid-symlink.deb" "$work_root/symlink-extract"

make_gate_deb \
  wrong-package \
  unrelated-package \
  riscv64 \
  'harboros-service-auth-abi (= 1)' \
  valid
expect_failure wrong-package \
  bash "$validator" "$built_deb" "$work_root/wrong-package-extract"

make_gate_deb \
  wrong-arch \
  harboros-im-gate \
  amd64 \
  'harboros-service-auth-abi (= 1)' \
  valid
expect_failure wrong-arch \
  bash "$validator" "$built_deb" "$work_root/wrong-arch-extract"

make_gate_deb \
  missing-abi \
  harboros-im-gate \
  riscv64 \
  '' \
  valid
expect_failure missing-abi \
  bash "$validator" "$built_deb" "$work_root/missing-abi-extract"

make_gate_deb \
  missing-recovery-unit \
  harboros-im-gate \
  riscv64 \
  'harboros-service-auth-abi (= 1)' \
  missing-recovery-unit
expect_failure missing-recovery-unit \
  bash "$validator" "$built_deb" "$work_root/missing-recovery-unit-extract"

make_gate_deb \
  wrong-exec-start \
  harboros-im-gate \
  riscv64 \
  'harboros-service-auth-abi (= 1)' \
  wrong-exec-start
expect_failure wrong-exec-start \
  bash "$validator" "$built_deb" "$work_root/wrong-exec-start-extract"

make_gate_deb \
  non-root-owner \
  harboros-im-gate \
  riscv64 \
  'harboros-service-auth-abi (= 1)' \
  non-root-owner
if ! dpkg-deb --fsys-tarfile "$built_deb" |
  LC_ALL=C tar --numeric-owner --list --verbose --file=- |
  awk '
    {
      member_path = $NF
      sub(/^\.\//, "", member_path)
      if ($2 == "123/456" && member_path == "usr/bin/harboros-im-gate") { found = 1 }
    }
    END { exit !(found == 1) }
  '; then
  echo "error: non-root-owner fixture does not contain numeric uid/gid 123/456" >&2
  exit 1
fi
expect_failure non-root-owner \
  bash "$validator" "$built_deb" "$work_root/non-root-owner-extract"

for variant in \
  missing-main-unit \
  missing-main-requires \
  missing-main-after \
  wrong-recovery-type \
  duplicate-exec-start \
  unsafe-executable-mode \
  unsafe-unit-mode \
  remain-after-exit-yes; do
  make_gate_deb \
    "$variant" \
    harboros-im-gate \
    riscv64 \
    'harboros-service-auth-abi (= 1)' \
    "$variant"
  expect_failure "$variant" \
    bash "$validator" "$built_deb" "$work_root/${variant}-extract"
done

echo "HarborNavi K3 Gate companion contract tests passed"
