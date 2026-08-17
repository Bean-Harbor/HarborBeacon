import copy
import hashlib
import importlib.util
import io
import json
import os
import tarfile
import tempfile
import unittest
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).parents[1]


def load_script(name: str):
    path = ROOT / "scripts" / name
    spec = importlib.util.spec_from_file_location(name.removesuffix(".py"), path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class ModelRuntimeRightsV2Tests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.validator = load_script("validate_k3_model_materials.py")
        cls.materials = load_script("generate_package_materials.py")
        cls.outputs = load_script("verify_model_runtime_output_set.py")
        cls.dependencies = load_script("model_runtime_dependency_contract.py")
        cls.manifest_path = ROOT / "models" / "k3-evt1-model-materials.json"
        cls.manifest = json.loads(cls.manifest_path.read_text(encoding="utf-8"))
        cls.runtime_manifest_path = ROOT / "debian" / "model-runtime-manifest.json.in"
        cls.control_path = ROOT / "debian" / "model-runtime-control.in"

    def test_manifest_v2_has_exact_ordered_rights_shape(self):
        self.assertEqual(self.manifest["schema_version"], 2)
        evidence = [
            item
            for material in self.manifest["materials"]
            for item in material["license"]["evidence_files"]
        ]
        self.assertEqual(
            [item["id"] for item in evidence],
            [
                "model-license-qwen2.5-0.5b-instruct",
                "model-license-declaration-jina-embeddings-v2-base-zh",
                "model-distribution-license-jina-embeddings-v2-base-zh",
            ],
        )
        self.assertTrue(all(item["id"] == item["kind"] for item in evidence))
        self.assertEqual(evidence[0]["purpose"], "distribution-license")
        self.assertEqual(evidence[2]["sha256"], "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30")
        for material in self.manifest["materials"]:
            review = material["license"]
            self.assertTrue(review["distribution_license_present"])
            self.assertTrue(review["evidence_verified"])
            self.assertEqual(review["notice_status"], "review-required")
            self.assertEqual(review["notice_review"]["review_status"], "blocked")
            self.assertIsNone(review["notice_review"]["tree_sha256"])
            self.assertEqual(review["review_status"], "blocked")
        review = self.materials.model_license_review(self.manifest_path)
        self.assertEqual((review["approved"], review["blocked"]), (0, 2))

    def test_v1_pointer_drop_tamper_swap_and_missing_distribution_fail(self):
        legacy = copy.deepcopy(self.manifest)
        legacy["schema_version"] = 1
        self.assertTrue(self.validator.validate_manifest(legacy, None, None))

        dropped = copy.deepcopy(self.manifest)
        dropped["materials"][1]["license"]["evidence_files"].pop()
        self.assertTrue(self.validator.validate_manifest(dropped, None, None))

        tampered = copy.deepcopy(self.manifest)
        tampered["materials"][0]["license"]["evidence_files"][0]["sha256"] = "0" * 64
        self.assertTrue(self.validator.validate_manifest(tampered, None, None))

        swapped = copy.deepcopy(self.manifest)
        swapped["materials"][1]["license"]["evidence_files"].reverse()
        self.assertTrue(self.validator.validate_manifest(swapped, None, None))

        missing_distribution = copy.deepcopy(self.manifest)
        missing_distribution["materials"][1]["license"]["distribution_license_present"] = False
        self.assertTrue(self.validator.validate_manifest(missing_distribution, None, None))

    def test_raw_evidence_is_frozen_once_and_rejects_drop_tamper_and_swap(self):
        payloads = [b"qwen-license\n", b"jina-declaration\n", b"apache-license\n"]
        entries = []
        for index, payload in enumerate(payloads):
            digest = hashlib.sha256(payload).hexdigest()
            entries.append(
                {
                    "id": f"evidence-{index}",
                    "sha256": digest,
                    "installed_path": f"/usr/share/doc/harboros-model-runtime/model-licenses/m{index}/LICENSE",
                }
            )
        manifest = {
            "schema_version": 2,
            "materials": [{"license": {"evidence_files": entries}}],
        }
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            evidence_root = root / "evidence"
            stage_root = root / "stage"
            evidence_root.mkdir()
            for entry, payload in zip(entries, payloads, strict=True):
                (evidence_root / entry["sha256"]).write_bytes(payload)
            self.assertEqual(
                self.validator.verify_and_stage_license_evidence(
                    manifest, evidence_root, stage_root
                ),
                [],
            )
            for entry, payload in zip(entries, payloads, strict=True):
                installed = stage_root.joinpath(*Path(entry["installed_path"].lstrip("/")).parts)
                self.assertEqual(installed.read_bytes(), payload)

            (evidence_root / entries[0]["sha256"]).unlink()
            self.assertTrue(
                self.validator.verify_and_stage_license_evidence(manifest, evidence_root, None)
            )
            (evidence_root / entries[0]["sha256"]).write_bytes(b"tampered")
            self.assertTrue(
                self.validator.verify_and_stage_license_evidence(manifest, evidence_root, None)
            )
            (evidence_root / entries[0]["sha256"]).write_bytes(payloads[1])
            (evidence_root / entries[1]["sha256"]).write_bytes(payloads[0])
            self.assertTrue(
                self.validator.verify_and_stage_license_evidence(manifest, evidence_root, None)
            )

    def _supply_chain_fixture(self):
        source = self.manifest["materials"][0]["license"]["evidence_files"][0]
        evidence = {
            **source,
            "concluded_license": "Apache-2.0",
            "declared_license": "Apache-2.0",
        }
        package = "harboros-model-runtime"
        version = "1.0"
        architecture = "riscv64"
        prefix = f"{package}_{version}_{architecture}"
        spdx_id = "SPDXRef-ModelLicenseEvidence-" + source["id"]
        spdx = {
            "packages": [{"name": package, "versionInfo": version, "SPDXID": "SPDXRef-root"}],
            "files": [
                {
                    "SPDXID": spdx_id,
                    "checksums": [{"algorithm": "SHA256", "checksumValue": source["sha256"]}],
                    "copyrightText": "NOASSERTION",
                    "fileName": source["installed_path"],
                    "licenseConcluded": "Apache-2.0",
                    "licenseInfoInFiles": ["Apache-2.0"],
                }
            ],
            "relationships": [
                {
                    "relatedSpdxElement": spdx_id,
                    "relationshipType": "CONTAINS",
                    "spdxElementId": "SPDXRef-root",
                }
            ],
        }
        component = {
            "bom-ref": f"model-license-evidence:{source['id']}@sha256:{source['sha256']}",
            "hashes": [{"alg": "SHA-256", "content": source["sha256"]}],
            "licenses": [{"expression": "Apache-2.0"}],
            "name": source["id"],
            "properties": [
                {"name": "harboros:installed-path", "value": source["installed_path"]},
                {"name": "harboros:purpose", "value": source["purpose"]},
                {"name": "harboros:revision", "value": source["revision"]},
                {"name": "harboros:source", "value": source["source"]},
            ],
            "type": "file",
        }
        sidecar = f"{prefix}.{source['id']}.{source['filename']}"
        installed_spdx_sha = "1" * 64
        installed_cdx_sha = "2" * 64
        model_materials_sha = "3" * 64
        provenance = {
            "subject": [
                {"digest": {"sha256": source["sha256"]}, "name": sidecar},
                {
                    "digest": {"sha256": installed_spdx_sha},
                    "name": f"/usr/share/doc/{package}/sbom.spdx.json",
                },
                {
                    "digest": {"sha256": installed_cdx_sha},
                    "name": f"/usr/share/doc/{package}/sbom.cdx.json",
                },
            ],
            "predicate": {
                "buildDefinition": {
                    "externalParameters": {"version": version, "arch": architecture},
                    "resolvedDependencies": [
                        {
                            "digest": {"sha256": model_materials_sha},
                            "uri": f"{prefix}.model-materials.json",
                        },
                        {"digest": {"sha256": source["sha256"]}, "uri": source["source"]},
                        {"digest": {"sha256": source["sha256"]}, "uri": sidecar},
                    ],
                }
            },
        }
        return {
            "spdx": spdx,
            "cyclonedx": {"components": [component]},
            "provenance": provenance,
            "evidence_files": [evidence],
            "package": package,
            "version": version,
            "architecture": architecture,
            "sidecar_prefix": prefix,
            "installed_spdx_sha256": installed_spdx_sha,
            "installed_cyclonedx_sha256": installed_cdx_sha,
            "model_materials_sha256": model_materials_sha,
        }

    def test_provenance_and_sbom_evidence_bindings_fail_closed(self):
        fixture = self._supply_chain_fixture()
        self.materials.verify_model_license_supply_chain(**fixture)
        for collection, mutation in (
            ("spdx", lambda value: value["files"][0].update({"fileName": "/swapped"})),
            ("cyclonedx", lambda value: value["components"][0].update({"bom-ref": "swapped"})),
            (
                "provenance",
                lambda value: value["predicate"]["buildDefinition"]["resolvedDependencies"][0].update(
                    {"uri": "https://example.invalid/swapped"}
                ),
            ),
        ):
            changed = copy.deepcopy(fixture)
            mutation(changed[collection])
            with self.assertRaises(ValueError):
                self.materials.verify_model_license_supply_chain(**changed)

    def test_deb_and_build_provenance_bindings_fail_closed(self):
        commit = "a" * 40
        provenance = {
            "subject": [{"digest": {"sha256": "b" * 64}, "name": "runtime.deb"}],
            "predicate": {
                "buildDefinition": {
                    "externalParameters": {
                        "package": "harboros-model-runtime",
                        "version": "1.0",
                        "arch": "riscv64",
                    },
                    "resolvedDependencies": [
                        {
                            "digest": {"sha256": "c" * 64},
                            "uri": "build-provenance.json",
                        },
                        {
                            "digest": {"gitCommit": commit},
                            "uri": f"git+{self.materials.SOURCE_REPOSITORY}@{commit}",
                        },
                    ],
                }
            },
        }
        arguments = {
            "provenance": provenance,
            "artifact_name": "runtime.deb",
            "artifact_sha256": "b" * 64,
            "build_provenance_name": "build-provenance.json",
            "build_provenance_sha256": "c" * 64,
            "package": "harboros-model-runtime",
            "version": "1.0",
            "architecture": "riscv64",
            "source_commit": commit,
        }
        self.materials.verify_package_provenance(**arguments)
        for field in ("artifact_sha256", "build_provenance_sha256"):
            changed = {**arguments, field: "d" * 64}
            with self.assertRaises(ValueError):
                self.materials.verify_package_provenance(**changed)

    def test_tar_metadata_and_manifest_derived_output_count(self):
        payload = b"license\n"
        entry = {
            "installed_path": "/usr/share/doc/example/LICENSE",
            "sha256": hashlib.sha256(payload).hexdigest(),
        }
        for mode, should_pass in ((0o644, True), (0o600, False)):
            archive_bytes = io.BytesIO()
            with tarfile.open(fileobj=archive_bytes, mode="w", format=tarfile.GNU_FORMAT) as archive:
                directory = tarfile.TarInfo("usr/share/doc/example")
                directory.type = tarfile.DIRTYPE
                directory.mode = 0o755
                directory.uid = 0
                directory.gid = 0
                directory.mtime = 123
                archive.addfile(directory)
                member = tarfile.TarInfo("usr/share/doc/example/LICENSE")
                member.mode = mode
                member.uid = 0
                member.gid = 0
                member.mtime = 123
                member.size = len(payload)
                archive.addfile(member, io.BytesIO(payload))
            archive_bytes.seek(0)
            if should_pass:
                self.materials.verify_installed_evidence_tar(archive_bytes, [entry], 123)
            else:
                with self.assertRaises(ValueError):
                    self.materials.verify_installed_evidence_tar(archive_bytes, [entry], 123)
        archive_bytes = io.BytesIO()
        with tarfile.open(fileobj=archive_bytes, mode="w", format=tarfile.GNU_FORMAT) as archive:
            directory = tarfile.TarInfo("usr/share/doc/example")
            directory.type = tarfile.DIRTYPE
            directory.mode = 0o2755
            directory.uid = 0
            directory.gid = 0
            directory.mtime = 123
            archive.addfile(directory)
            member = tarfile.TarInfo("usr/share/doc/example/LICENSE")
            member.mode = 0o644
            member.uid = 0
            member.gid = 0
            member.mtime = 123
            member.size = len(payload)
            archive.addfile(member, io.BytesIO(payload))
        archive_bytes.seek(0)
        with self.assertRaisesRegex(ValueError, "special mode bits"):
            self.materials.verify_installed_evidence_tar(archive_bytes, [entry], 123)
        expected = self.outputs.expected_names(self.manifest, "1.0", "riscv64")
        evidence_count = sum(
            len(material["license"]["evidence_files"])
            for material in self.manifest["materials"]
        )
        self.assertEqual(len(expected), 18 + evidence_count)

    def test_model_runtime_dependency_contract_rejects_drift(self):
        expected_dependencies = [
            "libc6",
            "ca-certificates",
            "adduser",
            "curl",
            "init-system-helpers",
            "harboros-system (>= 0.1.0~evt.1)",
            "harboros-system (<< 0.2)",
        ]
        contract = self.dependencies.load_dependency_contract(
            self.runtime_manifest_path, self.control_path
        )
        self.assertEqual(contract["bundled_runtime_dependencies"], [])
        self.assertEqual(contract["debian_control_dependencies"], expected_dependencies)

        manifest = json.loads(self.runtime_manifest_path.read_text(encoding="utf-8"))
        control = self.control_path.read_text(encoding="utf-8")
        mutations = []

        reordered = copy.deepcopy(manifest)
        reordered["debian_control_dependencies"][0:2] = reversed(
            reordered["debian_control_dependencies"][0:2]
        )
        mutations.append((reordered, control))

        dropped = copy.deepcopy(manifest)
        dropped["debian_control_dependencies"].pop()
        mutations.append((dropped, control))

        legacy = copy.deepcopy(manifest)
        legacy["runtime_dependencies"] = legacy.pop("bundled_runtime_dependencies")
        mutations.append((legacy, control))

        injected = copy.deepcopy(manifest)
        injected["debian_control_dependencies"].append("spacemit-tcm (= 3.0.0+3)")
        injected_control = control.replace(
            "harboros-system (<< 0.2)",
            "harboros-system (<< 0.2), spacemit-tcm (= 3.0.0+3)",
        )
        mutations.append((injected, injected_control))

        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            manifest_path = root / "runtime-manifest.json"
            control_path = root / "control"
            for changed_manifest, changed_control in mutations:
                manifest_path.write_text(
                    json.dumps(changed_manifest, ensure_ascii=True, indent=2, sort_keys=True)
                    + "\n",
                    encoding="utf-8",
                )
                control_path.write_text(changed_control, encoding="utf-8")
                with self.assertRaises(ValueError):
                    self.dependencies.load_dependency_contract(
                        manifest_path, control_path
                    )

    def test_dependency_inputs_reject_symlinks_and_in_place_mutation(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / "control"
            source.write_bytes(self.control_path.read_bytes())
            link = root / "control-link"
            try:
                link.symlink_to(source)
            except OSError:
                link = None
            if link is not None:
                with self.assertRaises(ValueError):
                    self.dependencies.read_regular_bytes(
                        link,
                        max_bytes=self.dependencies.MAX_CONTROL_BYTES,
                        label="control",
                    )

            real_fstat = self.dependencies.os.fstat
            calls = 0

            def changing_fstat(descriptor):
                nonlocal calls
                calls += 1
                value = real_fstat(descriptor)
                if calls == 2:
                    changed = mock.Mock(wraps=value)
                    changed.st_dev = value.st_dev
                    changed.st_ino = value.st_ino
                    changed.st_size = value.st_size
                    changed.st_mode = value.st_mode
                    changed.st_mtime_ns = value.st_mtime_ns + 1
                    changed.st_ctime_ns = value.st_ctime_ns
                    return changed
                return value

            with mock.patch.object(
                self.dependencies.os, "fstat", side_effect=changing_fstat
            ):
                with self.assertRaisesRegex(ValueError, "changed while being read"):
                    self.dependencies.read_regular_bytes(
                        source,
                        max_bytes=self.dependencies.MAX_CONTROL_BYTES,
                        label="control",
                    )

            original = source.read_bytes()
            original_stat = source.stat()
            real_open = self.dependencies.os.open

            def mutate_before_open(path, flags):
                Path(path).write_bytes(b"X" + original[1:])
                os.utime(
                    path,
                    ns=(
                        original_stat.st_atime_ns,
                        original_stat.st_mtime_ns + 1_000_000_000,
                    ),
                )
                return real_open(path, flags)

            with mock.patch.object(
                self.dependencies.os, "open", side_effect=mutate_before_open
            ):
                with self.assertRaisesRegex(ValueError, "missing, unsafe"):
                    self.dependencies.read_regular_bytes(
                        source,
                        max_bytes=self.dependencies.MAX_CONTROL_BYTES,
                        label="control",
                    )

    def test_model_runtime_dependency_provenance_fails_closed(self):
        contract = self.dependencies.load_dependency_contract(
            self.runtime_manifest_path, self.control_path
        )
        provenance = {
            "predicate": {
                "buildDefinition": {
                    "externalParameters": {
                        "bundled_runtime_dependencies": [],
                        "debian_control_dependencies": contract[
                            "debian_control_dependencies"
                        ],
                    },
                    "resolvedDependencies": [
                        {
                            "digest": {"sha256": contract["control_sha256"]},
                            "uri": self.dependencies.CONTROL_URI,
                        }
                    ],
                }
            }
        }
        self.materials.verify_model_runtime_dependency_provenance(
            provenance, contract
        )
        mutations = (
            lambda value: value["predicate"]["buildDefinition"][
                "externalParameters"
            ].update({"runtime_dependencies": []}),
            lambda value: value["predicate"]["buildDefinition"][
                "externalParameters"
            ]["debian_control_dependencies"].reverse(),
            lambda value: value["predicate"]["buildDefinition"][
                "resolvedDependencies"
            ][0]["digest"].update({"sha256": "0" * 64}),
        )
        for mutation in mutations:
            changed = copy.deepcopy(provenance)
            mutation(changed)
            with self.assertRaises(ValueError):
                self.materials.verify_model_runtime_dependency_provenance(
                    changed, contract
                )

        control_bytes = self.control_path.read_bytes()
        byte_tampered = control_bytes.replace(b"Depends: ", b"Depends:  ", 1)
        tampered_contract = self.dependencies.load_dependency_contract_bytes(
            self.runtime_manifest_path, byte_tampered
        )
        self.assertEqual(
            tampered_contract["debian_control_dependencies"],
            contract["debian_control_dependencies"],
        )
        self.assertNotEqual(
            tampered_contract["control_sha256"], contract["control_sha256"]
        )
        with self.assertRaises(ValueError):
            self.materials.verify_model_runtime_dependency_provenance(
                provenance, tampered_contract
            )

    def test_actual_deb_control_member_is_unique_regular_and_exact(self):
        control_bytes = self.control_path.read_bytes()

        def archive_with(entries):
            payload = io.BytesIO()
            with tarfile.open(fileobj=payload, mode="w", format=tarfile.GNU_FORMAT) as archive:
                for name, content, kind in entries:
                    member = tarfile.TarInfo(name)
                    member.type = kind
                    member.mode = 0o644
                    if kind == tarfile.REGTYPE:
                        member.size = len(content)
                        archive.addfile(member, io.BytesIO(content))
                    else:
                        member.linkname = "other"
                        archive.addfile(member)
            payload.seek(0)
            return payload

        valid = archive_with([("./control", control_bytes, tarfile.REGTYPE)])
        self.assertEqual(self.materials.read_debian_control_tar(valid), control_bytes)

        invalid_archives = (
            archive_with([]),
            archive_with(
                [
                    ("./control", control_bytes, tarfile.REGTYPE),
                    ("control", control_bytes, tarfile.REGTYPE),
                ]
            ),
            archive_with([("./control", b"", tarfile.SYMTYPE)]),
        )
        for archive in invalid_archives:
            with self.assertRaises(ValueError):
                self.materials.read_debian_control_tar(archive)


if __name__ == "__main__":
    unittest.main()
