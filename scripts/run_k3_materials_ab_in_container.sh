#!/usr/bin/env bash
set -euo pipefail

: "${EVIDENCE_ROOT:?EVIDENCE_ROOT is required}"
: "${HARBOROS_VALIDATOR_ROOT:?HARBOROS_VALIDATOR_ROOT is required}"
: "${MODEL_BUNDLE_ROOT:?MODEL_BUNDLE_ROOT is required}"
: "${MODEL_LICENSE_EVIDENCE_ROOT:?MODEL_LICENSE_EVIDENCE_ROOT is required}"
: "${SOURCE_COMMIT:?SOURCE_COMMIT is required}"
: "${SOURCE_DATE_EPOCH:?SOURCE_DATE_EPOCH is required}"
: "${DEBIAN_VERSION:?DEBIAN_VERSION is required}"
: "${HARBORBEACON_BUILD_CONTAINER_DIGEST:?HARBORBEACON_BUILD_CONTAINER_DIGEST is required}"
: "${HARBORBEACON_DEBIAN_SNAPSHOT:?HARBORBEACON_DEBIAN_SNAPSHOT is required}"

source_root_a="${SOURCE_ROOT_A:-$EVIDENCE_ROOT/root-a/source}"
source_root_b="${SOURCE_ROOT_B:-$EVIDENCE_ROOT/root-b/source}"
cargo_home="${BUILD_CARGO_HOME:-$EVIDENCE_ROOT/cargo-home}"
seed_cargo_home="${SEED_CARGO_HOME:-}"
target="${RUST_TARGET:-riscv64gc-unknown-linux-gnu}"
deb_arch="${DEB_ARCH:-riscv64}"
inspection="$EVIDENCE_ROOT/inspection"

[[ "$SOURCE_COMMIT" =~ ^[0-9a-f]{40}$ ]] || {
  echo "error: SOURCE_COMMIT must be a full lowercase Git commit" >&2
  exit 2
}
[[ "$SOURCE_DATE_EPOCH" =~ ^[0-9]+$ ]] || {
  echo "error: SOURCE_DATE_EPOCH must be numeric" >&2
  exit 2
}
for path in \
  "$EVIDENCE_ROOT" \
  "$HARBOROS_VALIDATOR_ROOT" \
  "$MODEL_BUNDLE_ROOT" \
  "$MODEL_LICENSE_EVIDENCE_ROOT" \
  "$source_root_a" \
  "$source_root_b"
do
  [[ -d "$path" && ! -L "$path" ]] || {
    echo "error: required directory is missing or unsafe: $path" >&2
    exit 2
  }
done
[[ ! -e "$cargo_home" ]] || {
  echo "error: fresh CARGO_HOME already exists: $cargo_home" >&2
  exit 2
}
for run in root-a root-b; do
  [[ ! -e "$EVIDENCE_ROOT/$run/artifacts" ]] || {
    echo "error: artifact root already exists for $run" >&2
    exit 2
  }
done

install -d "$cargo_home" "$inspection"
if [[ -n "$seed_cargo_home" ]]; then
  [[ -d "$seed_cargo_home" && ! -L "$seed_cargo_home" ]] || {
    echo "error: Cargo seed is missing or unsafe: $seed_cargo_home" >&2
    exit 2
  }
  cp -a "$seed_cargo_home/." "$cargo_home/"
fi
export CARGO_HOME="$cargo_home"

for source_root in "$source_root_a" "$source_root_b"; do
  git config --global --add safe.directory "$source_root"
  [[ "$(git -C "$source_root" rev-parse HEAD)" == "$SOURCE_COMMIT" ]]
  [[ -z "$(git -C "$source_root" status --porcelain --untracked-files=all)" ]]
done

cd "$source_root_a"
bash scripts/configure_debian_snapshot.sh
apt-get update
apt-get install --yes --no-install-recommends \
  adduser \
  binutils \
  curl \
  dpkg-dev \
  file \
  gcc-riscv64-linux-gnu \
  init-system-helpers \
  libc6-dev-riscv64-cross \
  python3 \
  python3-jsonschema \
  python3-yaml \
  xz-utils
rustup target add "$target"

bash -n scripts/build_harbornavi_k3_deb.sh
bash -n scripts/build_model_runtime_k3_deb.sh
bash -n scripts/build_cat_vision_runtime_k3_deb.sh
bash -n scripts/run_k3_materials_ab_in_container.sh
python3 -m unittest tests.test_k3_packaging_contract
python3 scripts/validate_k3_model_materials.py \
  --manifest models/k3-evt1-model-materials.json \
  --bundle-root "$MODEL_BUNDLE_ROOT" \
  --verify-license-evidence \
  --license-evidence-root "$MODEL_LICENSE_EVIDENCE_ROOT" \
  | tee "$inspection/model-material-validation.txt"
python3 scripts/validate_k3_model_materials.py \
  --manifest models/k3-evt1-cat-vision-materials.json \
  --bundle-root "$MODEL_BUNDLE_ROOT" \
  | tee "$inspection/cat-vision-material-validation.txt"

