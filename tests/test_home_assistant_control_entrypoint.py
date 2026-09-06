"""Real N2 Beacon HTTP acceptance with a loopback-only HarborLink fixture.

The fixture represents the southbound contract, not Home Assistant hardware.
Set HARBOR_TEST_N2_SERVICE_BIN to the actual compiled product executable.
"""

from __future__ import annotations

from copy import deepcopy
from datetime import datetime, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
from pathlib import Path
import socket
import tempfile
import threading
import time
import unittest
from urllib.error import HTTPError
from urllib.parse import urlsplit
from urllib.request import Request

from test_n2_capability_startup import ProductProcess, RULES, service_binary
from test_product_jobs_entrypoint import signed_headers


HA = "/api/harbor-beacon/home-assistant"
FIXTURE_TOKEN = "local-link-test-token"


def request(service, path, method="GET", body=None, role="FULL_ADMIN", timeout=15):
    headers = signed_headers(method, path, actor="ha-test-admin", role=role)
    headers["Content-Type"] = "application/json"
    req = Request(service.base + path, data=None if body is None else json.dumps(body).encode(),
                  headers=headers, method=method)
    try:
        response = service.http.open(req, timeout=timeout)
    except HTTPError as error:
        response = error
    with response:
        raw = response.read()
        return response.status, json.loads(raw) if raw else None


def action(entity_id="light.desk", service="turn_on", fields=None):
    return {"entity_id": entity_id, "domain": entity_id.split(".", 1)[0],
            "service": service, "fields": fields or {}}


def definition(entity_id="light.desk", service="turn_on", fields=None):
    return {"name": "Fixture desk light", "trigger": {"kind": "manual"},
            "conditions": {"match_mode": "all", "items": []}, "expires_at": None,
            "actions": [dict(action(entity_id, service, fields), kind="home_assistant")]}


