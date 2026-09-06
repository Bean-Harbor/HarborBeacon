"""Black-box startup tests for the real unified Beacon product executable.

Build N2 with --no-default-features --features fixed-local-models --bin
harboros-beacon, and set HARBOR_TEST_N2_SERVICE_BIN to that executable. Set
HARBOR_TEST_N1_SERVICE_BIN to a separately preserved non-external-runtime
harboros-beacon executable for the strict credential tests. Missing binaries
skip their class; they never substitute an AdminApi fixture for the product.

All requests target a disposable loopback listener. No installed credentials,
models, device state, IM platform, or external server are used.
"""

from __future__ import annotations

import base64
import hmac
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from urllib.error import HTTPError, URLError
from urllib.request import build_opener, ProxyHandler, Request
import uuid


RULES = "/api/harbor-beacon/automation/rules"
GATE_TOKEN = "gate_test_0123456789abcdef0123456789abcdef"
MODEL_TOKEN = "model_test_0123456789abcdef0123456789abcdef"
EDGE_KEY = bytes.fromhex("2a" * 32)


def service_binary(variable: str) -> Path:
    configured = os.environ.get(variable)
    if not configured:
        raise unittest.SkipTest(f"{variable} must identify a compiled product binary")
    binary = Path(configured).resolve()
    if not binary.is_file():
        raise AssertionError(f"Configured product binary is missing: {binary}")
    return binary


def edge_headers(method: str, path: str, *, key: bytes = EDGE_KEY) -> dict[str, str]:
    timestamp = str(int(time.time()))
    nonce = base64.urlsafe_b64encode(os.urandom(12)).decode().rstrip("=")
    principal = "startup-test-admin"
    canonical = "\n".join(("v1", timestamp, nonce, method, path, principal, principal, "FULL_ADMIN"))
    signature = base64.urlsafe_b64encode(hmac.digest(key, canonical.encode(), "sha256")).decode().rstrip("=")
    return {
        "X-Harbor-Principal-Id": principal,
        "X-Harbor-Principal-Name": principal,
        "X-Harbor-Principal-Role": "FULL_ADMIN",
        "X-Harbor-Original-Method": method,
        "X-Harbor-Original-URI": path,
        "X-Harbor-Edge-Assertion": f"v1.{timestamp}.{nonce}.{signature}",
    }


