import unittest
from pathlib import Path


ROOT = Path(__file__).parents[1]


class K3PackagingContractTests(unittest.TestCase):
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

        reproducible = (ROOT / "scripts" / "verify_k3_reproducible.sh").read_text(
            encoding="utf-8"
        )
        self.assertIn("diff --no-dereference --recursive", reproducible)
        self.assertIn("sha256sum --check ./*.sha256", reproducible)
        supply_chain = (ROOT / "scripts" / "generate_k3_supply_chain.py").read_text(
            encoding="utf-8"
        )
        self.assertIn('parser.add_argument("--debian-snapshot", required=True)', supply_chain)
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
