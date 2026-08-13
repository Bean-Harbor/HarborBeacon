#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
component="${1:-beacon}"
case "$component" in
  beacon) build_script="$repo_root/scripts/build_harbornavi_k3_deb.sh" ;;
  model-runtime) build_script="$repo_root/scripts/build_model_runtime_k3_deb.sh" ;;
  *) echo "usage: $0 beacon|model-runtime" >&2; exit 2 ;;
esac

work_root="$(mktemp -d "${TMPDIR:-/tmp}/harborbeacon-repro.XXXXXX")"
trap 'rm -rf -- "$work_root"' EXIT
for run in first second; do
  rm -rf -- "$work_root/target"
  OUT_DIR="$work_root/$run/out" CARGO_TARGET_DIR="$work_root/target" "$build_script"
done
first="$work_root/first/out"
second="$work_root/second/out"
[[ -n "$(find "$first" -maxdepth 1 -name '*.deb' -print -quit)" \
  && -n "$(find "$second" -maxdepth 1 -name '*.deb' -print -quit)" ]] || {
  echo "error: both builds must produce a release artifact set" >&2
  exit 2
}
diff --no-dereference --recursive "$first" "$second" || {
  echo "error: ${component} release artifact set is not reproducible" >&2
  exit 1
}
(
  cd "$first"
  sha256sum --check ./*.sha256
  sha256sum ./*.deb
)