class ProductProcess:
    def __init__(self, binary: Path, root: Path):
        self.binary = binary
        self.root = root
        root.mkdir(parents=True, exist_ok=True)
        self.credentials = root / "credentials"
        self.credentials.mkdir(exist_ok=True)
        self.edge_file = self.credentials / "harbor-edge-assertion-key"
        self.edge_file.write_text(EDGE_KEY.hex() + "\n", encoding="ascii")
        self.edge_file.chmod(0o600)
        (self.credentials / "harborlink-local-api-token").write_text("local-link-test-token\n", encoding="ascii")
        self.cat_state = root / "cat-reconciliation.json"
        self.webui = root / "webui"
        self.webui.mkdir(exist_ok=True)
        self.webui.joinpath("index.html").write_text("<!doctype html><title>Startup test</title>", encoding="ascii")
        self.env = {
            name: value for name, value in os.environ.items()
            if name.upper() in {"PATH", "SYSTEMROOT", "WINDIR", "COMSPEC", "PATHEXT", "TEMP", "TMP", "LANG", "LC_ALL"}
        }
        self.env.update({
            "HARBOR_EDGE_ASSERTION_KEY_FILE": str(self.edge_file),
            "HARBORBEACON_SOUTHBOUND_MODE": "harborlink",
            "HARBORLINK_MEDIA_API_URL": "http://127.0.0.1:9",
            "HARBORLINK_LOCAL_API_TOKEN_FILE": str(self.credentials / "harborlink-local-api-token"),
            "HARBOR_MODEL_API_BACKEND": "openai_proxy",
            "HARBOR_MODEL_API_BASE_URL": "http://127.0.0.1:9/v1",
            "HARBOR_MODEL_API_UPSTREAM_BASE_URL": "http://127.0.0.1:9",
            "HARBOR_K3_CAT_AUTO_RECORD_ENABLED": "false",
            "HARBOR_K3_CAT_RECORDING_VALIDATION_MODE": "off",
            "HARBOR_K3_CAT_ACTIVITY_POLICY_PATH": str(root / "cat-policy.json"),
            "HARBOR_K3_CAT_RECORDING_RECONCILIATION_PATH": str(self.cat_state),
            "HARBOR_K3_CAT_RECORDING_VALIDATION_STORE_PATH": str(root / "cat-validations.jsonl"),
            "HARBOR_K3_CAT_RECORDING_VALIDATION_TEMP_ROOT": str(root / "cat-temp"),
            "HARBOR_VISION_EVENT_STORE_PATH": str(root / "vision-events.jsonl"),
            "HARBOR_HARBOROS_WRITABLE_ROOT": str(root),
            "NO_PROXY": "*",
        })
        self.process: subprocess.Popen | None = None
        self.log = None
        self.launch_count = 0
        self.http = build_opener(ProxyHandler({}))

    def start(self):
        if self.process is not None:
            raise AssertionError("Stop the previous product process before restarting")
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", 0))
            port = listener.getsockname()[1]
        self.base = f"http://127.0.0.1:{port}"
        self.launch_count += 1
        self.log_path = self.root / f"service-{self.launch_count}.log"
        self.log = self.log_path.open("wb")
        self.process = subprocess.Popen([
            str(self.binary), "--bind", f"127.0.0.1:{port}",
            "--admin-state", str(self.root / "admin-console.json"),
            "--device-registry", str(self.root / "device-registry.json"),
            "--conversations", str(self.root / "conversations.json"),
            "--harbor-assistant-dist", str(self.webui), "--public-origin", self.base,
        ], cwd=self.root, env=self.env, stdout=self.log, stderr=subprocess.STDOUT)

    def stop(self):
        if self.process is not None:
            if self.process.poll() is None:
                self.process.terminate()
                try:
                    self.process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    self.process.kill()
                    self.process.wait(timeout=5)
            self.process = None
        if self.log is not None:
            self.log.close()
            self.log = None

    def request(self, path: str, method="GET", body=None, *, signed=False, token=None, key=EDGE_KEY):
        if not path.startswith("/") or path.startswith("//"):
            raise AssertionError("Tests may only request paths on their loopback product listener")
        headers = edge_headers(method, path, key=key) if signed else {}
        headers["Content-Type"] = "application/json"
        if token is not None:
            headers["Authorization"] = "Bearer " + token
        if path == "/api/web/turns":
            headers["X-Contract-Version"] = "2.0"
        request = Request(self.base + path, data=None if body is None else json.dumps(body).encode(), headers=headers, method=method)
        try:
            response = self.http.open(request, timeout=3)
        except HTTPError as error:
            response = error
        with response:
            raw = response.read()
            return response.status, json.loads(raw) if raw else None

    def wait_ready(self):
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise AssertionError(f"Product exited before health: {self.process.returncode}; {self.log_path.read_text(encoding='utf-8', errors='replace')}")
            try:
                status, body = self.request("/healthz")
                if status == 200:
                    return body
            except (URLError, OSError):
                pass
            time.sleep(0.05)
        raise AssertionError("Unified product listener did not become healthy")

    def wait_exit(self):
        try:
            return self.process.wait(timeout=10)
        except subprocess.TimeoutExpired as error:
            raise AssertionError("Invalid mandatory startup configuration did not stop the product") from error

    def thread_names(self):
        names = []
        for path in Path(f"/proc/{self.process.pid}/task").glob("*/comm"):
            try:
                names.append(path.read_text(encoding="ascii").strip())
            except FileNotFoundError:
                # Short-lived HTTP threads may exit while /proc is being read.
                continue
        return names