for run in root-a root-b; do
  case "$run" in
    root-a) source_root="$source_root_a" ;;
    root-b) source_root="$source_root_b" ;;
  esac
  target_root="$EVIDENCE_ROOT/$run/target"
  for component in beacon model-runtime cat-vision-runtime; do
    artifact_root="$EVIDENCE_ROOT/$run/artifacts/$component"
    work_root="$EVIDENCE_ROOT/$run/work/$component"
    install -d "$artifact_root" "$target_root" "$work_root"
    case "$component" in
      beacon) build_script=scripts/build_harbornavi_k3_deb.sh ;;
      model-runtime) build_script=scripts/build_model_runtime_k3_deb.sh ;;
      cat-vision-runtime) build_script=scripts/build_cat_vision_runtime_k3_deb.sh ;;
    esac
    cd "$source_root"
    env \
      CARGO_HOME="$cargo_home" \
      CARGO_TARGET_DIR="$target_root" \
      DEB_ARCH="$deb_arch" \
      DEBIAN_VERSION="$DEBIAN_VERSION" \
      HARBORBEACON_BUILD_CONTAINER_DIGEST="$HARBORBEACON_BUILD_CONTAINER_DIGEST" \
      HARBORBEACON_DEBIAN_SNAPSHOT="$HARBORBEACON_DEBIAN_SNAPSHOT" \
      MODEL_BUNDLE_ROOT="$MODEL_BUNDLE_ROOT" \
      OUT_DIR="$artifact_root" \
      PACKAGE_WORK_ROOT="$work_root" \
      RUST_TARGET="$target" \
      SOURCE_COMMIT="$SOURCE_COMMIT" \
      SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" \
      bash "$build_script"
    (
      cd "$artifact_root"
      sha256sum --check ./*.sha256
      find . -maxdepth 1 -type f -printf '%f\0' \
        | sort -z \
        | xargs -0 sha256sum \
        > "$inspection/$run-$component.sha256"
    )
  done
done

for component in beacon model-runtime cat-vision-runtime; do
  diff --no-dereference --recursive \
    "$EVIDENCE_ROOT/root-a/artifacts/$component" \
    "$EVIDENCE_ROOT/root-b/artifacts/$component" \
    > "$inspection/$component-ab.diff"
  diff -u \
    "$inspection/root-a-$component.sha256" \
    "$inspection/root-b-$component.sha256" \
    > "$inspection/$component-ab.sha256.diff"
done

PYTHONPATH="$HARBOROS_VALIDATOR_ROOT/build/src" python3 - \
  "$EVIDENCE_ROOT" \
  "$SOURCE_COMMIT" \
  "$DEBIAN_VERSION" \
  "$deb_arch" \
  > "$inspection/central-validator.jsonl" <<'PY'
import json
import sys
from pathlib import Path

from harboros_build.k3.package_materials import inspect_harbor_package_materials

root = Path(sys.argv[1])
source_commit = sys.argv[2]
version = sys.argv[3]
architecture = sys.argv[4]
source_repo = "https://github.com/Bean-Harbor/HarborBeacon"
packages = (
    ("beacon", "harboros-beacon", True),
    ("model-runtime", "harboros-model-runtime", True),
    ("cat-vision-runtime", "harboros-cat-vision-runtime", False),
)
for run in ("root-a", "root-b"):
    for component, package, expected_eligible in packages:
        material_root = root / run / "artifacts" / component
        artifacts = list(material_root.glob("*.deb"))
        assert len(artifacts) == 1, (run, component, artifacts)
        result = inspect_harbor_package_materials(
            artifacts[0],
            package=package,
            version=version,
            architecture=architecture,
            source_repo=source_repo,
            source_commit=source_commit,
        )
        assert result.present
        assert result.release_eligible is expected_eligible
        print(
            json.dumps(
                {
                    "blocker": result.blocker,
                    "component": component,
                    "descriptor_sha256": result.descriptor_sha256,
                    "manifest_sha256": result.manifest_sha256,
                    "material_count": len(result.material_paths),
                    "release_eligible": result.release_eligible,
                    "run": run,
                },
                sort_keys=True,
            )
        )
PY

install -d \
  "$EVIDENCE_ROOT/artifacts/beacon" \
  "$EVIDENCE_ROOT/artifacts/model-runtime" \
  "$EVIDENCE_ROOT/artifacts/cat-vision-runtime"
cp -a "$EVIDENCE_ROOT/root-a/artifacts/beacon/." "$EVIDENCE_ROOT/artifacts/beacon/"
cp -a "$EVIDENCE_ROOT/root-a/artifacts/model-runtime/." \
  "$EVIDENCE_ROOT/artifacts/model-runtime/"
cp -a "$EVIDENCE_ROOT/root-a/artifacts/cat-vision-runtime/." \
  "$EVIDENCE_ROOT/artifacts/cat-vision-runtime/"
printf 'PASS packaging-materials; cat-vision-runtime release_eligible=false\n' \
  > "$EVIDENCE_ROOT/status.txt"
(
  cd "$EVIDENCE_ROOT"
  find artifacts inputs inspection -type f -print0 \
    | sort -z \
    | xargs -0 sha256sum
  sha256sum status.txt
) > "$EVIDENCE_ROOT/evidence-root.sha256"
