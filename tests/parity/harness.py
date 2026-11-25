"""Parity harness for the watchgha Rust port.

Compares the Rust binary against the original Python `watchgha` (the "oracle")
on a corpus of CLI scenarios. Fixtures are captured from the oracle once and
committed; CI replays them in check mode without the oracle or upstream.

Modes:
    capture    run the oracle, write normalized fixtures
    check      run the Rust binary, diff against fixtures (CI gate)
    selfcheck  check mode against the oracle itself (must be 100% green
               before any porting; proves the harness + normalization are
               sound)

Usage:
    python harness.py --mode capture  --oracle .oracle-venv/bin/watchgha
    python harness.py --mode check    --rust   target/release/watchgha
    python harness.py --mode selfcheck --oracle .oracle-venv/bin/watchgha
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).parent
SPECS = HERE / "fixtures" / "specs"
EXPECTED = HERE / "fixtures" / "expected"
CASES_FILE = HERE / "cases.json"

# Fixed determinism env for every invocation. COLUMNS pins rich's wrap width;
# TZ pins nice_time's local zone; GITHUB_TOKEN forces bearer auth so no ~/.netrc
# is read; HOME is a temp dir for full hermeticity. NO_PROXY keeps requests to
# the loopback mock server from being hijacked by an ambient HTTP proxy.
BASE_ENV = {
    "COLUMNS": "100",
    "TZ": "UTC",
    "GITHUB_TOKEN": "test-token",
    "SAVE_DATA": "0",
    "TERM": "dumb",
    "NO_PROXY": "127.0.0.1,localhost",
    "no_proxy": "127.0.0.1,localhost",
}

# The rendered header ends with "<sha12>  @<nice_time>". nice_time depends on
# the wall clock and local zone, so it is provably nondeterministic across
# capture and check runs. Normalize the whole "@..." token to "@NOW".
NICE_TIME_RE = re.compile(
    r"@(?:\d{4}-\d{2}-\d{2}|\d{2}-\d{2})?\s?\d{1,2}:\d{2}(?:am|pm)"
)


def normalize(text: str) -> str:
    return NICE_TIME_RE.sub("@NOW", text)


def normalize_requests(reqs):
    """Sort requests so comparison is order-independent.

    The original fetches each run's jobs concurrently (a trio nursery), so the
    order in which the /jobs requests arrive is genuinely nondeterministic.
    Sorting by (path, query) compares the *set* of requests, which is the
    actual behavior contract, not the incidental arrival order.
    """
    return sorted(reqs, key=lambda r: (r.get("path", ""), r.get("query", "")))


def load_cases():
    with open(CASES_FILE, encoding="utf-8") as fh:
        return json.load(fh)


class MockServer:
    def __init__(self, spec_path: Path):
        import socket

        # Bind port 0 to grab a free port, then hand the same port to the server.
        with socket.socket() as s:
            s.bind(("127.0.0.1", 0))
            self.port = s.getsockname()[1]
        self.spec_path = spec_path
        self.log_file = tempfile.NamedTemporaryFile(
            "w", suffix=".jsonl", delete=False
        )
        self.log_file.close()
        self.proc = None
        self.base = f"http://127.0.0.1:{self.port}"

    def start(self):
        self.proc = subprocess.Popen(
            [
                sys.executable,
                str(HERE / "mock_gha.py"),
                "--port",
                str(self.port),
                "--spec",
                str(self.spec_path),
                "--log",
                self.log_file.name,
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        # Wait for the server to accept connections.
        import socket
        import urllib.request

        deadline = time.time() + 10
        while time.time() < deadline:
            try:
                with socket.create_connection(("127.0.0.1", self.port), timeout=0.5):
                    return
            except OSError:
                if self.proc.poll() is not None:
                    raise RuntimeError("mock server exited early")
                time.sleep(0.05)
        raise RuntimeError("mock server did not start")

    def requests(self):
        with open(self.log_file.name, encoding="utf-8") as fh:
            return [json.loads(line) for line in fh if line.strip()]

    def stop(self):
        if self.proc and self.proc.poll() is None:
            self.proc.send_signal(signal.SIGTERM)
            try:
                self.proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                self.proc.kill()
        try:
            os.unlink(self.log_file.name)
        except OSError:
            pass


def make_git_repo(repo_dir: Path, remote: str, branch: str):
    """Create a minimal git repo whose HEAD is `branch` with one remote."""
    repo_dir.mkdir(parents=True, exist_ok=True)
    env = {
        **{k: v for k, v in os.environ.items()},
        "GIT_CONFIG_GLOBAL": "/dev/null",
        "GIT_CONFIG_SYSTEM": "/dev/null",
    }

    def git(*args):
        subprocess.run(
            ["git", *args],
            cwd=repo_dir,
            check=True,
            env=env,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )

    git("init", "-q", "-b", branch)
    git("remote", "add", "origin", remote)


def run_case(case, binary):
    """Run `binary` for `case`; return (stdout, stderr, exit_code, requests)."""
    with tempfile.TemporaryDirectory() as tmp:
        tmp = Path(tmp)
        env = {
            **{k: v for k, v in os.environ.items()},
            **BASE_ENV,
            "HOME": str(tmp / "home"),
        }
        (tmp / "home").mkdir()
        env.pop("GITHUB_API_URL", None)
        env.pop("GITHUB_SERVER_URL", None)

        server = None
        requests = []
        args = list(case.get("args", []))

        if case.get("kind") == "mock":
            spec_path = SPECS / f"{case['spec']}.json"
            server = MockServer(spec_path)
            server.start()
            api = server.base
            env["GITHUB_API_URL"] = api
            if case.get("server_url"):
                env["GITHUB_SERVER_URL"] = case["server_url"]

            if case.get("local_repo"):
                repo_dir = tmp / "repo"
                make_git_repo(repo_dir, case["local_repo"]["remote"],
                              case["local_repo"]["branch"])
                if case.get("use_cwd"):
                    cwd = repo_dir
                else:
                    args = [str(repo_dir), *args]
                    cwd = tmp
            else:
                cwd = tmp
        else:
            cwd = tmp

        try:
            proc = subprocess.run(
                [binary, *args],
                cwd=cwd,
                env=env,
                capture_output=True,
                text=True,
                timeout=case.get("timeout", 30),
            )
            stdout, stderr, code = proc.stdout, proc.stderr, proc.returncode
        finally:
            if server:
                captured_requests = server.requests()
                server.stop()
                requests = captured_requests

    if requests is None:
        requests = []
    return stdout, stderr, code, requests


def raw_outcome(case, binary):
    stdout, stderr, code, requests = run_case(case, binary)
    return {"exit_code": code, "stdout": stdout, "stderr": stderr,
            "requests": normalize_requests(requests)}


def reduce_result(case, raw):
    """Apply the case's normalization / structural policy to a raw result.

    - nice_time normalization (default): the wall-clock token in the rendered
      header is nondeterministic, so it is replaced with @NOW.
    - structural_stdout: for --help, the exact wording is click/clap-specific
      and non-functional (INV-PARITY), so instead of storing stdout verbatim we
      record a list of tokens that must appear. Exact wording is a D-1
      divergence; the *option surface* is the functional contract.
    """
    norm = case.get("normalize", ["nice_time"])
    result = {"exit_code": raw["exit_code"]}
    if "structural_stdout" in case:
        result["structural_stdout"] = case["structural_stdout"]
    else:
        result["stdout"] = normalize(raw["stdout"]) if "nice_time" in norm \
            else raw["stdout"]
    result["stderr"] = normalize(raw["stderr"]) if "nice_time" in norm \
        else raw["stderr"]
    if case.get("kind") == "mock" and case.get("check_requests", True):
        result["requests"] = raw["requests"]
    return result


def outcome(case, binary):
    return reduce_result(case, raw_outcome(case, binary))


def structural_ok(tokens, actual_stdout):
    missing = [t for t in tokens if t not in actual_stdout]
    return missing


def cmd_capture(cases, oracle):
    EXPECTED.mkdir(parents=True, exist_ok=True)
    failures = 0
    for case in cases:
        name = case["name"]
        res = outcome(case, oracle)
        dest = EXPECTED / f"{name}.json"
        dest.write_text(json.dumps(res, indent=2, sort_keys=True) + "\n",
                        encoding="utf-8")
        print(f"captured {name} (exit {res['exit_code']}) -> {dest.name}")
    return failures


def cmd_check(cases, binary):
    failures = 0
    for case in cases:
        name = case["name"]
        exp_path = EXPECTED / f"{name}.json"
        if not exp_path.exists():
            print(f"FAIL {name}: no fixture {exp_path.name} (run capture first)")
            failures += 1
            continue
        expected = json.loads(exp_path.read_text(encoding="utf-8"))
        raw = raw_outcome(case, binary)
        actual = reduce_result(case, raw)
        diffs = []
        for key in ("exit_code", "stdout", "stderr", "requests"):
            if key in expected or key in actual:
                if expected.get(key) != actual.get(key):
                    diffs.append(key)
        # Structural stdout: verify each required token appears in the raw stdout.
        if "structural_stdout" in expected:
            missing = structural_ok(expected["structural_stdout"], raw["stdout"])
            if missing:
                diffs.append(f"structural missing {missing}")
        if diffs:
            failures += 1
            print(f"FAIL {name}: differs in {', '.join(diffs)}")
            for key in diffs:
                if key.startswith("structural"):
                    print(f"  --- actual stdout ---")
                    print(indent(raw["stdout"]))
                    continue
                print(f"  --- expected {key} ---")
                print(indent(str(expected.get(key))))
                print(f"  +++ actual {key} +++")
                print(indent(str(actual.get(key))))
        else:
            print(f"ok   {name}")
    return failures


def indent(text):
    return "\n".join("    " + line for line in str(text).splitlines()) or "    <empty>"


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--mode", required=True,
                    choices=["capture", "check", "selfcheck"])
    ap.add_argument("--oracle")
    ap.add_argument("--rust")
    ap.add_argument("--only", help="run only cases whose name contains this")
    args = ap.parse_args(argv)

    # Resolve binary paths to absolute: run_case sets cwd to a temp dir, which
    # would break a relative path to the oracle/rust binary.
    if args.oracle:
        args.oracle = os.path.abspath(args.oracle)
    if args.rust:
        args.rust = os.path.abspath(args.rust)

    cases = load_cases()
    if args.only:
        cases = [c for c in cases if args.only in c["name"]]

    if args.mode == "capture":
        if not args.oracle:
            ap.error("capture requires --oracle")
        return cmd_capture(cases, args.oracle)
    if args.mode == "check":
        if not args.rust:
            ap.error("check requires --rust")
        return cmd_check(cases, args.rust)
    if args.mode == "selfcheck":
        if not args.oracle:
            ap.error("selfcheck requires --oracle")
        # check mode against the oracle: capture in-memory then compare.
        failures = 0
        for case in cases:
            name = case["name"]
            first = outcome(case, args.oracle)
            second = outcome(case, args.oracle)
            diffs = [k for k in ("exit_code", "stdout", "stderr", "requests")
                     if first.get(k) != second.get(k)]
            if diffs:
                failures += 1
                print(f"FAIL {name}: nondeterministic in {', '.join(diffs)}")
                for k in diffs:
                    print(f"  run1 {k}: {first.get(k)!r}")
                    print(f"  run2 {k}: {second.get(k)!r}")
            else:
                print(f"ok   {name} (deterministic)")
        return failures


if __name__ == "__main__":
    sys.exit(1 if main() else 0)
