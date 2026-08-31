import contextlib
import importlib.util
import io
from pathlib import Path
import re
import sys
import unittest
from unittest import mock


REPO_ROOT = Path(__file__).resolve().parents[1]
FETCH_SCRIPT = REPO_ROOT / "scripts" / "fetch_hf_bootstrap_model.py"
RELEASE_WORKFLOW = REPO_ROOT / ".github" / "workflows" / "release.yml"
EXPECTED_REVISION = "7ae557604adf67be50417f59c2c2f167def9a775"


def load_fetch_module():
    spec = importlib.util.spec_from_file_location("fetch_hf_bootstrap_model", FETCH_SCRIPT)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"cannot load {FETCH_SCRIPT}")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class BootstrapModelProvenanceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.fetch = load_fetch_module()

    def test_revision_validator_rejects_moving_refs(self):
        for revision in ("main", "refs/heads/main", "7ae5576", "g" * 40):
            with self.subTest(revision=revision):
                with self.assertRaises(ValueError):
                    self.fetch.validate_immutable_revision(revision)

        self.assertEqual(
            self.fetch.validate_immutable_revision(EXPECTED_REVISION.upper()),
            EXPECTED_REVISION,
        )

    def test_release_workflow_pins_the_verified_revision(self):
        workflow = RELEASE_WORKFLOW.read_text(encoding="utf-8")
        revisions = re.findall(r"--revision\s+([^\s\\]+)", workflow)

        self.assertEqual(self.fetch.DEFAULT_REVISION, EXPECTED_REVISION)
        self.assertEqual(revisions, [EXPECTED_REVISION])
        self.assertRegex(revisions[0], r"^[0-9a-f]{40}$")

    def test_metadata_sha_must_match_requested_revision(self):
        argv = [
            str(FETCH_SCRIPT),
            "--revision",
            EXPECTED_REVISION,
            "--output",
            "unused",
            "--dry-run",
        ]
        metadata = {"sha": "0" * 40, "siblings": []}

        with mock.patch.object(sys, "argv", argv), mock.patch.object(
            self.fetch, "request_json", return_value=metadata
        ):
            with self.assertRaisesRegex(SystemExit, "model revision mismatch"):
                self.fetch.main()

    def test_dry_run_resolves_the_revision_specific_endpoint(self):
        argv = [
            str(FETCH_SCRIPT),
            "--revision",
            EXPECTED_REVISION,
            "--output",
            "unused",
            "--dry-run",
        ]
        metadata = {
            "sha": EXPECTED_REVISION,
            "siblings": [{"rfilename": name} for name in self.fetch.DEFAULT_FILES],
        }
        requested_urls = []

        def request_json(url, _token):
            requested_urls.append(url)
            return metadata

        with mock.patch.object(sys, "argv", argv), mock.patch.object(
            self.fetch, "request_json", side_effect=request_json
        ), contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(self.fetch.main(), 0)

        self.assertEqual(
            requested_urls,
            [
                "https://huggingface.co/api/models/"
                f"Qwen/Qwen2.5-0.5B-Instruct/revision/{EXPECTED_REVISION}"
            ],
        )


if __name__ == "__main__":
    unittest.main()
