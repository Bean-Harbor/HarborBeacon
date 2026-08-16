import hashlib
import importlib.util
import io
import json
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).parents[1]


class K3PackagingContractTests(unittest.TestCase):
    def test_release_material_templates_are_canonical_and_fail_closed(self):
        for relative in (
            "debian/first-party-rights.json",
            "debian/component-contract-beacon.json.in",
            "debian/component-contract-model-runtime.json.in",
            "debian/model-runtime-manifest.json.in",
            "debian/model-runtime-third-party.json",
        ):
            path = ROOT / relative
            value = json.loads(path.read_text(encoding="utf-8"))
            self.assertEqual(
                path.read_bytes(),
                (json.dumps(value, ensure_ascii=True, indent=2, sort_keys=True) + "\n").encode(),
            )

        script_path = ROOT / "scripts" / "generate_package_materials.py"
        spec = importlib.util.spec_from_file_location("generate_package_materials", script_path)
        self.assertIsNotNone(spec)
        self.assertIsNotNone(spec.loader)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        self.assertEqual(
            module.normalize_license_expression("MIT/Apache-2.0"),
            "MIT OR Apache-2.0",
        )
        self.assertTrue(module.valid_spdx_expression("MIT OR Apache-2.0"))
        model_review = module.model_license_review(
            ROOT / "models" / "k3-evt1-model-materials.json"
        )
        self.assertEqual(model_review["total"], 5)
        self.assertEqual(model_review["approved"], 2)
        self.assertEqual(model_review["blocked"], 3)
        runtime_review = module.runtime_license_review(
            ROOT / "debian" / "model-runtime-manifest.json.in",
            ROOT / "debian" / "model-runtime-third-party.json",
            "riscv64",
        )
        self.assertEqual(runtime_review["approved"], 0)
        self.assertEqual(runtime_review["blocked"], 3)

    def test_registry_license_declaration_is_bound_to_cargo_lock_checksum(self):
        script_path = ROOT / "scripts" / "generate_package_materials.py"
        spec = importlib.util.spec_from_file_location("generate_package_materials", script_path)
        self.assertIsNotNone(spec)
        self.assertIsNotNone(spec.loader)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            root_manifest = root / "Cargo.toml"
            registry_key = "index.crates.io-fixture"
            dependency_root = (
                root / "registry" / "src" / registry_key / "fixture-dep-1.0.0"
            )
            dependency_root.mkdir(parents=True)
            dependency_manifest = dependency_root / "Cargo.toml"
            root_manifest.write_text("[package]\nname='fixture-root'\nversion='0.1.0'\n")
            dependency_manifest_bytes = (
                b"[package]\nname='fixture-dep'\nversion='1.0.0'\nlicense='MIT'\n"
            )
            dependency_manifest.write_bytes(dependency_manifest_bytes)
            crate_archive = (
                root
                / "registry"
                / "cache"
                / registry_key
                / "fixture-dep-1.0.0.crate"
            )
            crate_archive.parent.mkdir(parents=True)
            with tarfile.open(crate_archive, "w:gz") as archive:
                member = tarfile.TarInfo("fixture-dep-1.0.0/Cargo.toml")
                member.size = len(dependency_manifest_bytes)
                archive.addfile(member, io.BytesIO(dependency_manifest_bytes))
            checksum = hashlib.sha256(crate_archive.read_bytes()).hexdigest()
            cargo_lock = root / "Cargo.lock"
            cargo_lock.write_text(
                "version = 4\n\n"
                "[[package]]\nname = \"fixture-root\"\nversion = \"0.1.0\"\n\n"
                "[[package]]\nname = \"fixture-dep\"\nversion = \"1.0.0\"\n"
                "source = \"registry+https://github.com/rust-lang/crates.io-index\"\n"
                f"checksum = \"{checksum}\"\n"
            )
            root_id = "path+file:///fixture#fixture-root@0.1.0"
            dependency_id = (
                "registry+https://github.com/rust-lang/crates.io-index"
                "#fixture-dep@1.0.0"
            )
            metadata = root / "metadata.json"
            metadata.write_text(
                json.dumps(
                    {
                        "packages": [
                            {
                                "id": root_id,
                                "license": None,
                                "manifest_path": str(root_manifest),
                                "name": "fixture-root",
                                "source": None,
                                "version": "0.1.0",
                            },
                            {
                                "id": dependency_id,
                                "license": "MIT",
                                "manifest_path": str(dependency_manifest),
                                "name": "fixture-dep",
                                "source": "registry+https://github.com/rust-lang/crates.io-index",
                                "version": "1.0.0",
                            },
                        ],
                        "resolve": {
                            "nodes": [{"id": root_id}, {"id": dependency_id}]
                        },
                    }
                )
            )
            review = module.cargo_license_review(metadata, root_manifest, cargo_lock)
            self.assertEqual(review["approved"], 1)
            self.assertEqual(review["blocked"], 0)
            dependency = review["dependencies"][0]
            self.assertEqual(dependency["checksum"], checksum)
            self.assertEqual(
                dependency["evidence_basis"],
                "cargo-lock-checksum-bound-manifest-declaration",
            )
            self.assertEqual(dependency["license_evidence"], [])

    def test_maintainer_and_runtime_shell_scripts_parse(self):
        shell = shutil.which("sh")
        if shell is None:
            self.skipTest("POSIX sh is not available on this host")
        for relative in (
            "debian/ensure-model-runtime-data-layout",
            "debian/model-runtime-postinst",
            "debian/model-runtime-prerm",
            "debian/wait-model-runtime-health",
            "scripts/run_k3_materials_ab_in_container.sh",
        ):
            subprocess.run(
                [shell, "-n", str(ROOT / relative)],
                check=True,
                capture_output=True,
                text=True,
            )

    def test_lifecycle_executes_failure_and_deadline_paths(self):
        lifecycle = (
            ROOT / "scripts" / "test_model_runtime_deb_lifecycle.sh"
        ).read_text(encoding="utf-8")
        fixture = (
            ROOT / "scripts" / "build_model_runtime_lifecycle_fixture.sh"
        ).read_text(encoding="utf-8")
        for evidence in (
            '"$work_root/control/prerm" upgrade',
            "HARBOR_TEST_FAIL_VLM_ONCE",
            "HARBOR_TEST_VERIFY_FAULT",
            "kill -TERM \"$PPID\"",
            "runuser -u harbormodel",
            "curl-success",
            "date-deadline",
        ):
            self.assertIn(evidence, lifecycle)
        self.assertIn("dpkg-deb --root-owner-group --build", fixture)

    def test_beacon_service_uses_system_owned_credentials_fail_closed(self):
        unit = (ROOT / "debian" / "harboros-beacon.service").read_text(
            encoding="utf-8"
        )
        self.assertIn("Requires=harboros-bootstrap.service", unit)
        self.assertIn("After=network-online.target harboros-bootstrap.service", unit)
        self.assertLess(
            unit.index("LoadCredential=harbor-edge-assertion-key:"),
            unit.index("ExecStart="),
        )
        self.assertIn(
            "EnvironmentFile=/data/harboros/secrets/beacon-gate.env", unit
        )
        self.assertNotIn("EnvironmentFile=-/etc/default/harboros-beacon", unit)
        self.assertIn(
            "LoadCredential=harborlink-local-api-token:/data/harborlink/secrets/local-api.token",
            unit,
        )
        self.assertNotIn("/etc/harborlink/local-api.token", unit)
        postinst = (ROOT / "debian" / "postinst").read_text(encoding="utf-8")
        self.assertNotIn("/etc/default/harboros-beacon", postinst)
        self.assertNotIn("append_env_if_missing", postinst)
        self.assertIn(
            "LoadCredential=harbor-edge-assertion-key:/data/harboros/secrets/edge-assertion.key",
            unit,
        )
        self.assertIn(
            "Environment=HARBOR_EDGE_ASSERTION_KEY_FILE=%d/harbor-edge-assertion-key",
            unit,
        )
        self.assertNotIn("HARBOR_EDGE_ASSERTION_KEY=", unit)
        self.assertIn("ReadWritePaths=/data/harborbeacon", unit)
        self.assertNotIn("ReadWritePaths=/data/harboros", unit)
        self.assertIn("RequiresMountsFor=/data", unit)
        build = (ROOT / "scripts" / "build_harbornavi_k3_deb.sh").read_text(
            encoding="utf-8"
        )
        for package in (
            "harboros-system",
            "harborlink",
            "harboros-model-runtime",
        ):
            self.assertIn(f"{package} (>= 0.1.0~evt.1)", build)
            self.assertIn(f"{package} (<< 0.2)", build)

    def test_component_contract_paths_do_not_collide(self):
        beacon_build = (ROOT / "scripts" / "build_harbornavi_k3_deb.sh").read_text(
            encoding="utf-8"
        )
        model_build = (ROOT / "scripts" / "build_model_runtime_k3_deb.sh").read_text(
            encoding="utf-8"
        )
        beacon_contract = (
            ROOT / "debian" / "component-contract-beacon.json.in"
        ).read_text(encoding="utf-8")
        self.assertIn("usr/share/harboros/component-contract.json", beacon_build)
        self.assertIn(
            "usr/share/harboros/component-contracts/harboros-model-runtime.json",
            model_build,
        )
        self.assertNotIn(
            '"$pkg_dir/usr/share/harboros/component-contract.json"', model_build
        )
        model_contract = (
            ROOT / "debian" / "component-contract-model-runtime.json.in"
        ).read_text(encoding="utf-8")
        self.assertEqual(
            set(json.loads(model_contract)),
            {"contracts", "package", "schema_version", "source_commit"},
        )
        for capability in (
            "lazy-model-loading",
            "loopback-only",
            "signed-model-materials",
        ):
            self.assertIn(capability, model_contract)
        self.assertNotIn('"lazy-model-load"', model_contract)
        self.assertNotIn('"signed-package-only"', model_contract)
        for capability in (
            "hmac-sha256",
            "loopback-only",
            "nonce-replay-rejection",
            "role-full-admin",
            "role-trusted-lan",
        ):
            self.assertIn(capability, beacon_contract)

    def test_external_profile_fails_startup_without_edge_key_file(self):
        service = (ROOT / "src" / "bin" / "harborbeacon_service.rs").read_text(
            encoding="utf-8"
        )
        admin = (ROOT / "src" / "bin" / "agent_hub_admin_api.rs").read_text(
            encoding="utf-8"
        )
        self.assertIn('#[cfg(feature = "external-model-runtime")]', service)
        self.assertIn("EdgeAssertionVerifier::from_credential_env()", service)
        self.assertIn("std::process::exit(2)", service)
        self.assertIn(
            'const HARBOR_EDGE_ASSERTION_KEY_FILE_ENV: &str = "HARBOR_EDGE_ASSERTION_KEY_FILE";',
            admin,
        )

    def test_model_runtime_is_bootstrap_gated_and_not_legacy_overridable(self):
        unit = (ROOT / "debian" / "harboros-model-runtime.service").read_text(
            encoding="utf-8"
        )
        control = (ROOT / "debian" / "model-runtime-control.in").read_text(
            encoding="utf-8"
        )
        self.assertIn("Requires=harboros-bootstrap.service", unit)
        self.assertIn("After=local-fs.target harboros-bootstrap.service", unit)
        self.assertIn("Environment=HARBOR_MODEL_API_BIND=127.0.0.1:8792", unit)
        self.assertIn("Environment=HARBOR_MODEL_API_BACKEND=candle", unit)
        self.assertNotIn("EnvironmentFile=", unit)
        self.assertIn("harboros-system (>= 0.1.0~evt.1)", control)
        self.assertIn("harboros-system (<< 0.2)", control)

    def test_k3_event_vlm_is_loopback_only_and_fully_pinned(self):
        vlm_unit = (ROOT / "debian" / "harboros-vlm-runtime.service").read_text(
            encoding="utf-8"
        )
        beacon_unit = (ROOT / "debian" / "harboros-beacon.service").read_text(
            encoding="utf-8"
        )
        control = (ROOT / "debian" / "model-runtime-control.in").read_text(
            encoding="utf-8"
        )
        build = (ROOT / "scripts" / "build_model_runtime_k3_deb.sh").read_text(
            encoding="utf-8"
        )
        runtime_manifest = json.loads(
            (ROOT / "debian" / "model-runtime-manifest.json.in").read_text(
                encoding="utf-8"
            )
        )

        self.assertIn("--host 127.0.0.1 --port 8080", vlm_unit)
        self.assertIn("--vision-backend smt", vlm_unit)
        self.assertIn("--ctx-size 4096 -t 8 -tb 8", vlm_unit)
        self.assertIn("--no-warmup", vlm_unit)
        self.assertIn("Environment=OMP_NUM_THREADS=8", vlm_unit)
        self.assertIn("LimitNOFILE=1048576", vlm_unit)
        self.assertNotIn("taskset", vlm_unit)
        self.assertNotIn("--media-path", vlm_unit)
        self.assertIn("ReadOnlyPaths=/data/models", vlm_unit)
        self.assertIn(
            "ExecStartPost=/usr/lib/harboros-model-runtime/wait-health "
            "http://127.0.0.1:8080/health 300",
            vlm_unit,
        )
        self.assertIn("TimeoutStartSec=315", vlm_unit)
        self.assertNotIn("0.0.0.0", vlm_unit)
        self.assertIn("HARBORNAVI_VLM_API_BASE=http://127.0.0.1:8080/v1", beacon_unit)
        self.assertIn("harboros-vlm-runtime.service", beacon_unit)

        exact_dependencies = (
            "llama.cpp-tools-spacemit (= 0.1.1)",
            "spacemit-onnxruntime (= 2.0.3+3)",
            "spacemit-tcm (= 3.0.0+3)",
        )
        build_dependencies = (
            "llama.cpp-tools-spacemit=0.1.1",
            "spacemit-onnxruntime=2.0.3+3",
            "spacemit-tcm=3.0.0+3",
        )
        for dependency in exact_dependencies:
            self.assertIn(dependency, control)
        for dependency in build_dependencies:
            self.assertIn(dependency, build)

        services = {
            service["unit"]: service for service in runtime_manifest["services"]
        }
        self.assertEqual(
            services["harboros-model-runtime.service"]["health"],
            "http://127.0.0.1:8792/healthz",
        )
        self.assertEqual(
            services["harboros-vlm-runtime.service"]["health"],
            "http://127.0.0.1:8080/health",
        )

    def test_model_release_install_is_root_owned_atomic_and_manifest_verified(self):
        postinst = (ROOT / "debian" / "model-runtime-postinst").read_text(
            encoding="utf-8"
        )
        prerm = (ROOT / "debian" / "model-runtime-prerm").read_text(
            encoding="utf-8"
        )
        layout = (ROOT / "debian" / "ensure-model-runtime-data-layout").read_text(
            encoding="utf-8"
        )
        verifier = (ROOT / "scripts" / "verify_k3_model_release.py").read_text(
            encoding="utf-8"
        )
        model_unit = (ROOT / "debian" / "harboros-model-runtime.service").read_text(
            encoding="utf-8"
        )

        self.assertIn('mktemp -d "/data/models/releases/.VERSION_PLACEHOLDER.install.XXXXXXXX"', postinst)
        self.assertIn('chown root:root "$staging_root"', postinst)
        self.assertIn('"$verify" --manifest "$manifest" --root "$staging_root"', postinst)
        self.assertIn('mv -- "$release_root" "$backup_root"', postinst)
        self.assertIn('mv -- "$staging_root" "$release_root"', postinst)
        self.assertIn("mv -Tf /data/models/current.new /data/models/current", postinst)
        self.assertNotIn("chown -R harbormodel", postinst)
        self.assertIn("systemctl restart harboros-model-runtime.service", postinst)
        self.assertIn("systemctl restart harboros-vlm-runtime.service", postinst)
        self.assertIn("remove|upgrade|deconfigure", prerm)
        self.assertIn(
            "systemctl stop harboros-model-runtime.service "
            "harboros-vlm-runtime.service",
            prerm,
        )
        self.assertIn("install -d -o root -g root -m 0755", layout)
        self.assertIn("ReadOnlyPaths=/data/models", model_unit)
        self.assertIn("ReadWritePaths=/data/models/cache", model_unit)
        self.assertIn("TimeoutStartSec=75", model_unit)
        self.assertIn('transaction_committed=0', postinst)
        self.assertIn("trap cleanup EXIT", postinst)
        self.assertIn("trap 'exit 1' HUP INT TERM", postinst)
        self.assertIn("/run/harboros-model-runtime/upgrade-active", postinst)
        self.assertIn("/run/harboros-model-runtime", prerm)
        self.assertIn("systemctl is-active --quiet harboros-model-runtime.service", postinst)
        self.assertIn("systemctl is-active --quiet harboros-vlm-runtime.service", postinst)
        self.assertIn('mv -- "$backup_root" "$release_root"', postinst)
        self.assertIn("os.lstat", verifier)
        self.assertIn("followlinks=False", verifier)
        self.assertIn("unexpected file:", verifier)
        self.assertIn("SHA256 mismatch:", verifier)

    def test_model_manifest_locks_only_selected_vlm_resolution(self):
        manifest = json.loads(
            (ROOT / "models" / "k3-evt1-model-materials.json").read_text(
                encoding="utf-8"
            )
        )
        self.assertTrue(all(item["state"] == "locked" for item in manifest["materials"]))
        paths = [
            file_entry["package_path"]
            for material in manifest["materials"]
            for file_entry in material["files"]
        ]
        self.assertEqual(len(paths), 11)
        self.assertIn(
            "vlm/Qwen3.5-0.8B/qwen3_5vl_0.8b-vision-384-op23.f16.onnx",
            paths,
        )
        self.assertFalse(any("vision-224" in path or "vision-768" in path for path in paths))
        self.assertTrue(
            all(
                isinstance(file_entry["size"], int)
                and len(file_entry["sha256"]) == 64
                for material in manifest["materials"]
                for file_entry in material["files"]
            )
        )
        reviews = {item["id"]: item["license"] for item in manifest["materials"]}
        self.assertEqual(
            reviews["semantic-router-bootstrap-llm"]["declared_license"],
            "Apache-2.0",
        )
        self.assertEqual(
            reviews["rag-embedding-model"]["declared_license"],
            "Apache-2.0",
        )
        self.assertEqual(
            reviews["event-vlm-qwen3.5-0.8b-384"]["review_status"],
            "blocked",
        )

        workflow = (ROOT / ".github" / "workflows" / "k3-evt-package.yml").read_text(
            encoding="utf-8"
        )
        self.assertNotIn("--expect-blocked", workflow)
        rights = json.loads(
            (ROOT / "debian" / "first-party-rights.json").read_text(
                encoding="utf-8"
            )
        )
        self.assertEqual(rights["rights_holder"], "Harbor Innovations")
        self.assertEqual(
            rights["declared_license"],
            "LicenseRef-Harbor-Innovations-Proprietary",
        )
        self.assertFalse(rights["third_party"]["covered_by_approval"])
        self.assertTrue(rights["third_party"]["review_required"])

    def test_data_layout_helpers_are_fixed_and_installed(self):
        cases = [
            (
                "debian/ensure-beacon-data-layout",
                "/data/harborbeacon",
                "usr/lib/harborbeacon/ensure-data-layout",
                "scripts/build_harbornavi_k3_deb.sh",
            ),
            (
                "debian/ensure-model-runtime-data-layout",
                "/data/models",
                "usr/lib/harboros-model-runtime/ensure-data-layout",
                "scripts/build_model_runtime_k3_deb.sh",
            ),
        ]
        for helper_path, root, installed_path, build_path in cases:
            helper = (ROOT / helper_path).read_text(encoding="utf-8")
            build = (ROOT / build_path).read_text(encoding="utf-8")
            self.assertIn(f'"${{1:-}}" != "{root}"', helper)
            self.assertNotIn("secrets", helper)
            self.assertIn(installed_path, build)

    def test_release_builds_are_clean_and_use_generated_provenance_name(self):
        for script_name in (
            "scripts/build_harbornavi_k3_deb.sh",
            "scripts/build_model_runtime_k3_deb.sh",
        ):
            script = (ROOT / script_name).read_text(encoding="utf-8")
            self.assertIn("git status --porcelain --untracked-files=all", script)
            self.assertIn("build-provenance.json", script)
            self.assertNotIn("/provenance.json", script)
            self.assertIn("HARBORBEACON_DEBIAN_SNAPSHOT", script)
            self.assertIn("--debian-snapshot", script)
            self.assertIn("cd \"$out_dir\"", script)
            self.assertIn("sbom.cdx.json", script)
            self.assertIn("generate_package_provenance.py", script)
            self.assertIn("generate_package_materials.py", script)
            self.assertIn("--cargo-metadata", script)
            self.assertIn("--cargo-lock", script)
            self.assertNotIn(
                '--build-provenance "$out_dir/${material_prefix}.build-provenance.json"',
                script,
            )
            self.assertIn("release-materials", (
                ROOT / "scripts" / "generate_package_materials.py"
            ).read_text(encoding="utf-8"))
            self.assertIn(
                "--remap-path-prefix=${cargo_target_dir}=./target", script
            )

        reproducible = (ROOT / "scripts" / "verify_k3_reproducible.sh").read_text(
            encoding="utf-8"
        )
        self.assertIn("diff --no-dereference --recursive", reproducible)
        self.assertIn("sha256sum --check ./*.sha256", reproducible)
        self.assertIn('CARGO_TARGET_DIR="$work_root/$run/target"', reproducible)
        self.assertNotIn('CARGO_TARGET_DIR="$work_root/target"', reproducible)
        supply_chain = (ROOT / "scripts" / "generate_k3_supply_chain.py").read_text(
            encoding="utf-8"
        )
        self.assertIn('parser.add_argument("--debian-snapshot", required=True)', supply_chain)
        self.assertIn('parser.add_argument("--cargo-metadata", type=Path, required=True)', supply_chain)
        self.assertIn('parser.add_argument("--model-root", type=Path)', supply_chain)
        self.assertIn('parser.add_argument("--input-file", type=Path', supply_chain)
        self.assertIn('parser.add_argument("--runtime-dependency"', supply_chain)
        self.assertIn('"relationshipType": "CONTAINS"', supply_chain)
        self.assertIn('"toolchain"', supply_chain)
        self.assertIn('"riscv64_linux_gnu_gcc"', supply_chain)
        self.assertIn('"xz"', supply_chain)
        self.assertIn('"debian_packages"', supply_chain)
        for package in (
            "dpkg-dev",
            "gcc-riscv64-linux-gnu",
            "libc6-dev-riscv64-cross",
            "python3",
            "xz-utils",
        ):
            self.assertIn(f'"{package}"', supply_chain)
        workflow = (ROOT / ".github" / "workflows" / "k3-evt-package.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("libc6-dev-riscv64-cross", workflow)
        package_provenance = (
            ROOT / "scripts" / "generate_package_provenance.py"
        ).read_text(encoding="utf-8")
        self.assertIn('"subject"', package_provenance)
        self.assertIn("sha256(args.artifact)", package_provenance)
        driver = (
            ROOT / "scripts" / "run_k3_materials_ab_in_container.sh"
        ).read_text(encoding="utf-8")
        self.assertIn("inspect_harbor_package_materials", driver)
        self.assertIn("--verify-license-evidence", driver)
        self.assertIn("--license-evidence-root", driver)
        self.assertIn("diff --no-dereference --recursive", driver)
        self.assertIn("root-a", driver)
        self.assertIn("root-b", driver)

    def test_obsolete_public_release_path_is_fail_closed(self):
        workflow = (ROOT / ".github" / "workflows" / "release.yml").read_text(
            encoding="utf-8"
        )
        self.assertIn("legacy-release-disabled", workflow)
        self.assertIn("contents: read", workflow)
        self.assertIn("exit 1", workflow)
        self.assertNotIn("softprops/action-gh-release", workflow)
        self.assertNotIn("contents: write", workflow)


if __name__ == "__main__":
    unittest.main()
