#!/usr/bin/env python3
"""
Self-test: run the disk streaming conformance suite against the repo's dev-only range server
in "private" mode (Authorization required).

This validates Range-mode auth + CORS preflight behavior.
No third-party deps; stdlib only.
"""

from __future__ import annotations

import os
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

import conformance


def _repo_root() -> Path:
    return Path(__file__).resolve().parents[2]


def _free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.bind(("127.0.0.1", 0))
        return int(s.getsockname()[1])


def _wait_for_http(url: str, *, timeout_s: float = 5.0) -> None:
    start = time.time()
    last_err: str | None = None
    while time.time() - start < timeout_s:
        try:
            req = urllib.request.Request(url=url, method="HEAD")
            with urllib.request.urlopen(req, timeout=1.0) as resp:
                if 200 <= int(getattr(resp, "status", 0)) < 500:
                    return
        except urllib.error.HTTPError as e:
            # `urlopen` raises on 4xx. For readiness we treat any <500 as "server is up".
            if 200 <= int(getattr(e, "code", 0)) < 500:
                return
            last_err = str(e)
        except urllib.error.URLError as e:
            last_err = str(e)
            time.sleep(0.05)
    raise RuntimeError(f"Timed out waiting for {url} ({last_err or 'no response'})")


def main() -> int:
    root = _repo_root()
    range_server_js = root / "server" / "range_server.js"
    if not range_server_js.exists():
        print(f"error: range server not found: {range_server_js}", file=sys.stderr)
        return 2

    port = _free_port()
    token = "Bearer selftest-token"

    with tempfile.TemporaryDirectory(prefix="aero-range-server-private-") as tmpdir:
        tmp = Path(tmpdir)
        test_file = tmp / "disk.img"
        test_file.write_bytes(os.urandom(4096))

        base_url = f"http://127.0.0.1:{port}/{test_file.name}"

        proc = subprocess.Popen(
            [
                "node",
                str(range_server_js),
                "--dir",
                str(tmp),
                "--port",
                str(port),
                "--auth-token",
                token,
            ],
            cwd=str(root),
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        try:
            _wait_for_http(base_url, timeout_s=10.0)
            return int(
                conformance.main(
                    [
                        "--base-url",
                        base_url,
                        "--origin",
                        "https://example.com",
                        "--token",
                        token,
                        "--strict",
                    ]
                )
            )
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=2.0)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=2.0)

            if proc.stdout is not None:
                out = proc.stdout.read().strip()
                if out:
                    print()
                    print("range_server.js output:")
                    print(out)


if __name__ == "__main__":
    raise SystemExit(main())

