#!/usr/bin/env python3
"""
Self-test: run the disk streaming conformance suite against the repo's dev-only chunk server.

This is intended as a quick "works on my machine" validation for changes to:
- `server/chunk_server.js`
- `tools/disk-streaming-conformance/conformance.py`

No third-party deps; stdlib only.
"""

from __future__ import annotations

import hashlib
import json
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
        except urllib.error.URLError as e:
            last_err = str(e)
            time.sleep(0.05)
    raise RuntimeError(f"Timed out waiting for {url} ({last_err or 'no response'})")


def _sha256_hex(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def main() -> int:
    root = _repo_root()
    chunk_server_js = root / "server" / "chunk_server.js"
    if not chunk_server_js.exists():
        print(f"error: chunk server not found: {chunk_server_js}", file=sys.stderr)
        return 2

    port = _free_port()

    with tempfile.TemporaryDirectory(prefix="aero-chunk-server-") as tmpdir:
        tmp = Path(tmpdir)
        chunks_dir = tmp / "chunks"
        chunks_dir.mkdir(parents=True, exist_ok=True)

        # Keep it small but realistic: must be multiple of 512.
        total_size = 4096
        chunk_size = 1024
        if total_size % 512 != 0 or chunk_size % 512 != 0 or total_size % chunk_size != 0:
            raise RuntimeError("internal error: sizes must be multiples of 512 and evenly divisible")

        disk_bytes = os.urandom(total_size)
        chunk_count = total_size // chunk_size
        chunk_index_width = 8

        chunks: list[dict[str, object]] = []
        for i in range(chunk_count):
            start = i * chunk_size
            end = start + chunk_size
            chunk = disk_bytes[start:end]
            name = str(i).zfill(chunk_index_width) + ".bin"
            (chunks_dir / name).write_bytes(chunk)
            chunks.append({"size": len(chunk), "sha256": _sha256_hex(chunk)})

        manifest = {
            "schema": "aero.chunked-disk-image.v1",
            "version": "v1",
            "mimeType": "application/octet-stream",
            "totalSize": total_size,
            "chunkSize": chunk_size,
            "chunkCount": chunk_count,
            "chunkIndexWidth": chunk_index_width,
            "chunks": chunks,
        }
        (tmp / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")

        base_url = f"http://127.0.0.1:{port}"
        manifest_url = f"{base_url}/manifest.json"

        proc = subprocess.Popen(
            ["node", str(chunk_server_js), "--dir", str(tmp), "--port", str(port)],
            cwd=str(root),
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        try:
            _wait_for_http(manifest_url, timeout_s=10.0)
            # Run in strict mode: the dev server should be fully compliant.
            return int(
                conformance.main(
                    [
                        "--mode",
                        "chunked",
                        "--base-url",
                        base_url,
                        "--origin",
                        "https://example.com",
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
                    print("chunk_server.js output:")
                    print(out)


if __name__ == "__main__":
    raise SystemExit(main())

