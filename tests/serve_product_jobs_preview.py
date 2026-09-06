"""Loopback-only UI preview with the real Beacon and a disposable test session.

Never point this helper at installed device data. It uses test_n2_capability_startup
credentials and creates a fresh isolated state directory for each invocation.
"""

import argparse
import json
import mimetypes
from pathlib import Path
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import tempfile
from urllib.error import HTTPError, URLError
from urllib.parse import unquote, urlsplit
from urllib.request import Request

from test_n2_capability_startup import ProductProcess
from test_product_jobs_entrypoint import create_rule_history, seed_large_history, signed_headers

CSRF = "product-jobs-local-preview-csrf"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--webui", type=Path, required=True)
    parser.add_argument("--port", type=int, default=18421)
    parser.add_argument("--seed-history", type=int, default=30000)
    args = parser.parse_args()
    dist = args.webui.resolve()
    if not dist.joinpath("index.html").is_file():
        raise RuntimeError("The production WebUI index is required")
    temporary = tempfile.TemporaryDirectory(prefix="product-jobs-preview-")
    service = ProductProcess(args.binary.resolve(), Path(temporary.name))
    service.env["HARBOR_BEACON_STARTUP_PROFILE"] = "n2"

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
            self.wfile.write(data)

        def handle_request(self):
            origin = f"http://127.0.0.1:{args.port}"
            if self.headers.get("Host") != f"127.0.0.1:{args.port}":
                return self.send(403, b'{}')
            path = urlsplit(self.path).path
            if self.command not in {"GET", "HEAD"}:
                if self.headers.get("X-HarborOS-CSRF-Token") != CSRF or self.headers.get("Origin") not in (None, origin):
                    return self.send(403, b'{"error":"Local preview request rejected"}')
            if path == "/api/harboros/v1/session":
                return self.send(200, json.dumps({"data": {"authenticated": True, "principal": "local-preview",
                    "is_admin": True, "mode": "session"}}).encode(), extra={"X-HarborOS-CSRF-Token": CSRF})
            if path == "/api/harboros/v1/bootstrap":
                return self.send(200, b'{"data":{"required":false}}')
            if path.startswith("/api/harbor-beacon/"):
                headers = signed_headers(self.command, self.path)
                headers["Content-Type"] = self.headers.get("Content-Type", "application/json")
                length = int(self.headers.get("Content-Length", "0"))
                if length > 64 * 1024:
                    return self.send(413, b'{}')
                body = self.rfile.read(length) if length else None
                req = Request(service.base + self.path, data=body, method=self.command, headers=headers)
                try:
                    response = service.http.open(req, timeout=15)
                except HTTPError as error:
                    response = error
                except (URLError, OSError):
                    return self.send(503, b'{"error":"Local product is unavailable"}')
                with response:
                    extra = {}
                    if response.headers.get("Content-Disposition"):
                        extra["Content-Disposition"] = response.headers["Content-Disposition"]
                    return self.send(response.status, response.read(), response.headers.get("Content-Type", "application/json"), extra)
            if path.startswith("/api/"):
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
        service.start()
        service.wait_ready()
        create_rule_history(service)
        if args.seed_history > 1:
            seed_large_history(service, min(args.seed_history, 50000))
        print(json.dumps({"url": f"http://127.0.0.1:{args.port}/ui/harbor-assistant?tab=settings&section=tasks",
                          "fixture_session": True, "history_fixture_rows": args.seed_history,
                          "product_pid": service.process.pid}), flush=True)
        server.serve_forever()
    finally:
        server.server_close()
        service.stop()
        temporary.cleanup()


if __name__ == "__main__":
    main()