class HarborLinkFixture:
    """Authenticated contract fixture; every accepted service POST is counted."""

    def __init__(self):
        self.lock = threading.RLock()
        self.release = threading.Event()
        self.requests = []
        self.errors = []
        self.enabled = False
        self.exposed_domains = ["light", "switch"]
        self.entities = {}
        self.modes = {}
        self.services = {domain: ["turn_on", "turn_off", "toggle"] for domain in self.exposed_domains}
        for entity_id, name, state, mode in (
            ("light.desk", "Fixture desk light", "off", "update"),
            ("light.accepted", "Fixture accepted light", "off", "accepted"),
            ("light.rejected", "Fixture rejected light", "off", "rejected"),
            ("light.unknown", "Fixture uncertain light", "off", "drop"),
            ("light.unavailable", "Fixture unavailable light", "unavailable", "update"),
            ("switch.fan", "Fixture desk fan", "off", "update"),
        ):
            self.entities[entity_id] = {
                "entity_id": entity_id, "domain": entity_id.split(".", 1)[0],
                "display_name": name, "state": state, "area_id": "fixture_office",
                "last_changed": datetime.now(timezone.utc).isoformat(), "attributes": {},
            }
            self.modes[entity_id] = mode
        fixture = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_):
                pass

            def send(self, status, body):
                encoded = json.dumps(body).encode()
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(encoded)))
                self.end_headers()
                try:
                    self.wfile.write(encoded)
                except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                    pass

            def handle_request(self):
                path = urlsplit(self.path).path
                body = None
                if self.headers.get("Content-Length"):
                    body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                contract = self.headers.get("X-HarborLink-Contract-Version")
                authorized = self.headers.get("Authorization") == "Bearer " + FIXTURE_TOKEN
                request_id = self.headers.get("X-Request-Id")
                with fixture.lock:
                    fixture.requests.append({"method": self.command, "path": path,
                                             "body": body, "request_id": request_id})
                    if not authorized or contract != "1.0":
                        fixture.errors.append("Missing HarborLink authentication or contract header")
                        return self.send(401, {"error": "Fixture contract rejected"})
                    if self.command in {"POST", "PUT"} and not request_id:
                        fixture.errors.append("Missing mutation request ID")
                        return self.send(400, {"error": "Fixture request ID required"})
                    if path == "/v1/home-assistant":
                        if self.command == "PUT":
                            fixture.enabled = body["enabled"]
                            fixture.exposed_domains = body.get("exposedDomains") or ["light", "switch"]
                        return self.send(200, fixture.status())
                    if self.command == "POST" and path == "/v1/home-assistant/test":
                        return self.send(200, {"ok": True, "status": "connected",
                                               "location_name": "Loopback fixture", "version": "fixture"})
                    if self.command == "GET" and path == "/v1/home-assistant/entities":
                        return self.send(200, list(fixture.entities.values()))
                    if self.command == "GET" and path == "/v1/home-assistant/services":
                        return self.send(200, [{"domain": domain, "services": [
                            {"service": service, "name": service.replace("_", " ").title(), "fields": {}}
                            for service in services]} for domain, services in fixture.services.items()])
                    if self.command == "POST" and path.startswith("/v1/home-assistant/services/"):
                        domain, service = path.rsplit("/", 2)[-2:]
                        entity_id = body["entity_id"]
                        mode = fixture.modes.get(entity_id, "rejected")
                        result = {"domain": domain, "service": service, "entity_id": entity_id,
                                  "ok": mode != "rejected", "changed_entity_count": 0}
                        if mode == "update":
                            current = fixture.entities[entity_id]["state"]
                            state = "on" if service == "turn_on" or (service == "toggle" and current != "on") else "off"
                            fixture.entities[entity_id].update(
                                state=state, last_changed=datetime.now(timezone.utc).isoformat())
                            result["changed_entity_count"] = 1
                        if mode == "mismatch":
                            result["entity_id"] = "light.someone_else"
                    else:
                        return self.send(404, {"error": "Unimplemented loopback fixture route"})
                if mode == "timeout":
                    fixture.release.wait(9)
                if mode in {"drop", "timeout"}:
                    self.close_connection = True
                    try:
                        self.connection.shutdown(socket.SHUT_RDWR)
                    except OSError:
                        pass
                    return
                if mode == "invalid":
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", "7")
                    self.end_headers()
                    self.wfile.write(b"invalid")
                    return
                return self.send(200, result)

            do_GET = do_POST = do_PUT = handle_request

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.server.daemon_threads = True
        self.base = "http://127.0.0.1:" + str(self.server.server_port)
        self.worker = threading.Thread(target=self.server.serve_forever, daemon=True)

    def start(self):
        self.worker.start()

    def stop(self):
        self.release.set()
        self.server.shutdown()
        self.server.server_close()
        self.worker.join(timeout=2)

    def status(self):
        return {"enabled": self.enabled, "configured": self.enabled,
                "baseUrlConfigured": self.enabled, "tokenConfigured": self.enabled,
                "allowedEntityCount": len(self.entities), "allowedCameraCount": 0,
                "allowedEntities": list(self.entities), "allowedCameras": [],
                "cameraEntityBindings": {}, "exposedDomains": self.exposed_domains}

    def control(self, entity_id="light.desk", state=None, mode=None, services=None):
        with self.lock:
            if entity_id not in self.entities:
                raise ValueError("Unknown fixture entity")
            if state is not None:
                self.entities[entity_id]["state"] = state
            if mode is not None:
                if mode not in {"update", "accepted", "rejected", "drop", "timeout", "invalid", "mismatch"}:
                    raise ValueError("Unknown fixture mode")
                self.modes[entity_id] = mode
            if services is not None:
                self.services[self.entities[entity_id]["domain"]] = services

    def service_posts(self):
        with self.lock:
            return deepcopy([entry for entry in self.requests if entry["method"] == "POST"
                             and entry["path"].startswith("/v1/home-assistant/services/")])

    def snapshot(self):
        with self.lock:
            return {"fixture": True, "live_hardware": False, "entities": deepcopy(self.entities),
                    "modes": dict(self.modes), "services": deepcopy(self.services),
                    "service_posts": self.service_posts(), "contract_errors": list(self.errors)}


def configure(service, fixture):
    status, body = request(service, HA + "/config", "PUT", {
        "enabled": True, "base_url": fixture.base, "access_token": "fixture-ha-token",
        "exposed_domains": ["light", "switch"], "allowed_entities": list(fixture.entities)})
    if status != 200:
        raise AssertionError(body)
    return body


