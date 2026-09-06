"""Package declarations for the private B2 execution protocol, not Gate v2.0."""

import json
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[1]
CONTRACT_ID = "harboros.n2.ai-execution"


class N2ExecutionComponentContractTests(unittest.TestCase):
    def contract(self, filename, package):
        text = (ROOT / "debian" / filename).read_text(encoding="utf-8")
        rendered = text.replace("SOURCE_COMMIT_PLACEHOLDER", "a" * 40)
        contract = json.loads(rendered)
        self.assertEqual(
            set(contract), {"contracts", "package", "schema_version", "source_commit"}
        )
        self.assertEqual(contract["package"], package)
        self.assertEqual(contract["schema_version"], 1)
        self.assertEqual(contract["source_commit"], "a" * 40)
        self.assertEqual(
            rendered, json.dumps(contract, indent=2, sort_keys=True) + "\n"
        )
        identifiers = [item["id"] for item in contract["contracts"]]
        self.assertEqual(len(identifiers), len(set(identifiers)))
        return {item["id"]: item for item in contract["contracts"]}

    def test_beacon_declares_client_without_claiming_executor(self):
        contracts = self.contract(
            "component-contract-beacon.json.in", "harboros-beacon"
        )
        self.assertEqual(contracts[CONTRACT_ID], {
            "id": CONTRACT_ID,
            "version": 1,
            "capabilities": ["runtime-owned-ai-execution-client-v1"],
        })
        self.assertIn("harboros.k3.beacon-edge-assertion", contracts)
        self.assertIn("harboros.k3.beacon-cat-activity", contracts)

    def test_runtime_declares_execution_owner_and_retains_text_materials(self):
        contracts = self.contract(
            "component-contract-model-runtime.json.in", "harboros-model-runtime"
        )
        self.assertEqual(contracts[CONTRACT_ID], {
            "id": CONTRACT_ID,
            "version": 1,
            "capabilities": ["runtime-owned-ai-execution-v1"],
        })
        self.assertIn(
            "signed-model-materials",
            contracts["harboros.k3.model-runtime"]["capabilities"],
        )

    def test_vision_material_package_does_not_claim_execution_owner(self):
        contracts = self.contract(
            "component-contract-cat-vision-runtime.json.in",
            "harboros-cat-vision-runtime",
        )
        self.assertNotIn(CONTRACT_ID, contracts)

    def test_release_builders_embed_the_declared_contract(self):
        for script, template, installed_path in (
            (
                "build_harbornavi_k3_deb.sh",
                "component-contract-beacon.json.in",
                "$pkg_dir/usr/share/harboros/component-contract.json",
            ),
            (
                "build_model_runtime_k3_deb.sh",
                "component-contract-model-runtime.json.in",
                "$pkg_dir/usr/share/harboros/component-contracts/harboros-model-runtime.json",
            ),
        ):
            with self.subTest(script=script):
                contents = (ROOT / "scripts" / script).read_text(encoding="utf-8")
                self.assertIn("debian/" + template, contents)
                self.assertIn(installed_path, contents)
                self.assertIn("--component-contract ", contents)

    def test_changed_templates_are_canonical_lf_bytes(self):
        attributes = (ROOT / ".gitattributes").read_text(encoding="utf-8")
        for filename in (
            "component-contract-beacon.json.in",
            "component-contract-model-runtime.json.in",
        ):
            with self.subTest(filename=filename):
                raw = (ROOT / "debian" / filename).read_bytes()
                self.assertEqual(
                    raw,
                    (json.dumps(json.loads(raw), indent=2, sort_keys=True) + "\n").encode(),
                )
                self.assertIn(f"debian/{filename} text eol=lf", attributes)

    def test_installed_generation_and_restart_barriers_remain(self):
        unit = (ROOT / "debian/n2/harboros-beacon.service").read_text(
            encoding="utf-8"
        )
        self.assertIn("ExecStartPre=+/usr/lib/harborbeacon/verify-k3-generation", unit)
        self.assertIn(
            "ExecStartPre=+/usr/bin/systemctl restart harboros-model-runtime.service",
            unit,
        )
        control = (ROOT / "debian/n2/control").read_text(encoding="utf-8")
        for package in ("harboros-model-runtime", "harboros-cat-vision-runtime"):
            self.assertIn(f"{package} (= VERSION_PLACEHOLDER)", control)


if __name__ == "__main__":
    unittest.main()
