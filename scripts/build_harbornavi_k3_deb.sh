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
package_work_parent="${PACKAGE_WORK_ROOT:-${TMPDIR:-/tmp}}"
if [[ "$package_work_parent" =~ ^/mnt/[[:alpha:]](/|$) ]]; then
  package_work_parent="/tmp"
fi
mkdir -p "$package_work_parent"
build_root="$(mktemp -d "${package_work_parent%/}/harbornavi-k3-deb.XXXXXX")"
pkg_name="harboros-beacon_${release_label}_${deb_arch}"
pkg_dir="${build_root}/${pkg_name}"
cargo_target_root="${CARGO_TARGET_DIR:-${repo_root}/target}"
cargo_release_dir="${cargo_target_root}/${target}/release"

cleanup_build_root() {
  rm -rf -- "$build_root"
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

export CARGO_TARGET_RISCV64GC_UNKNOWN_LINUX_GNU_LINKER="${CARGO_TARGET_RISCV64GC_UNKNOWN_LINUX_GNU_LINKER:-riscv64-linux-gnu-gcc}"

cargo build --release --target "$target" --bin harboros-beacon
cargo build --release --target "$target" --bin harbor-model-api
cargo build --release --target "$target" --bin harbornavi-k3-local-vision-smoke
cargo build --release --target "$target" --bin harbornavi-k3-multi-vision-smoke
cargo build --release --target "$target" --bin harbornavi-ha-mqtt-event-contract-smoke

mkdir -p "$pkg_dir/DEBIAN"
mkdir -p "$pkg_dir/usr/bin"
mkdir -p "$pkg_dir/etc/systemd/system"
mkdir -p "$pkg_dir/usr/lib/harboros-beacon"
mkdir -p "$pkg_dir/usr/share/doc/harboros-beacon"
mkdir -p "$pkg_dir/var/lib/harboros-beacon/vision-models/mobilenetv2-cat-binary-v2-20260806"
mkdir -p "$pkg_dir/var/lib/harboros-beacon/vision-models/package-roboflow-v1-320x320-fp32"
find "$build_root" -type d -exec chmod a-s,u=rwx,go=rx {} +

cp "${cargo_release_dir}/harboros-beacon" "$pkg_dir/usr/bin/harboros-beacon"
cp "${cargo_release_dir}/harbor-model-api" "$pkg_dir/usr/bin/harbor-model-api"
cp "${cargo_release_dir}/harbornavi-k3-local-vision-smoke" "$pkg_dir/usr/bin/harbornavi-k3-local-vision-smoke"
cp "${cargo_release_dir}/harbornavi-k3-multi-vision-smoke" "$pkg_dir/usr/bin/harbornavi-k3-multi-vision-smoke"
cp "${cargo_release_dir}/harbornavi-ha-mqtt-event-contract-smoke" "$pkg_dir/usr/bin/harbornavi-ha-mqtt-event-contract-smoke"
chmod 0755 "$pkg_dir/usr/bin/harboros-beacon" "$pkg_dir/usr/bin/harbor-model-api" "$pkg_dir/usr/bin/harbornavi-k3-local-vision-smoke" "$pkg_dir/usr/bin/harbornavi-k3-multi-vision-smoke" "$pkg_dir/usr/bin/harbornavi-ha-mqtt-event-contract-smoke"
sed 's/\r$//' scripts/harbornavi_k3_yolov8_analyzer.py > "$pkg_dir/usr/lib/harboros-beacon/harbornavi_k3_yolov8_analyzer.py"
sed 's/\r$//' scripts/harbornavi_k3_yolo_stream_worker.py > "$pkg_dir/usr/lib/harboros-beacon/harbornavi_k3_yolo_stream_worker.py"
sed 's/\r$//' scripts/harbornavi_k3_cat_recording_classifier.py > "$pkg_dir/usr/lib/harboros-beacon/harbornavi_k3_cat_recording_classifier.py"
chmod 0755 \
  "$pkg_dir/usr/lib/harboros-beacon/harbornavi_k3_yolov8_analyzer.py" \
  "$pkg_dir/usr/lib/harboros-beacon/harbornavi_k3_yolo_stream_worker.py" \
  "$pkg_dir/usr/lib/harboros-beacon/harbornavi_k3_cat_recording_classifier.py"
cp config/harbornavi-k3/vision-models/mobilenetv2-cat-binary-v2-20260806/mobilenetv2_cat_binary_int8.onnx \
  "$pkg_dir/var/lib/harboros-beacon/vision-models/mobilenetv2-cat-binary-v2-20260806/mobilenetv2_cat_binary_int8.onnx"
sed 's/\r$//' config/harbornavi-k3/vision-models/mobilenetv2-cat-binary-v2-20260806/runtime-contract.json \
  > "$pkg_dir/var/lib/harboros-beacon/vision-models/mobilenetv2-cat-binary-v2-20260806/runtime-contract.json"
chmod 0644 \
  "$pkg_dir/var/lib/harboros-beacon/vision-models/mobilenetv2-cat-binary-v2-20260806/mobilenetv2_cat_binary_int8.onnx" \
  "$pkg_dir/var/lib/harboros-beacon/vision-models/mobilenetv2-cat-binary-v2-20260806/runtime-contract.json"
cp config/harbornavi-k3/vision-models/package-roboflow-v1-320x320-fp32/yolov8n-package-roboflow-v1-320x320.onnx \
  "$pkg_dir/var/lib/harboros-beacon/vision-models/package-roboflow-v1-320x320-fp32/yolov8n-package-roboflow-v1-320x320.onnx"
sed 's/\r$//' config/harbornavi-k3/vision-models/package-roboflow-v1-320x320-fp32/label.txt \
  > "$pkg_dir/var/lib/harboros-beacon/vision-models/package-roboflow-v1-320x320-fp32/label.txt"
sed 's/\r$//' config/harbornavi-k3/vision-models/package-roboflow-v1-320x320-fp32/runtime-contract.json \
  > "$pkg_dir/var/lib/harboros-beacon/vision-models/package-roboflow-v1-320x320-fp32/runtime-contract.json"
chmod 0644 \
  "$pkg_dir/var/lib/harboros-beacon/vision-models/package-roboflow-v1-320x320-fp32/yolov8n-package-roboflow-v1-320x320.onnx" \
  "$pkg_dir/var/lib/harboros-beacon/vision-models/package-roboflow-v1-320x320-fp32/label.txt" \
  "$pkg_dir/var/lib/harboros-beacon/vision-models/package-roboflow-v1-320x320-fp32/runtime-contract.json"

sed 's/\r$//' debian/harboros-beacon.service > "$pkg_dir/etc/systemd/system/harboros-beacon.service"
sed 's/\r$//' debian/semantic-router.service > "$pkg_dir/etc/systemd/system/semantic-router.service"
chmod 0644 \
  "$pkg_dir/etc/systemd/system/harboros-beacon.service" \
  "$pkg_dir/etc/systemd/system/semantic-router.service"

sed \
  -e "s/VERSION_PLACEHOLDER/${debian_version}/g" \
  -e "s/ARCH_PLACEHOLDER/${deb_arch}/g" \
  debian/control \
  | sed 's/^Depends: .*/Depends: libc6, openssl, ca-certificates, harborlink (>= 0.1.0), python3, python3-numpy, python3-pil, python3-opencv, python3-spacemit-ort/' \
  > "$pkg_dir/DEBIAN/control"
printf 'X-HarborNavi-Version: %s\n' "$release_label" >> "$pkg_dir/DEBIAN/control"

sed 's/\r$//' debian/postinst > "$pkg_dir/DEBIAN/postinst"
sed 's/\r$//' debian/prerm > "$pkg_dir/DEBIAN/prerm"
chmod 0755 "$pkg_dir/DEBIAN/postinst" "$pkg_dir/DEBIAN/prerm"

cat > "$pkg_dir/usr/share/doc/harboros-beacon/harbornavi-k3-package.txt" <<EOF
HarborNavi K3 local vision event package
release_label=${release_label}
debian_version=${debian_version}
rust_target=${target}
deb_arch=${deb_arch}
analyzer=/usr/lib/harboros-beacon/harbornavi_k3_yolov8_analyzer.py
stream_worker=/usr/lib/harboros-beacon/harbornavi_k3_yolo_stream_worker.py
cat_recording_validator=mobilenet_v2_int8
cat_recording_classifier=/usr/lib/harboros-beacon/harbornavi_k3_cat_recording_classifier.py
cat_recording_classifier_model=/var/lib/harboros-beacon/vision-models/mobilenetv2-cat-binary-v2-20260806/mobilenetv2_cat_binary_int8.onnx
cat_recording_classifier_sha256=d0c1bdcf973ca7f6efc6e62af764ff59300e0d27abbc75c20c7f86515769d825
cat_recording_classifier_policy=up_to_9_frames_at_least_3_positive
single_runner=/usr/bin/harbornavi-k3-local-vision-smoke
multi_runner=/usr/bin/harbornavi-k3-multi-vision-smoke
ha_mqtt_runner=/usr/bin/harbornavi-ha-mqtt-event-contract-smoke
semantic_router_service=/etc/systemd/system/semantic-router.service
semantic_router_binary=/usr/bin/harbor-model-api
semantic_router_healthz=http://127.0.0.1:4176/healthz
default_model=/var/lib/harboros-beacon/models/yolov8n_192x320.q.onnx
default_labels=/var/lib/harboros-beacon/models/label.txt
package_yolo_model=/var/lib/harboros-beacon/vision-models/package-roboflow-v1-320x320-fp32/yolov8n-package-roboflow-v1-320x320.onnx
package_yolo_labels=/var/lib/harboros-beacon/vision-models/package-roboflow-v1-320x320-fp32/label.txt
package_yolo_model_sha256=c9df4e5e872f2857b3bcad1910121dee7358b1625cf32620938cb54dcc985568
capture_modes=oneshot_ffmpeg,persistent_ffmpeg,local_restream
fixed_rate_scheduler=enabled
default_four_channel_phase_offsets=0ms,2500ms,5000ms,7500ms
persistent_capture_root=/run/harbornavi/capture
EOF

mkdir -p "$out_dir"
find "$pkg_dir" -type d -exec chmod a-s,u=rwx,go=rx {} +
dpkg-deb --root-owner-group --build "$pkg_dir" "${out_dir}/${pkg_name}.deb"

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
