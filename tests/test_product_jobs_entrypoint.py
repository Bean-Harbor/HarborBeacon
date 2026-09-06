"""ProductJob HTTP acceptance against the real compiled N2 service, on loopback."""

import base64
from concurrent.futures import ThreadPoolExecutor
import hmac
import json
import os
from pathlib import Path
import tempfile
import time
import unittest
from urllib.error import HTTPError
from urllib.request import Request

from test_n2_capability_startup import EDGE_KEY, ProductProcess, RULES, service_binary

JOBS = "/api/harbor-beacon/product-jobs"


def signed_headers(method, path, actor="jobs-test-admin", role="FULL_ADMIN"):
    timestamp = str(int(time.time()))
    nonce = base64.urlsafe_b64encode(os.urandom(12)).decode().rstrip("=")
    canonical = "\n".join(("v1", timestamp, nonce, method, path, actor, actor, role))
    signature = base64.urlsafe_b64encode(hmac.digest(EDGE_KEY, canonical.encode(), "sha256")).decode().rstrip("=")
    return {"X-Harbor-Principal-Id": actor, "X-Harbor-Principal-Name": actor,
            "X-Harbor-Principal-Role": role, "X-Harbor-Original-Method": method,
            "X-Harbor-Original-URI": path, "X-Harbor-Edge-Assertion": f"v1.{timestamp}.{nonce}.{signature}"}


def request(service, path=JOBS, method="GET", body=None, actor="jobs-test-admin", role="FULL_ADMIN"):
    headers = signed_headers(method, path, actor, role)
    headers["Content-Type"] = "application/json"
    req = Request(service.base + path, data=None if body is None else json.dumps(body).encode(), headers=headers, method=method)
    try:
        response = service.http.open(req, timeout=10)
    except HTTPError as error:
        response = error
    with response:
        return response.status, json.loads(response.read())


def create_rule_history(service):
    definition = {"name": "Evening record", "trigger": {"kind": "manual"},
                  "conditions": {"match_mode": "all", "items": []}, "expires_at": None,
                  "actions": [{"kind": "record", "message": "Private executor message /data/model"}]}
    status, body = request(service, RULES, "POST", definition)
    assert status == 200, body
    rule_id = body["rule"]["rule_id"]
    for action in ("preview", "enable"):
        assert request(service, f"{RULES}/{rule_id}/{action}", "POST", {"revision": 1})[0] == 200
    status, body = request(service, f"{RULES}/{rule_id}/run", "POST", {"revision": 1, "trigger_id": "jobs-test"})
    assert status == 200, body
    return rule_id


def seed_large_history(service, count=30000):
    """Fixture data based on one real execution; this is not count independent executions."""
    service.stop()
    path = service.root / "admin-console.rules.json"
    state = json.loads(path.read_text(encoding="utf-8"))
    template = state["runs"][0]
    state["runs"] = [dict(template, run_id=f"fixture-run-{index}", trigger_id=f"fixture-{index}") for index in range(count)]
    path.write_text(json.dumps(state, separators=(",", ":")), encoding="utf-8")
    service.start()
    service.wait_ready()


