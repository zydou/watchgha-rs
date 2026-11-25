"""Mock GitHub Actions API for parity tests.

A tiny stdlib-only HTTP server that serves canned workflow-run/job payloads and
logs every request it receives. The parity harness points the binary under test
at this server via GITHUB_API_URL / GITHUB_SERVER_URL, so the full CLI path
(repo resolution -> URL building -> fetch -> render) runs deterministically
end to end, and the request log doubles as an oracle for URL construction.

Timestamps in a fixture are stored as integer offsets in seconds relative to
"now" (negative = in the past). They are resolved to real ISO timestamps at
request time. This keeps the runs inside the original's 7-day window forever,
so fixtures never age out, while every binary under test sees byte-identical
payloads within a single run of the harness.

Usage:
    python mock_gha.py --port 8765 --spec spec.json --log requests.jsonl

The spec is a JSON object:
    {
      "routes": {
        "/repos/o/r/actions/runs": {"workflow_runs": [ ... ]},
        "/repos/o/r/actions/runs/1/jobs": {"jobs": [ ... ]}
      }
    }
Any string value equal to an integer (e.g. "-120") inside the routed JSON is
treated as a now-offset and rewritten to an ISO timestamp before serving. The
literal token "__API__" inside any value is rewritten to this server's own base
URL ("http://127.0.0.1:<port>"), so fixtures can reference their own jobs_url
without knowing the port the harness will pick.
"""

from __future__ import annotations

import argparse
import datetime
import json
import re
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import urlparse

# An entire string that is just an integer, e.g. "-120" or "0".
_OFFSET_RE = re.compile(r"^-?\d+$")


def _resolve_offsets(node, api_base):
    """Recursively rewrite now-offset strings and __API__ tokens."""
    if isinstance(node, dict):
        return {k: _resolve_offsets(v, api_base) for k, v in node.items()}
    if isinstance(node, list):
        return [_resolve_offsets(v, api_base) for v in node]
    if isinstance(node, str):
        node = node.replace("__API__", api_base)
        if _OFFSET_RE.match(node):
            when = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(
                seconds=int(node)
            )
            # GitHub emits e.g. 2026-01-18T16:55:30Z.
            return when.strftime("%Y-%m-%dT%H:%M:%SZ")
        return node
    return node


class MockGhaHandler(BaseHTTPRequestHandler):
    server_version = "MockGHA/1.0"

    def log_message(self, fmt, *args):  # silence default stderr logging
        return

    def _record(self, method):
        parsed = urlparse(self.path)
        entry = {
            "method": method,
            "path": parsed.path,
            "query": parsed.query,
        }
        with self.server.log_lock:
            with open(self.server.log_path, "a", encoding="utf-8") as fh:
                fh.write(json.dumps(entry) + "\n")

    def _serve(self, method):
        self._record(method)
        parsed = urlparse(self.path)
        body = self.server.routes.get(parsed.path)
        if body is None:
            payload = {"message": f"mock: no route for {parsed.path}"}
            self._send(404, payload)
            return
        resolved = _resolve_offsets(body, self.server.api_base)
        self._send(200, resolved)

    def _send(self, status, payload):
        data = json.dumps(payload).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):  # noqa: N802
        self._serve("GET")


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--spec", required=True, help="route spec JSON file")
    ap.add_argument("--log", required=True, help="request log JSONL file")
    args = ap.parse_args(argv)

    with open(args.spec, encoding="utf-8") as fh:
        spec = json.load(fh)

    httpd = ThreadingHTTPServer(("127.0.0.1", args.port), MockGhaHandler)
    httpd.routes = spec.get("routes", {})
    httpd.api_base = f"http://127.0.0.1:{args.port}"
    httpd.log_path = args.log
    httpd.log_lock = threading.Lock()
    # Truncate the log so each harness run starts clean.
    open(args.log, "w").close()

    sys.stderr.write(f"mock listening on 127.0.0.1:{args.port}\n")
    sys.stderr.flush()
    httpd.serve_forever()


if __name__ == "__main__":
    main()