class HomeAssistantControlEntrypoint(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.binary = service_binary("HARBOR_TEST_N2_SERVICE_BIN")

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="ha-control-http-")
        self.addCleanup(self.temp.cleanup)
        self.fixture = HarborLinkFixture()
        self.fixture.start()
        self.addCleanup(self.fixture.stop)
        self.service = ProductProcess(self.binary, Path(self.temp.name))
        self.addCleanup(self.service.stop)
        self.service.env.update(HARBOR_BEACON_STARTUP_PROFILE="n2", HARBORLINK_MEDIA_API_URL=self.fixture.base)
        self.service.start()
        self.service.wait_ready()
        configure(self.service, self.fixture)

    def tearDown(self):
        self.assertEqual(self.fixture.errors, [])

    def manual(self, body=None, suffix="service-action"):
        status, response = request(self.service, HA + "/" + suffix, "POST", body or action())
        self.assertEqual(status, 200, response)
        return response

    def create_rule(self):
        status, response = request(self.service, RULES, "POST", definition())
        self.assertEqual(status, 200, response)
        return RULES + "/" + response["rule"]["rule_id"]

    def enable_rule(self):
        path = self.create_rule()
        for suffix in ("preview", "enable"):
            status, response = request(self.service, path + "/" + suffix, "POST", {"revision": 1})
            self.assertEqual(status, 200, response)
        return path

    def run_rule(self, path, trigger="fixture-trigger"):
        status, response = request(self.service, path + "/run", "POST", {"revision": 1, "trigger_id": trigger})
        self.assertEqual(status, 200, response)
        return response["run"]

    def test_config_test_sync_use_real_beacon_and_authenticated_harborlink(self):
        status, response = request(self.service, HA + "/test", "POST", {})
        self.assertEqual(status, 200, response)
        self.assertTrue(response["test"]["ok"])
        status, response = request(self.service, HA + "/sync", "POST", {})
        self.assertEqual(status, 200, response)
        self.assertEqual(len(response["entities"]), 6)
        self.assertEqual(response["status"]["entity_count"], 6)
        self.assertEqual(response["status"]["service_count"], 6)
        self.assertTrue(response["status"]["managed_by_harborlink"])
        self.assertNotIn("fixture-ha-token", json.dumps(response))
        state = json.loads((self.service.root / "admin-console.json").read_text(encoding="utf-8"))
        self.assertNotIn("fixture-ha-token", json.dumps(state))
        self.assertEqual(self.fixture.service_posts(), [])

    def test_manual_rechecks_entity_and_service_after_successful_sync(self):
        self.assertEqual(request(self.service, HA + "/sync", "POST", {})[0], 200)
        for state in ("unavailable", "unknown"):
            with self.subTest(state=state):
                self.fixture.control(state=state)
                response = self.manual()
                self.assertEqual(response["status"], "blocked", response)
                self.assertFalse(response["allowed"])
                self.assertFalse(response["executed"])
        self.fixture.control(state="off", services=["turn_off"])
        self.assertEqual(self.manual()["status"], "blocked")
        self.assertEqual(self.fixture.service_posts(), [])

    def test_manual_target_overrides_and_non_admin_are_rejected_before_dispatch(self):
        for fields in ({"entity_id": "light.someone_else"}, {"target": {"entity_id": "light.someone_else"}}):
            with self.subTest(fields=fields):
                self.assertEqual(self.manual(action(fields=fields))["status"], "blocked")
        body = dict(action(), entity_id="switch.fan")
        self.assertEqual(self.manual(body)["status"], "blocked")
        status, response = request(self.service, HA + "/service-action", "POST", action(), role="TRUSTED_LAN")
        self.assertEqual(status, 403, {"response": response, "service_posts": self.fixture.service_posts()})
        self.assertEqual(self.fixture.service_posts(), [])

    def test_manual_ack_is_distinct_from_observed_state_and_rejection(self):
        self.fixture.control(mode="accepted")
        accepted = self.manual()
        self.assertEqual(accepted["status"], "succeeded", accepted)
        self.assertTrue(accepted["executed"])
        self.assertTrue(accepted["result"]["ok"])
        self.assertRegex(accepted["message"].lower(), r"not (?:yet )?confirmed")
        self.assertEqual(request(self.service, HA + "/entities")[1]["entities"][0]["state"], "off")
        self.fixture.control(mode="rejected")
        rejected = self.manual()
        self.assertEqual(rejected["status"], "failed", rejected)
        self.assertTrue(rejected["executed"])
        self.assertFalse(rejected["result"]["ok"])
        self.assertEqual(len(self.fixture.service_posts()), 2)

    def test_non_admin_ha_and_rules_mutations_cannot_fall_back_to_local_owner(self):
        path = self.enable_rule()
        cases = [
            (HA + "/config", "PUT", {"enabled": False}),
            (HA + "/test", "POST", {}),
            (HA + "/sync", "POST", {}),
            (HA + "/service-smoke", "POST", action()),
            (RULES, "POST", definition()),
            (path, "PUT", dict(definition(), revision=1)),
            (path + "/preview", "POST", {"revision": 1}),
            (path + "/enable", "POST", {"revision": 1}),
            (path + "/pause", "POST", {"revision": 1}),
            (path + "/delete", "POST", {"revision": 1}),
            (path + "/run", "POST", {"revision": 1, "trigger_id": "unauthorized"}),
            (RULES + "/events", "POST", {"event_type": "fixture.event", "event_id": "unauthorized"}),
        ]
        for route, method, body in cases:
            with self.subTest(route=route, method=method):
                status, response = request(self.service, route, method, body, role="TRUSTED_LAN")
                self.assertEqual(status, 403, response)
        self.assertEqual(self.fixture.service_posts(), [])

    def test_manual_ambiguous_results_are_unknown_without_automatic_retry(self):
        for mode in ("drop", "invalid", "mismatch", "timeout"):
            with self.subTest(mode=mode):
                self.fixture.control(mode=mode)
                before = len(self.fixture.service_posts())
                response = self.manual()
                self.assertEqual(response["status"], "unknown", response)
                self.assertTrue(response["allowed"])
                self.assertTrue(response["executed"])
                self.assertEqual(len(self.fixture.service_posts()), before + 1)
        time.sleep(0.2)
        self.assertEqual(len(self.fixture.service_posts()), 4)

    def test_smoke_uses_the_same_outcome_and_preflight_rules(self):
        for mode, expected in (("accepted", "succeeded"), ("rejected", "failed"), ("drop", "unknown")):
            with self.subTest(mode=mode):
                self.fixture.control(mode=mode)
                self.assertEqual(self.manual(suffix="service-smoke")["status"], expected)
        self.fixture.control(state="unavailable")
        blocked = self.manual(suffix="service-smoke")
        self.assertEqual(blocked["status"], "blocked")
        self.assertFalse(blocked["executed"])
        self.assertEqual(len(self.fixture.service_posts()), 3)

    def test_rules_fresh_preflight_blocks_unavailable_action_after_enable(self):
        path = self.enable_rule()
        self.assertEqual(self.fixture.service_posts(), [])
        self.fixture.control(state="unavailable")
        run = self.run_rule(path)
        self.assertEqual(run["status"], "failed", run)
        self.assertEqual(run["actions"][0]["status"], "failed")
        self.assertEqual(self.fixture.service_posts(), [])
        self.assertEqual(request(self.service, path + "/runs")[1]["runs"], [run])

    def test_rules_preview_blocks_offline_entity_without_recording_a_valid_preview(self):
        path = self.create_rule()
        self.fixture.control(state="unavailable")
        status, response = request(self.service, path + "/preview", "POST", {"revision": 1})
        self.assertEqual(status, 422, response)
        self.fixture.control(state="off")
        status, response = request(self.service, path + "/enable", "POST", {"revision": 1})
        self.assertEqual(status, 409, response)
        self.assertEqual(self.fixture.service_posts(), [])

    def test_rules_enable_rechecks_services_after_a_valid_preview(self):
        path = self.create_rule()
        status, response = request(self.service, path + "/preview", "POST", {"revision": 1})
        self.assertEqual(status, 200, response)
        self.fixture.control(services=["turn_off"])
        status, response = request(self.service, path + "/enable", "POST", {"revision": 1})
        self.assertEqual(status, 422, response)
        rule = next(rule for rule in request(self.service, RULES)[1]["rules"] if path.endswith(rule["rule_id"]))
        self.assertEqual(rule["status"], "draft")
        self.assertEqual(self.fixture.service_posts(), [])

    def test_rules_fresh_service_preflight_blocks_removed_service_after_enable(self):
        path = self.enable_rule()
        self.fixture.control(services=["turn_off"])
        self.assertEqual(self.run_rule(path)["status"], "failed")
        self.assertEqual(self.fixture.service_posts(), [])

    def test_rules_outcomes_history_and_replay_do_not_dispatch_twice(self):
        path = self.enable_rule()
        for mode, status, action_status in (("accepted", "completed", "succeeded"),
                                            ("rejected", "failed", "failed"),
                                            ("drop", "unknown", "unknown"),
                                            ("mismatch", "unknown", "unknown")):
            with self.subTest(mode=mode):
                self.fixture.control(mode=mode)
                before = len(self.fixture.service_posts())
                run = self.run_rule(path, mode)
                self.assertEqual(run["status"], status, run)
                self.assertEqual(run["actions"][0]["status"], action_status, run)
                self.assertEqual(self.run_rule(path, mode), run)
                self.assertEqual(len(self.fixture.service_posts()), before + 1)
                if mode == "accepted":
                    self.assertRegex(run["actions"][0]["message"].lower(), r"not (?:yet )?confirmed")
                    self.assertEqual(self.fixture.snapshot()["entities"]["light.desk"]["state"], "off")
        history = request(self.service, path + "/runs")[1]["runs"]
        self.assertEqual(len(history), 4)
        self.service.stop()
        self.service.start()
        self.service.wait_ready()
        self.assertEqual(request(self.service, path + "/runs")[1]["runs"], history)
        self.assertEqual(self.run_rule(path, "drop")["status"], "unknown")
        self.assertEqual(len(self.fixture.service_posts()), 4)


if __name__ == "__main__":
    unittest.main(verbosity=2)