class ProductJobsEntrypoint(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.binary = service_binary("HARBOR_TEST_N2_SERVICE_BIN")

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="product-jobs-http-")
        self.addCleanup(self.temp.cleanup)
        self.service = ProductProcess(self.binary, Path(self.temp.name))
        self.addCleanup(self.service.stop)
        self.service.env["HARBOR_BEACON_STARTUP_PROFILE"] = "n2"
        self.service.start()
        self.service.wait_ready()

    def create(self, key="export-1"):
        status, body = request(self.service, method="POST", body={"job_type": "rules_history_export", "idempotency_key": key})
        self.assertIn(status, (200, 202), body)
        return body["job"]

    def wait_status(self, job, expected):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            status, body = request(self.service, JOBS + "/" + job["job_id"])
            self.assertEqual(status, 200, body)
            latest = body["job"]
            if latest["status"] in expected:
                return latest
            self.assertIn(latest["status"], ("queued", "running"), latest)
            time.sleep(0.01)
        self.fail(f"Task did not reach {expected}")

    def test_real_export_download_idempotency_and_restart(self):
        create_rule_history(self.service)
        job = self.create()
        completed = self.wait_status(job, {"succeeded"})
        self.assertEqual(completed["result"]["record_count"], 1)
        status, exported = request(self.service, JOBS + "/" + job["job_id"] + "/result")
        self.assertEqual(status, 200)
        self.assertEqual(len(exported["executions"]), 1)
        self.assertNotIn("Private executor", json.dumps(exported))
        self.assertEqual(self.create()["job_id"], job["job_id"])
        self.service.stop()
        self.service.start()
        self.service.wait_ready()
        self.assertEqual(request(self.service, JOBS + "/" + job["job_id"])[1]["job"], completed)

    def test_signed_actor_isolation_and_forged_actor_rejection(self):
        self.assertIn(self.service.request(JOBS)[0], (401, 503))
        job = self.create()
        self.wait_status(job, {"succeeded"})
        path = JOBS + "/" + job["job_id"]
        self.assertEqual(request(self.service, actor="second-admin")[1]["jobs"], [])
        for suffix, method in (("", "GET"), ("/result", "GET"), ("/cancel", "POST")):
            self.assertEqual(request(self.service, path + suffix, method, actor="second-admin")[0], 404)
        status, _ = request(self.service, method="POST", body={"job_type": "rules_history_export",
            "idempotency_key": "spoof", "actor_id": "second-admin"})
        self.assertEqual(status, 422)
        self.assertEqual(request(self.service, role="TRUSTED_LAN", method="POST", body={
            "job_type": "rules_history_export", "idempotency_key": "member"})[0], 403)

    def test_concurrent_replay_starts_one_job(self):
        with ThreadPoolExecutor(max_workers=6) as pool:
            jobs = list(pool.map(lambda _: self.create("one-request"), range(6)))
        self.assertEqual(len({job["job_id"] for job in jobs}), 1)
        terminal = self.wait_status(jobs[0], {"succeeded"})
        self.assertEqual([event["action"] for event in terminal["events"]].count("started"), 1)

    def test_running_cancel_and_new_task(self):
        create_rule_history(self.service)
        seed_large_history(self.service)
        job = self.create()
        self.wait_status(job, {"running"})
        status, pending = request(self.service, JOBS + "/" + job["job_id"] + "/cancel", "POST", {})
        self.assertEqual(status, 200, pending)
        self.assertTrue(pending["job"]["cancel_requested"])
        self.assertEqual(self.wait_status(job, {"cancelled"})["result"], None)
        status, retry = request(self.service, JOBS + "/" + job["job_id"] + "/retry", "POST", {"idempotency_key": "retry"})
        self.assertEqual(status, 202, retry)
        self.assertEqual(self.wait_status(retry["job"], {"succeeded"})["result"]["record_count"], 30000)

    def test_killed_export_recovers_as_interrupted_and_retries(self):
        create_rule_history(self.service)
        seed_large_history(self.service)
        job = self.create()
        self.wait_status(job, {"running"})
        self.service.stop()
        self.service.start()
        self.service.wait_ready()
        recovered = self.wait_status(job, {"interrupted"})
        self.assertIsNone(recovered["result"])
        self.assertTrue(recovered["can_retry"])
        self.assertEqual(request(self.service, JOBS + "/" + job["job_id"] + "/result")[0], 409)

    def test_storage_failure_isolated_and_export_error_redacted(self):
        root = self.service.root / "admin-console.product-jobs"
        root.mkdir()
        state = root / "state.json"
        state.write_text("{broken", encoding="ascii")
        status, body = request(self.service)
        self.assertEqual(status, 503)
        self.assertNotIn(str(root), json.dumps(body))
        self.assertEqual(state.read_text(), "{broken")
        self.assertEqual(request(self.service, RULES)[0], 200)
        self.assertEqual(self.service.request("/healthz")[0], 200)

    def test_failed_export_can_retry_after_source_is_repaired(self):
        self.service.stop()
        source = self.service.root / "admin-console.rules.json"
        source.write_text("{invalid", encoding="ascii")
        self.service.start()
        self.service.wait_ready()
        job = self.create()
        failed = self.wait_status(job, {"failed"})
        self.assertEqual(failed["error_code"], "EXPORT_FAILED")
        self.assertIsNone(failed["result"])
        self.service.stop()
        source.write_text('{"schema_version":1,"rules":{},"runs":[]}', encoding="ascii")
        self.service.start()
        self.service.wait_ready()
        status, retry = request(self.service, JOBS + "/" + job["job_id"] + "/retry", "POST", {"idempotency_key": "repair-retry"})
        self.assertEqual(status, 202, retry)
        self.assertEqual(self.wait_status(retry["job"], {"succeeded"})["result"]["record_count"], 0)


if __name__ == "__main__":
    unittest.main(verbosity=2)
