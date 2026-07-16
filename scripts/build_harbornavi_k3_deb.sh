#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

target="${RUST_TARGET:-riscv64gc-unknown-linux-gnu}"
deb_arch="${DEB_ARCH:-riscv64}"
date_stamp="${HARBORNAVI_BUILD_DATE:-$(date +%Y%m%d)}"
release_label="${RELEASE_VERSION:-harbornavi-p1-capture-opt-${date_stamp}+riscv64}"
debian_version="${DEBIAN_VERSION:-0.1.0+harbornavi.p1.captureopt.${date_stamp}.riscv64}"
out_dir="${OUT_DIR:-${repo_root}/dist/harbornavi-k3-debs}"
# Some Cargo dependencies track OUT_DIR and rebuild whenever the package output path changes.
unset OUT_DIR
pkg_name="harboros-beacon_${release_label}_${deb_arch}"
package_work_parent="${PACKAGE_WORK_ROOT:-${TMPDIR:-/tmp}}"
if [[ "$package_work_parent" =~ ^/mnt/[[:alpha:]](/|$) ]]; then
  echo "warning: PACKAGE_WORK_ROOT is on a Windows mount; using /tmp for dpkg work files" >&2
  package_work_parent="/tmp"
fi
mkdir -p "$package_work_parent"
build_root="$(mktemp -d "${package_work_parent%/}/harboros-beacon-k3-deb.XXXXXX")"
pkg_dir="${build_root}/${pkg_name}"
cargo_target_root="${CARGO_TARGET_DIR:-${repo_root}/target}"
cargo_release_dir="${cargo_target_root}/${target}/release"
mediamtx_version="${MEDIAMTX_VERSION:-1.19.2}"

cleanup_build_root() {
  rm -rf "$build_root"
}

trap cleanup_build_root EXIT

if [[ "$target" != "riscv64gc-unknown-linux-gnu" ]]; then
  echo "error: K3 package target must be riscv64gc-unknown-linux-gnu, got ${target}" >&2
  exit 2
fi

if [[ "$deb_arch" != "riscv64" ]]; then
  echo "error: K3 Debian architecture must be riscv64, got ${deb_arch}" >&2
  exit 2
fi

command -v dpkg-deb >/dev/null || {
  echo "error: dpkg-deb is required" >&2
  exit 2
}

command -v riscv64-linux-gnu-gcc >/dev/null || {
  echo "error: riscv64-linux-gnu-gcc is required" >&2
  exit 2
}

export CARGO_TARGET_RISCV64GC_UNKNOWN_LINUX_GNU_LINKER="${CARGO_TARGET_RISCV64GC_UNKNOWN_LINUX_GNU_LINKER:-\
riscv64-linux-gnu-gcc}"

cargo build --release --target "$target" \
  --bin harboros-beacon \
  --bin harbornavi-k3-local-vision-smoke \
  --bin harbornavi-k3-multi-vision-smoke \
  --bin harbornavi-ha-mqtt-event-contract-smoke

mediamtx_bin="${MEDIAMTX_BIN:-}"
if [[ -z "$mediamtx_bin" ]]; then
  command -v go >/dev/null || {
    echo "error: Go is required to cross-compile MediaMTX ${mediamtx_version}; set MEDIAMTX_BIN to a prebuilt linux/riscv64 binary" >&2
    exit 2
  }
  command -v git >/dev/null || {
    echo "error: Git is required to prepare MediaMTX ${mediamtx_version} sources" >&2
    exit 2
  }
  mediamtx_build_dir="${build_root}/mediamtx-bin"
  mediamtx_source_dir="${build_root}/mediamtx-source"
  mediamtx_source_archive="${MEDIAMTX_SOURCE_ARCHIVE:-\
${XDG_CACHE_HOME:-${HOME}/.cache}/harbor-mediamtx/mediamtx-v${mediamtx_version}.tar.gz}"
  mediamtx_hls_js="${MEDIAMTX_HLS_JS:-\
${XDG_CACHE_HOME:-${HOME}/.cache}/harbor-mediamtx/mediamtx-v${mediamtx_version}-hls.min.js}"
  mkdir -p "$mediamtx_build_dir"
  if [[ -f "$mediamtx_source_archive" ]]; then
    if [[ -f "${mediamtx_source_archive}.sha256" ]]; then
      (
        cd "$(dirname "$mediamtx_source_archive")"
        sha256sum --check --strict "$(basename "${mediamtx_source_archive}.sha256")"
      )
    fi
    mkdir -p "$mediamtx_source_dir"
    tar -xzf "$mediamtx_source_archive" -C "$mediamtx_source_dir" --strip-components=1
  else
    git clone --quiet --depth 1 --branch "v${mediamtx_version}" \
      https://github.com/bluenviron/mediamtx.git "$mediamtx_source_dir"
  fi
  (
    cd "$mediamtx_source_dir"
    printf 'v%s\n' "$mediamtx_version" > internal/core/VERSION
    if [[ -f "$mediamtx_hls_js" ]]; then
      if [[ -f "${mediamtx_hls_js}.sha256" ]]; then
        (
          cd "$(dirname "$mediamtx_hls_js")"
          sha256sum --check --strict "$(basename "${mediamtx_hls_js}.sha256")"
        )
      fi
      cp "$mediamtx_hls_js" internal/servers/hls/hls.min.js
    else
      go generate ./internal/servers/hls
    fi
    env CGO_ENABLED=0 GOARCH=riscv64 GOOS=linux \
      go build -trimpath -o "${mediamtx_build_dir}/mediamtx" .
  )
  mediamtx_bin="${mediamtx_build_dir}/mediamtx"
fi

if [[ ! -f "$mediamtx_bin" ]]; then
  echo "error: MediaMTX binary is missing: ${mediamtx_bin}" >&2
  exit 2