class N2CapabilityStartup(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.binary = service_binary("HARBOR_TEST_N2_SERVICE_BIN")

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="beacon-n2-startup-")
        self.addCleanup(self.temporary.cleanup)
        self.service = ProductProcess(self.binary, Path(self.temporary.name))
        self.addCleanup(self.service.stop)
        self.service.env["HARBOR_BEACON_STARTUP_PROFILE"] = "n2"

    def start(self):
        self.service.start()
        health = self.service.wait_ready()
        self.assertEqual(health["service"], "harborbeacon")
        self.assertEqual(health["startup"]["profile"], "n2")
        capabilities = health["startup"]["capabilities"]
        self.assertEqual(len(capabilities), len({item["capability"] for item in capabilities}))
        return {item["capability"]: item for item in capabilities}

    def assert_unavailable(self, capabilities, capability):
        self.assertEqual(capabilities[capability]["state"], "unavailable")
        self.assertTrue(capabilities[capability]["reason_code"])

    def rules_request(self, path=RULES, method="GET", body=None):
        status, payload = self.service.request(path, method, body, signed=True)
        self.assertEqual(status, 200, payload)
        return payload

    def create_rule(self, trigger=None):
        definition = {
            "name": "isolated startup record",
            "trigger": trigger or {"kind": "manual"},
            "conditions": {"match_mode": "all", "items": []},
            "actions": [{"kind": "record", "message": "local startup record"}],
            "expires_at": int(time.time()) + 180,
        }
        rule = self.rules_request(method="POST", body=definition)["rule"]
        self.assertEqual(rule["status"], "draft")
        path = RULES + "/" + rule["rule_id"]
        preview = self.rules_request(path + "/preview", "POST", {"revision": rule["revision"]})
        self.assertEqual(preview["revision"], rule["revision"])
        self.assertTrue(preview["conditions_matched"])
        enabled = self.rules_request(path + "/enable", "POST", {"revision": rule["revision"]})["rule"]
        self.assertEqual(enabled["status"], "enabled")
        return enabled, path

    def assert_manual_record_works(self):
        rule, path = self.create_rule()
        run = self.rules_request(path + "/run", "POST", {"revision": rule["revision"], "trigger_id": str(uuid.uuid4())})["run"]
        self.assertEqual(run["status"], "completed")
        self.assertEqual(run["trigger_kind"], "manual")
        self.assertEqual(run["actions"], [{"index": 0, "status": "succeeded", "message": "local startup record"}])
        self.assertEqual(len(self.rules_request(path + "/runs")["runs"]), 1)

    def assert_gate_denied(self, token=GATE_TOKEN):
        status, body = self.service.request("/api/web/turns", "POST", {"trace_id": "startup-test"}, token=token)
        self.assertEqual(status, 401, body)
        self.assertEqual(body["error"]["code"], "SERVICE_AUTH_FAILED")

    def test_missing_optional_credentials_keep_signed_rules_available(self):
        capabilities = self.start()
        self.assert_unavailable(capabilities, "gate_turns")
        self.assert_unavailable(capabilities, "local_inference")
        self.assert_manual_record_works()
        self.assert_gate_denied()
        self.assert_gate_denied(token=None)
        status, _ = self.service.request(RULES, signed=True, key=b"x" * 32)
        self.assertEqual(status, 401)
        status, body = self.service.request("/api/inference/healthz")
        self.assertEqual(status, 503)
        self.assertEqual(body["error"]["code"], "MODEL_RUNTIME_UNAVAILABLE")
        status, _ = self.service.request("/api/inference/v1/chat/completions", "POST", {"messages": []})
        self.assertEqual(status, 401)
        self.assertIsNone(self.service.process.poll())

    def test_partial_systemd_directory_uses_supported_legacy_gate_token(self):
        self.service.env.update({
            "CREDENTIALS_DIRECTORY": str(self.service.credentials),
            "HARBORBEACON_WEB_API_TOKEN": GATE_TOKEN,
            "HARBORGATE_BEARER_TOKEN": "sender_test_0123456789abcdef0123456789abcdef",
        })
        capabilities = self.start()
        self.assertEqual(capabilities["gate_turns"]["state"], "configured")
        self.assertIsNone(capabilities["gate_turns"]["reason_code"])
        status, body = self.service.request("/api/web/turns", "POST", {"trace_id": "startup-test"}, token=GATE_TOKEN)
        self.assertEqual(status, 422, body)
        self.assertEqual(body["error"]["code"], "VALIDATION_ERROR")
        self.assert_gate_denied(token="wrong_test_0123456789abcdef0123456789abcdef")
        self.assert_manual_record_works()

    def test_configured_but_offline_model_does_not_block_rules(self):
        # Fixed N2 builds always use this port. Reserve it without accepting traffic.
        with socket.socket() as reservation:
            if hasattr(socket, "SO_EXCLUSIVEADDRUSE"):
                reservation.setsockopt(socket.SOL_SOCKET, socket.SO_EXCLUSIVEADDRUSE, 1)
            try:
                reservation.bind(("127.0.0.1", 8792))
            except OSError as error:
                self.skipTest(f"Cannot safely reserve fixed model port 8792: {error}")
            self.service.env["HARBOR_MODEL_API_TOKEN"] = MODEL_TOKEN
            try:
                capabilities = self.start()
                self.assertEqual(capabilities["local_inference"]["state"], "configured")
                self.assertIsNone(capabilities["local_inference"]["reason_code"])
                status, body = self.service.request("/api/inference/healthz")
                self.assertEqual(status, 503, body)
                self.assertEqual(body["error"]["code"], "MODEL_RUNTIME_UNAVAILABLE")
                self.assert_manual_record_works()
                self.assertIsNone(self.service.process.poll())
            finally:
                self.service.stop()

    def test_invalid_explicit_gate_file_does_not_enable_legacy_token(self):
        self.service.env.update({
            "HARBOR_GATE_TO_BEACON_TOKEN_FILE": str(self.service.root / "missing-explicit-gate-token"),
            "HARBORBEACON_WEB_API_TOKEN": GATE_TOKEN,
        })
        capabilities = self.start()
        self.assert_unavailable(capabilities, "gate_turns")
        self.assert_gate_denied()
        self.assert_manual_record_works()

    def test_missing_link_credential_disables_media_but_keeps_rules_available(self):
        self.service.env["HARBORLINK_LOCAL_API_TOKEN_FILE"] = str(self.service.root / "missing-link-token")
        self.service.env.pop("HARBORLINK_LOCAL_API_TOKEN", None)
        self.service.env.pop("CREDENTIALS_DIRECTORY", None)
        capabilities = self.start()
        self.assert_unavailable(capabilities, "harborlink")
        self.assert_unavailable(capabilities, "vision")
        self.assertEqual(capabilities["vision"]["reason_code"], "HARBORLINK_CONFIG_UNAVAILABLE")
        self.assert_manual_record_works()

    def test_missing_mandatory_edge_key_stops_product(self):
        self.service.env["HARBORBEACON_WEB_API_TOKEN"] = GATE_TOKEN
        self.service.env["HARBOR_EDGE_ASSERTION_KEY_FILE"] = str(self.service.root / "missing-edge-key")
        self.service.start()
        self.assertEqual(self.service.wait_exit(), 2)
        self.assertIn("edge assertion credential", self.service.log_path.read_text(encoding="utf-8"))
        with self.assertRaises((URLError, OSError)):
            self.service.request("/healthz")

    def test_explicit_profile_cannot_reinterpret_n2_binary_as_n1(self):
        self.service.env["HARBOR_BEACON_STARTUP_PROFILE"] = "n1"
        self.service.start()
        self.assertEqual(self.service.wait_exit(), 2)
        self.assertIn("does not match the compiled runtime", self.service.log_path.read_text(encoding="utf-8"))
        with self.assertRaises((URLError, OSError)):
            self.service.request("/healthz")

    def test_corrupt_core_state_stops_product_without_replacing_file(self):
        state = self.service.root / "admin-console.json"
        original = b"{not-valid-core-state"
        state.write_bytes(original)
        self.service.start()
        self.assertEqual(self.service.wait_exit(), 2)
        self.assertEqual(state.read_bytes(), original)
        with self.assertRaises((URLError, OSError)):
            self.service.request("/healthz")

    def test_invalid_visual_configuration_does_not_kill_rules(self):
        self.service.env["HARBOR_K3_CAT_RECORDING_VALIDATION_MODE"] = "invalid-startup-mode"
        capabilities = self.start()
        self.assert_unavailable(capabilities, "vision")
        self.assert_manual_record_works()

    def test_corrupt_visual_state_is_preserved_without_killing_rules(self):
        original = b"{not-valid-visual-state"
        self.service.cat_state.write_bytes(original)
        capabilities = self.start()
        self.assert_unavailable(capabilities, "vision")
        self.assert_manual_record_works()
        self.assertEqual(self.service.cat_state.read_bytes(), original)

    def test_corrupt_visual_validation_log_is_preserved_without_killing_rules(self):
        state = self.service.root / "cat-validations.jsonl"
        original = b"{not-valid-visual-validation\n"
        state.write_bytes(original)
        capabilities = self.start()
        self.assert_unavailable(capabilities, "vision")
        self.assertEqual(capabilities["vision"]["reason_code"], "VISION_VALIDATION_UNAVAILABLE")
        self.assert_manual_record_works()
        self.assertEqual(state.read_bytes(), original)

    def test_schedule_is_not_duplicated_by_http_clones_and_survives_restart(self):
        self.start()
        rule, path = self.create_rule({"kind": "schedule", "interval_seconds": 10})
        pid = self.service.process.pid
        for _ in range(20):
            self.rules_request()
            self.assertEqual(self.service.request("/healthz")[0], 200)
        self.assertEqual(self.service.process.pid, pid)
        deadline = time.monotonic() + 30
        runs = []
        while time.monotonic() < deadline:
            runs = self.rules_request(path + "/runs")["runs"]
            if runs and all(run["status"] == "completed" for run in runs):
                break
            time.sleep(0.2)
        self.assertTrue(runs, "Real Rules worker did not execute the schedule")
        self.assertTrue(all(run["status"] == "completed" for run in runs), "Scheduled record did not complete")
        paused = self.rules_request(path + "/pause", "POST", {"revision": rule["revision"]})["rule"]
        runs = self.rules_request(path + "/runs")["runs"]
        self.assertEqual(len(runs), len({run["trigger_id"] for run in runs}))
        self.assertTrue(all(run["status"] == "completed" and run["trigger_kind"] == "schedule" for run in runs))
        before = {run["run_id"] for run in runs}
        self.service.stop()
        self.start()
        restored = next(item for item in self.rules_request()["rules"] if item["rule_id"] == rule["rule_id"])
        self.assertEqual(restored["status"], "paused")
        self.assertEqual(restored["revision"], paused["revision"])
        self.assertEqual({run["run_id"] for run in self.rules_request(path + "/runs")["runs"]}, before)

    @unittest.skipUnless(sys.platform.startswith("linux"), "Linux /proc is required for real worker thread-count verification")
    def test_http_requests_do_not_spawn_additional_rules_workers(self):
        self.start()
        self.assertEqual(self.service.thread_names().count("harbor-rules"), 1)
        for _ in range(20):
            self.rules_request()
            self.service.request("/healthz")
        self.assertEqual(self.service.thread_names().count("harbor-rules"), 1)


