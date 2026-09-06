"""Loopback UI preview: real Beacon, disposable session, deterministic HA fixture.

The southbound fixture controls no physical device. All state is created in a
fresh temporary directory, and every external request stays on loopback.
"""

import argparse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import mimetypes
from pathlib import Path
import tempfile
from urllib.error import HTTPError, URLError
from urllib.parse import unquote, urlsplit
from urllib.request import Request

from test_home_assistant_control_entrypoint import HA, HarborLinkFixture, configure, request
from test_n2_capability_startup import ProductProcess
from test_product_jobs_entrypoint import signed_headers


CSRF = "home-assistant-local-fixture-csrf"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--webui", type=Path, required=True)
    parser.add_argument("--port", type=int, default=18422)
    args = parser.parse_args()
    dist = args.webui.resolve()
    if not dist.joinpath("index.html").is_file():
        raise RuntimeError("The production WebUI index is required")
    temporary = tempfile.TemporaryDirectory(prefix="home-assistant-preview-")
    fixture = HarborLinkFixture()
    service = ProductProcess(args.binary.resolve(), Path(temporary.name))
    service.env.update(HARBOR_BEACON_STARTUP_PROFILE="n2", HARBORLINK_MEDIA_API_URL=fixture.base)

    class Handler(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass

        def send(self, status, data, content_type="application/json", extra=None):
            self.send_response(status)
            self.send_header("Content-Type", content_type)
            self.send_header("Content-Length", str(len(data)))
            self.send_header("Cache-Control", "no-store")
            for name, value in (extra or {}).items():
                self.send_header(name, value)
            self.end_headers()
            try:
                self.wfile.write(data)
            except (BrokenPipeError, ConnectionResetError, ConnectionAbortedError):
                pass

        def handle_request(self):
            origin = f"http://127.0.0.1:{args.port}"
            if self.headers.get("Host") != f"127.0.0.1:{args.port}":
                return self.send(403, b'{}')
            path = urlsplit(self.path).path
            if self.command != "GET":
                if self.headers.get("X-HarborOS-CSRF-Token") != CSRF or self.headers.get("Origin") not in (None, origin):
                    return self.send(403, b'{"error":"Local preview request rejected"}')
            try:
                length = int(self.headers.get("Content-Length", "0"))
            except ValueError:
                return self.send(400, b'{}')
            if length < 0 or length > 64 * 1024:
                return self.send(413, b'{}')
            body = self.rfile.read(length) if length else None
            if path == "/api/harboros/v1/session" and self.command == "GET":
                return self.send(200, json.dumps({"data": {"authenticated": True,
                    "principal": "ha-loopback-fixture", "is_admin": True, "mode": "session"}}).encode(),
                    extra={"X-HarborOS-CSRF-Token": CSRF})
            if path == "/api/harboros/v1/bootstrap" and self.command == "GET":
                return self.send(200, b'{"data":{"required":false}}')
            if path == "/__fixture/state" and self.command == "GET":
                return self.send(200, json.dumps(fixture.snapshot()).encode())
            if path == "/__fixture/control" and self.command == "POST":
                try:
                    payload = json.loads(body or b"{}")
                    fixture.control(**payload)
                except (ValueError, TypeError):
                    return self.send(422, b'{"error":"Invalid fixture control"}')
                return self.send(200, json.dumps(fixture.snapshot()).encode())
            if path.startswith("/api/harbor-beacon/"):
                headers = signed_headers(self.command, self.path, actor="ha-test-admin")
                headers["Content-Type"] = self.headers.get("Content-Type", "application/json")
                req = Request(service.base + self.path, data=body, method=self.command, headers=headers)
                try:
                    response = service.http.open(req, timeout=30)
                except HTTPError as error:
                    response = error
                except (URLError, OSError):
                    return self.send(503, b'{"error":"Local product is unavailable"}')
                with response:
                    extra = {}
                    if response.headers.get("Content-Disposition"):
                        extra["Content-Disposition"] = response.headers["Content-Disposition"]
                    return self.send(response.status, response.read(), response.headers.get("Content-Type", "application/json"), extra)
            if path.startswith("/api/") or self.command != "GET":
                return self.send(404, b'{"error":"Not available in local preview"}')
            relative = unquote(path.removeprefix("/ui/")).lstrip("/")
            file = (dist / relative).resolve()
            if not file.is_relative_to(dist):
                return self.send(404, b'{}')
            if not file.is_file():
                file = dist / "index.html"
            return self.send(200, file.read_bytes(), mimetypes.guess_type(file.name)[0] or "application/octet-stream")

        do_GET = do_POST = do_PUT = do_DELETE = handle_request

    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    try:
        fixture.start()
        service.start()
        service.wait_ready()
        configure(service, fixture)
        for suffix in ("test", "sync"):
            status, body = request(service, HA + "/" + suffix, "POST", {})
            if status != 200:
                raise RuntimeError(body)
        print(json.dumps({"url": f"http://127.0.0.1:{args.port}/ui/harbor-assistant?tab=home-assistant",
                          "fixture_session": True, "fixture_southbound": True, "live_hardware": False,
                          "product_pid": service.process.pid, "fixture_state": "/__fixture/state",
                          "fixture_control": "/__fixture/control"}), flush=True)
        server.serve_forever()
    finally:
        server.server_close()
        service.stop()
        fixture.stop()
        temporary.cleanup()


if __name__ == "__main__":
    main()