fi

mkdir -p "$pkg_dir/DEBIAN"
mkdir -p "$pkg_dir/usr/bin"
mkdir -p "$pkg_dir/etc/systemd/system"
mkdir -p "$pkg_dir/etc/harboros-beacon"
mkdir -p "$pkg_dir/usr/lib/harboros-beacon"
mkdir -p "$pkg_dir/usr/share/doc/harboros-beacon"
find "$build_root" -type d -exec chmod a-s,u=rwx,go=rx {} +

cp "${cargo_release_dir}/harboros-beacon" "$pkg_dir/usr/bin/harboros-beacon"
cp "${cargo_release_dir}/harbornavi-k3-local-vision-smoke" "$pkg_dir/usr/bin/harbornavi-k3-local-vision-smoke"
cp "${cargo_release_dir}/harbornavi-k3-multi-vision-smoke" "$pkg_dir/usr/bin/harbornavi-k3-multi-vision-smoke"
cp "${cargo_release_dir}/harbornavi-ha-mqtt-event-contract-smoke" \
  "$pkg_dir/usr/bin/harbornavi-ha-mqtt-event-contract-smoke"
chmod 0755 \
  "$pkg_dir/usr/bin/harboros-beacon" \
  "$pkg_dir/usr/bin/harbornavi-k3-local-vision-smoke" \
  "$pkg_dir/usr/bin/harbornavi-k3-multi-vision-smoke" \
  "$pkg_dir/usr/bin/harbornavi-ha-mqtt-event-contract-smoke"
cp scripts/harbornavi_k3_yolov8_analyzer.py "$pkg_dir/usr/lib/harboros-beacon/harbornavi_k3_yolov8_analyzer.py"
cp "$mediamtx_bin" "$pkg_dir/usr/lib/harboros-beacon/mediamtx"
chmod 0755 "$pkg_dir/usr/lib/harboros-beacon/harbornavi_k3_yolov8_analyzer.py"
chmod 0755 "$pkg_dir/usr/lib/harboros-beacon/mediamtx"

cp debian/harboros-beacon.service "$pkg_dir/etc/systemd/system/harboros-beacon.service"
cp debian/harboros-mediamtx.service "$pkg_dir/etc/systemd/system/harboros-mediamtx.service"
cp debian/mediamtx.yml "$pkg_dir/etc/harboros-beacon/mediamtx.yml"
chmod 0644 \
  "$pkg_dir/etc/systemd/system/harboros-mediamtx.service" \
  "$pkg_dir/etc/harboros-beacon/mediamtx.yml"

sed \
  -e "s/VERSION_PLACEHOLDER/${debian_version}/g" \
  -e "s/ARCH_PLACEHOLDER/${deb_arch}/g" \
  debian/control \
  | sed 's/^Depends: .*/Depends: libc6, openssl, ca-certificates, python3, python3-opencv, python3-spacemit-ort/' \
  > "$pkg_dir/DEBIAN/control"
printf 'X-HarborNavi-Version: %s\n' "$release_label" >> "$pkg_dir/DEBIAN/control"

cp debian/postinst "$pkg_dir/DEBIAN/postinst"
cp debian/prerm "$pkg_dir/DEBIAN/prerm"
chmod 0755 "$pkg_dir/DEBIAN/postinst" "$pkg_dir/DEBIAN/prerm"

cat > "$pkg_dir/usr/share/doc/harboros-beacon/harbornavi-k3-package.txt" <<EOF
HarborNavi K3 local vision event package
release_label=${release_label}
debian_version=${debian_version}
rust_target=${target}
deb_arch=${deb_arch}
analyzer=/usr/lib/harboros-beacon/harbornavi_k3_yolov8_analyzer.py
single_runner=/usr/bin/harbornavi-k3-local-vision-smoke
multi_runner=/usr/bin/harbornavi-k3-multi-vision-smoke
ha_mqtt_runner=/usr/bin/harbornavi-ha-mqtt-event-contract-smoke
mediamtx=/usr/lib/harboros-beacon/mediamtx
mediamtx_version=${mediamtx_version}
webrtc_whep=http://127.0.0.1:8889
webrtc_ice_udp_port=8189
default_model=/var/lib/harboros-beacon/models/yolov8n_192x320.q.onnx
default_labels=/var/lib/harboros-beacon/models/label.txt
capture_modes=oneshot_ffmpeg,persistent_ffmpeg,local_restream
fixed_rate_scheduler=enabled
default_four_channel_phase_offsets=0ms,2500ms,5000ms,7500ms
persistent_capture_root=/run/harbornavi/capture
EOF

mkdir -p "$out_dir"
find "$pkg_dir" -type d -exec chmod a-s,u=rwx,go=rx {} +
dpkg-deb --build "$pkg_dir" "${out_dir}/${pkg_name}.deb"

sha256sum "${out_dir}/${pkg_name}.deb" > "${out_dir}/${pkg_name}.deb.sha256"
file "${cargo_release_dir}/harboros-beacon" > "${out_dir}/${pkg_name}.file.txt"
dpkg-deb --info "${out_dir}/${pkg_name}.deb" > "${out_dir}/${pkg_name}.info.txt"
dpkg-deb --contents "${out_dir}/${pkg_name}.deb" > "${out_dir}/${pkg_name}.contents.txt"

cat <<EOF
package=${out_dir}/${pkg_name}.deb
sha256=${out_dir}/${pkg_name}.deb.sha256
info=${out_dir}/${pkg_name}.info.txt
contents=${out_dir}/${pkg_name}.contents.txt
file=${out_dir}/${pkg_name}.file.txt
release_label=${release_label}
debian_version=${debian_version}
EOF