class N1StrictCredentialStartup(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.binary = service_binary("HARBOR_TEST_N1_SERVICE_BIN")

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="beacon-n1-startup-")
        self.addCleanup(self.temporary.cleanup)
        self.service = ProductProcess(self.binary, Path(self.temporary.name))
        self.addCleanup(self.service.stop)
        self.service.env.update({
            "HARBOR_BEACON_STARTUP_PROFILE": "n1",
            "HARBOR_MODEL_API_TOKEN": MODEL_TOKEN,
            "HARBOR_MODEL_API_BACKEND": "semantic_router",
            "HARBORBEACON_WEB_API_TOKEN": GATE_TOKEN,
        })

    def test_valid_n1_file_credentials_start_protected_product(self):
        current = self.service.credentials / "gate-to-beacon-accept-current"
        previous = self.service.credentials / "gate-to-beacon-accept-previous"
        current.write_text(GATE_TOKEN + "\n", encoding="ascii")
        previous.write_text("", encoding="ascii")
        current.chmod(0o600)
        previous.chmod(0o600)
        self.service.env.update({
            "HARBOR_GATE_TO_BEACON_TOKEN_FILE": str(current),
            "HARBOR_GATE_TO_BEACON_TOKEN_PREVIOUS_FILE": str(previous),
        })
        self.service.start()
        health = self.service.wait_ready()
        self.assertEqual(health["service"], "harborbeacon")
        self.assertEqual(health["startup"]["profile"], "n1")
        capabilities = {item["capability"]: item for item in health["startup"]["capabilities"]}
        for capability in ("gate_turns", "local_inference", "harborlink", "vision"):
            self.assertEqual(capabilities[capability]["state"], "configured")
            self.assertIsNone(capabilities[capability]["reason_code"])
        status, body = self.service.request("/api/web/turns", "POST", {"trace_id": "startup-test"}, token=GATE_TOKEN)
        self.assertEqual(status, 422, body)
        self.assertEqual(body["error"]["code"], "VALIDATION_ERROR")
        status, body = self.service.request("/api/web/turns", "POST", {"trace_id": "startup-test"})
        self.assertEqual(status, 401, body)
        self.assertEqual(body["error"]["code"], "SERVICE_AUTH_FAILED")
        self.assertIsNone(self.service.process.poll())

    def test_legacy_env_does_not_replace_n1_mandatory_file_credentials(self):
        self.service.start()
        self.assertEqual(self.service.wait_exit(), 2)
        self.assertIn("HARBOR_GATE_TO_BEACON_TOKEN_FILE", self.service.log_path.read_text(encoding="utf-8"))
        with self.assertRaises((URLError, OSError)):
            self.service.request("/healthz")

    def test_partial_systemd_directory_does_not_relax_n1_credentials(self):
        self.service.env["CREDENTIALS_DIRECTORY"] = str(self.service.credentials)
        self.service.start()
        self.assertEqual(self.service.wait_exit(), 2)
        self.assertIn("HARBOR_GATE_TO_BEACON_TOKEN_FILE", self.service.log_path.read_text(encoding="utf-8"))
        with self.assertRaises((URLError, OSError)):
            self.service.request("/healthz")

    def test_explicit_profile_cannot_relax_n1_binary_to_n2(self):
        self.service.env["HARBOR_BEACON_STARTUP_PROFILE"] = "n2"
        self.service.start()
        self.assertEqual(self.service.wait_exit(), 2)
        self.assertIn("does not match the compiled runtime", self.service.log_path.read_text(encoding="utf-8"))
        with self.assertRaises((URLError, OSError)):
            self.service.request("/healthz")


if __name__ == "__main__":
    unittest.main()
