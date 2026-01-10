#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""
Win7 virtio functional tests host harness (QEMU).

This is a Python alternative to Invoke-AeroVirtioWin7Tests.ps1 for environments where
PowerShell is inconvenient (e.g. Linux CI).

It:
- starts a tiny HTTP server on 127.0.0.1:<port> (guest reaches it as 10.0.2.2:<port> via slirp)
- launches QEMU with virtio-blk + virtio-net and COM1 redirected to a log file
- tails the serial log until it sees AERO_VIRTIO_SELFTEST|RESULT|PASS/FAIL
"""

from __future__ import annotations

import argparse
import http.server
import os
import shlex
import socketserver
import subprocess
import sys
import time
from pathlib import Path
from threading import Thread


class _QuietHandler(http.server.BaseHTTPRequestHandler):
    expected_path: str = "/aero-virtio-selftest"

    def do_GET(self) -> None:  # noqa: N802
        if self.path == self.expected_path:
            body = b"OK\n"
            self.send_response(200)
        else:
            body = b"NOT_FOUND\n"
            self.send_response(404)

        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, fmt: str, *args: object) -> None:
        # Silence default request logging (we only care about the selftest marker).
        return


class _ReusableTcpServer(socketserver.TCPServer):
    allow_reuse_address = True


def _read_new_bytes(path: Path, pos: int) -> tuple[bytes, int]:
    try:
        with path.open("rb") as f:
            f.seek(pos)
            data = f.read()
            return data, pos + len(data)
    except FileNotFoundError:
        return b"", pos
    except OSError:
        # On some platforms/filesystems the log file may briefly be unavailable while QEMU opens it.
        return b"", pos


def _stop_process(proc: subprocess.Popen[bytes]) -> None:
    if proc.poll() is not None:
        return
    try:
        proc.terminate()
        proc.wait(timeout=5)
    except Exception:
        try:
            proc.kill()
        except Exception:
            pass


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--qemu-system", required=True, help="Path to qemu-system-* binary")
    parser.add_argument("--disk-image", required=True, help="Prepared Win7 disk image")
    parser.add_argument(
        "--serial-log",
        default="win7-virtio-serial.log",
        help="Path to capture COM1 serial output",
    )
    parser.add_argument("--memory-mb", type=int, default=2048)
    parser.add_argument("--smp", type=int, default=2)
    parser.add_argument("--timeout-seconds", type=int, default=600)
    parser.add_argument("--http-port", type=int, default=18080)
    parser.add_argument("--http-path", default="/aero-virtio-selftest")
    parser.add_argument("--snapshot", action="store_true", help="Discard disk writes (snapshot=on)")
    parser.add_argument(
        "--follow-serial",
        action="store_true",
        help="Stream newly captured COM1 serial output to stdout while waiting",
    )

    # Any remaining args are passed directly to QEMU.
    args, qemu_extra = parser.parse_known_args()

    disk_image = Path(args.disk_image).resolve()
    serial_log = Path(args.serial_log).resolve()
    serial_log.parent.mkdir(parents=True, exist_ok=True)

    if serial_log.exists():
        serial_log.unlink()

    handler = type("_Handler", (_QuietHandler,), {"expected_path": args.http_path})

    with _ReusableTcpServer(("127.0.0.1", args.http_port), handler) as httpd:
        thread = Thread(target=httpd.serve_forever, daemon=True)
        thread.start()

        drive = f"file={disk_image},if=virtio,cache=writeback"
        if args.snapshot:
            drive += ",snapshot=on"

        qemu_args = [
            args.qemu_system,
            "-m",
            str(args.memory_mb),
            "-smp",
            str(args.smp),
            "-display",
            "none",
            "-no-reboot",
            "-chardev",
            f"file,id=charserial0,path={serial_log}",
            "-serial",
            "chardev:charserial0",
            "-netdev",
            "user,id=net0",
            "-device",
            "virtio-net-pci,netdev=net0",
            "-drive",
            drive,
        ] + qemu_extra

        print("Launching QEMU:")
        print("  " + " ".join(shlex.quote(str(a)) for a in qemu_args))
        proc = subprocess.Popen(qemu_args)
        try:
            pos = 0
            tail = b""
            deadline = time.monotonic() + args.timeout_seconds

            while time.monotonic() < deadline:
                chunk, pos = _read_new_bytes(serial_log, pos)
                if chunk:
                    if args.follow_serial:
                        sys.stdout.write(chunk.decode("utf-8", errors="replace"))
                        sys.stdout.flush()

                    tail += chunk
                    if len(tail) > 131072:
                        tail = tail[-131072:]

                    if b"AERO_VIRTIO_SELFTEST|RESULT|PASS" in tail:
                        print("PASS: AERO_VIRTIO_SELFTEST|RESULT|PASS")
                        return 0
                    if b"AERO_VIRTIO_SELFTEST|RESULT|FAIL" in tail:
                        print("FAIL: AERO_VIRTIO_SELFTEST|RESULT|FAIL")
                        _print_tail(serial_log)
                        return 1

                if proc.poll() is not None:
                    # One last read after exit in case QEMU shut down immediately after writing the marker.
                    chunk2, pos = _read_new_bytes(serial_log, pos)
                    if chunk2:
                        tail += chunk2
                        if b"AERO_VIRTIO_SELFTEST|RESULT|PASS" in tail:
                            print("PASS: AERO_VIRTIO_SELFTEST|RESULT|PASS")
                            return 0
                        if b"AERO_VIRTIO_SELFTEST|RESULT|FAIL" in tail:
                            print("FAIL: AERO_VIRTIO_SELFTEST|RESULT|FAIL")
                            _print_tail(serial_log)
                            return 1

                    print(f"FAIL: QEMU exited before selftest result marker (exit code: {proc.returncode})")
                    _print_tail(serial_log)
                    return 3

                time.sleep(0.25)

            print("FAIL: timed out waiting for AERO_VIRTIO_SELFTEST result marker")
            _print_tail(serial_log)
            return 2
        finally:
            _stop_process(proc)
            httpd.shutdown()


def _print_tail(path: Path) -> None:
    try:
        data = path.read_bytes()
    except FileNotFoundError:
        return
    except OSError:
        return

    tail = data[-8192:]
    sys.stdout.write("\n--- Serial tail ---\n")
    try:
        sys.stdout.write(tail.decode("utf-8", errors="replace"))
    except Exception:
        # Fallback if stdout encoding is strict.
        sys.stdout.buffer.write(tail)


if __name__ == "__main__":
    raise SystemExit(main())
