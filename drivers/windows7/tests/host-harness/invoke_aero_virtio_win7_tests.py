#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""
Win7 virtio functional tests host harness (QEMU).

This is a Python alternative to Invoke-AeroVirtioWin7Tests.ps1 for environments where
PowerShell is inconvenient (e.g. Linux CI).

Note: In the default (non-transitional) mode, this harness uses *modern-only* virtio-pci devices
(`disable-legacy=on`) and forces the Aero contract v1 PCI Revision ID (`x-pci-revision=0x01`) for
virtio-blk/virtio-net/virtio-input so the Win7 drivers bind to the Aero contract v1 IDs
(DEV_1041/DEV_1042/DEV_1052) and strict `&REV_01` INFs can bind under QEMU.

When `--with-virtio-snd` is enabled, the harness also configures virtio-snd as a modern-only
virtio-pci device and forces the Aero contract v1 revision (`disable-legacy=on,x-pci-revision=0x01`)
so the canonical Win7 INF (`aero_virtio_snd.inf`) can bind to `PCI\\VEN_1AF4&DEV_1059&REV_01` under QEMU.

Use `--virtio-transitional` to opt back into QEMU's default transitional devices (legacy + modern)
for older QEMU builds (or when intentionally testing legacy driver packages).
In transitional mode the harness uses QEMU defaults for virtio-blk/virtio-net (and relaxes per-test marker
requirements so older guest selftest binaries can still be used).
It also attempts to attach virtio-input keyboard/mouse devices when QEMU advertises them; otherwise it
warns that the guest virtio-input selftest will likely fail.

It:
- starts a tiny HTTP server on 127.0.0.1:<port> (guest reaches it as 10.0.2.2:<port> via slirp)
  - use `--http-log <path>` to record per-request logs (useful for CI artifacts)
  - serves a deterministic large payload at `<http_path>-large`:
    - HTTP 200
    - 1 MiB body of bytes 0..255 repeating
    - correct Content-Length
    - also accepts a deterministic 1 MiB upload via HTTP POST to the same `...-large` path (validates SHA-256)
- launches QEMU with virtio-blk + virtio-net + virtio-input (and optionally virtio-snd) and COM1 redirected to a log file
  - in transitional mode virtio-input is skipped (with a warning) if QEMU does not advertise virtio-keyboard-pci/virtio-mouse-pci
- captures QEMU stderr to `<serial-base>.qemu.stderr.log` (next to the serial log) for debugging early exits
- optionally enables a QMP monitor to:
  - request a graceful QEMU shutdown so side-effectful devices (notably the `wav` audiodev backend) can flush/finalize
    their output files before verification
  - inject deterministic virtio-input events via `input-send-event` (when `--with-input-events` /
    `--with-virtio-input-events` is enabled)
  (unix socket on POSIX; TCP loopback fallback on Windows)
- tails the serial log until it sees AERO_VIRTIO_SELFTEST|RESULT|PASS/FAIL
  - in default (non-transitional) mode, a PASS result also requires per-test markers for virtio-blk, virtio-input,
     virtio-snd (PASS or SKIP), virtio-snd-capture (PASS or SKIP), virtio-snd-duplex (PASS or SKIP), and virtio-net
     so older selftest binaries cannot accidentally pass
  - when --with-virtio-snd is enabled, virtio-snd, virtio-snd-capture, and virtio-snd-duplex must PASS (not SKIP)
  - when --with-input-events (alias: --with-virtio-input-events) is enabled, virtio-input-events must PASS (not FAIL/missing)

For convenience when scraping CI logs, the harness may also emit a host-side virtio-net marker when the guest
includes large-transfer fields:

- `AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_LARGE|PASS/FAIL/INFO|large_ok=...|large_bytes=...|large_fnv1a64=...|large_mbps=...|upload_ok=...|upload_bytes=...|upload_mbps=...`
"""

from __future__ import annotations

import argparse
import http.server
import hashlib
import json
import math
import warnings
import os
import shlex
import socket
import socketserver
import subprocess
import struct
import sys
import time
from array import array
from dataclasses import dataclass
from pathlib import Path
from threading import Thread
from typing import Optional


def _append_suffix_before_query_fragment(path: str, suffix: str) -> str:
    q = path.find("?")
    h = path.find("#")
    idx = -1
    if q != -1 and h != -1:
        idx = min(q, h)
    elif q != -1:
        idx = q
    elif h != -1:
        idx = h
    if idx == -1:
        return path + suffix
    return path[:idx] + suffix + path[idx:]


class _QuietHandler(http.server.BaseHTTPRequestHandler):
    # Use HTTP/1.1 so the built-in `Expect: 100-continue` handling can run for large POST uploads
    # (WinHTTP may send this header; without a 100 response the client and server can deadlock).
    protocol_version = "HTTP/1.1"
    expected_path: str = "/aero-virtio-selftest"
    http_log_path: Optional[Path] = None
    large_body: bytes = bytes(range(256)) * (1024 * 1024 // 256)
    socket_timeout_seconds: float = 60.0
    large_chunk_size: int = 64 * 1024
    large_etag: str = '"8505ae4435522325"'
    large_upload_sha256: str = "fbbab289f7f94b25736c58be46a994c441fd02552cc6022352e3d86d2fab7c83"

    def setup(self) -> None:
        super().setup()
        try:
            # Avoid hanging indefinitely if the guest connects but stalls (or stops reading mid-body).
            self.connection.settimeout(self.socket_timeout_seconds)
        except Exception:
            pass

    def handle_expect_100(self) -> bool:  # noqa: N802
        # Ensure the interim 100 Continue is flushed immediately so clients that
        # wait for it (e.g. WinHTTP) do not deadlock with the server's body read.
        ok = super().handle_expect_100()
        try:
            self.wfile.flush()
        except Exception:
            pass
        return ok

    def do_GET(self) -> None:  # noqa: N802
        self._handle_request(send_body=True)

    def do_HEAD(self) -> None:  # noqa: N802
        self._handle_request(send_body=False)

    def do_POST(self) -> None:  # noqa: N802
        self._handle_large_upload()

    def _handle_request(self, *, send_body: bool) -> None:
        etag: Optional[str] = None
        if self.path == self.expected_path:
            body = b"OK\n"
            content_type = "text/plain"
            self.send_response(200)
        elif self.path in (
            self.expected_path + "-large",
            _append_suffix_before_query_fragment(self.expected_path, "-large"),
        ):
            # Deterministic 1 MiB payload (0..255 repeating) for sustained virtio-net TX/RX stress.
            body = self.large_body
            content_type = "application/octet-stream"
            etag = self.large_etag
            self.send_response(200)
        else:
            body = b"NOT_FOUND\n"
            content_type = "text/plain"
            self.send_response(404)

        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        if etag is not None:
            self.send_header("ETag", etag)
        self.send_header("Connection", "close")
        # Make sure the server-side request loop terminates after this response
        # (we always send Connection: close and do not implement keep-alive).
        self.close_connection = True
        self.end_headers()
        if not send_body:
            return

        try:
            mv = memoryview(body)
            chunk = int(self.large_chunk_size) if self.large_chunk_size > 0 else len(mv)
            for i in range(0, len(mv), chunk):
                self.wfile.write(mv[i : i + chunk])
        except Exception:
            # Best-effort response; never fail the harness due to a socket send error.
            return

    def _handle_large_upload(self) -> None:
        """
        Accept a deterministic 1 MiB upload to the large endpoint and validate integrity.

        This stresses the guest TX path (in addition to the large GET, which stresses RX).
        """
        large_paths = (
            self.expected_path + "-large",
            _append_suffix_before_query_fragment(self.expected_path, "-large"),
        )
        if self.path not in large_paths:
            body = b"NOT_FOUND\n"
            self.send_response(404)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", str(len(body)))
            self.send_header("Cache-Control", "no-store")
            self.send_header("Connection", "close")
            self.close_connection = True
            self.end_headers()
            try:
                self.wfile.write(body)
            except Exception:
                return
            return

        expected_len = len(self.large_body)
        length_str = self.headers.get("Content-Length")
        try:
            length = int(length_str) if length_str is not None else 0
        except Exception:
            length = 0

        ok = False
        digest_hex: Optional[str] = None
        if length == expected_len:
            sha = hashlib.sha256()
            remaining = length
            while remaining > 0:
                to_read = min(remaining, max(1, int(self.large_chunk_size)))
                chunk = self.rfile.read(to_read)
                if not chunk:
                    break
                sha.update(chunk)
                remaining -= len(chunk)
            digest_hex = sha.hexdigest()
            ok = remaining == 0 and digest_hex == self.large_upload_sha256

        body = b"OK\n" if ok else b"BAD_UPLOAD\n"
        self.send_response(200 if ok else 400)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        if digest_hex is not None:
            self.send_header("X-Aero-Upload-SHA256", digest_hex)
        self.send_header("Connection", "close")
        self.close_connection = True
        self.end_headers()
        try:
            self.wfile.write(body)
        except Exception:
            return

    def log_message(self, fmt: str, *args: object) -> None:
        log_path = getattr(self, "http_log_path", None)
        if not log_path:
            # Silence default request logging unless explicitly enabled.
            return

        try:
            msg = "%s - - [%s] %s\n" % (
                self.address_string(),
                self.log_date_time_string(),
                fmt % args,
            )
        except Exception:
            return

        try:
            with open(log_path, "a", encoding="utf-8", errors="replace") as f:
                f.write(msg)
        except Exception:
            # Best-effort logging; never fail the harness due to HTTP log I/O.
            return


class _ReusableTcpServer(socketserver.ThreadingMixIn, socketserver.TCPServer):
    allow_reuse_address = True
    # Each request is handled on its own thread so a stalled large transfer can't block
    # the accept loop or prevent graceful shutdown.
    daemon_threads = True
    # Do not wait for handler threads on server_close(). The harness sets per-connection
    # socket timeouts so threads should exit promptly, but this avoids pathological hangs.
    block_on_close = False


@dataclass(frozen=True)
class _WaveFmtChunk:
    format_tag: int
    channels: int
    sample_rate: int
    block_align: int
    bits_per_sample: int
    # When format_tag is WAVE_FORMAT_EXTENSIBLE (0xFFFE), the fmt chunk also carries a
    # 16-byte SubFormat GUID that indicates the underlying sample type (PCM vs IEEE float).
    subformat: Optional[bytes] = None


@dataclass(frozen=True)
class _WaveFileInfo:
    fmt: _WaveFmtChunk
    data_offset: int
    data_size: int


@dataclass(frozen=True)
class _QmpEndpoint:
    unix_socket: Optional[Path] = None
    tcp_host: Optional[str] = None
    tcp_port: Optional[int] = None

    def qemu_arg(self) -> str:
        if self.unix_socket is not None:
            return f"unix:{self.unix_socket},server,nowait"
        if self.tcp_host is not None and self.tcp_port is not None:
            return f"tcp:{self.tcp_host}:{self.tcp_port},server,nowait"
        raise AssertionError("invalid QMP endpoint")


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


def _qmp_read_json(sock: socket.socket, *, timeout_seconds: float = 2.0) -> dict[str, object]:
    sock.settimeout(timeout_seconds)
    buf = b""
    while True:
        chunk = sock.recv(4096)
        if not chunk:
            raise RuntimeError("EOF while waiting for QMP JSON message")
        buf += chunk
        while b"\n" in buf:
            line, buf = buf.split(b"\n", 1)
            line = line.strip()
            if not line:
                continue
            try:
                return json.loads(line.decode("utf-8", errors="replace"))
            except json.JSONDecodeError:
                # QMP is line-delimited JSON; if we got partial/garbled data keep reading.
                continue


def _try_qmp_quit(endpoint: _QmpEndpoint, *, timeout_seconds: float = 2.0) -> bool:
    """
    Attempt to shut QEMU down gracefully via QMP.

    This is primarily to ensure side-effectful devices (notably the `wav` audiodev backend)
    flush/finalize their output files before the host harness verifies them.
    """
    if endpoint.tcp_port is None and endpoint.unix_socket is None:
        return False

    deadline = time.monotonic() + timeout_seconds
    while True:
        if endpoint.unix_socket is not None and not endpoint.unix_socket.exists():
            if time.monotonic() >= deadline:
                return False
            time.sleep(0.1)
            continue

        try:
            if endpoint.unix_socket is not None:
                with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as s:
                    s.connect(str(endpoint.unix_socket))
                    # Greeting.
                    _qmp_read_json(s, timeout_seconds=1.0)
                    s.sendall(b'{"execute":"qmp_capabilities"}\n')
                    # Capabilities ack (ignore contents).
                    _qmp_read_json(s, timeout_seconds=1.0)
                    s.sendall(b'{"execute":"quit"}\n')
                    return True
            else:
                host = endpoint.tcp_host or "127.0.0.1"
                port = endpoint.tcp_port
                if port is None:
                    return False
                with socket.create_connection((host, port), timeout=1.0) as s:
                    _qmp_read_json(s, timeout_seconds=1.0)
                    s.sendall(b'{"execute":"qmp_capabilities"}\n')
                    _qmp_read_json(s, timeout_seconds=1.0)
                    s.sendall(b'{"execute":"quit"}\n')
                    return True
        except Exception:
            if time.monotonic() >= deadline:
                return False
            time.sleep(0.1)


# Stable QOM `id=` values for virtio-input devices so QMP can target them explicitly (rather than
# relying on whatever default input routing the QEMU machine config uses, which may hit PS/2).
_VIRTIO_INPUT_QMP_KEYBOARD_ID = "aero_virtio_kbd0"
_VIRTIO_INPUT_QMP_MOUSE_ID = "aero_virtio_mouse0"


def _qmp_read_response(sock: socket.socket, *, timeout_seconds: float = 2.0) -> dict[str, object]:
    """
    Read QMP messages until we see a command response (`{"return":...}` or `{"error":...}`).

    QMP may interleave asynchronous event notifications; these are ignored for the purpose of simple
    request/response helpers.
    """
    while True:
        msg = _qmp_read_json(sock, timeout_seconds=timeout_seconds)
        if "return" in msg or "error" in msg:
            return msg


def _qmp_send_command(sock: socket.socket, cmd: dict[str, object]) -> dict[str, object]:
    sock.sendall(json.dumps(cmd, separators=(",", ":")).encode("utf-8") + b"\n")
    resp = _qmp_read_response(sock, timeout_seconds=2.0)
    if "error" in resp:
        raise RuntimeError(f"QMP command failed: {resp}")
    return resp


def _qmp_connect(endpoint: _QmpEndpoint, *, timeout_seconds: float = 5.0) -> socket.socket:
    if endpoint.tcp_port is None and endpoint.unix_socket is None:
        raise RuntimeError("invalid QMP endpoint (no tcp_port/unix_socket)")

    deadline = time.monotonic() + timeout_seconds
    while True:
        if endpoint.unix_socket is not None and not endpoint.unix_socket.exists():
            if time.monotonic() >= deadline:
                raise RuntimeError("timed out waiting for QMP unix socket")
            time.sleep(0.05)
            continue

        try:
            if endpoint.unix_socket is not None:
                s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                s.settimeout(2.0)
                s.connect(str(endpoint.unix_socket))
            else:
                host = endpoint.tcp_host or "127.0.0.1"
                port = endpoint.tcp_port
                if port is None:
                    raise RuntimeError("invalid QMP endpoint (missing tcp_port)")
                s = socket.create_connection((host, port), timeout=2.0)

            # Greeting.
            _qmp_read_json(s, timeout_seconds=2.0)
            _qmp_send_command(s, {"execute": "qmp_capabilities"})
            return s
        except Exception:
            try:
                s.close()  # type: ignore[misc]
            except Exception:
                pass
            if time.monotonic() >= deadline:
                raise
            time.sleep(0.05)


def _qmp_key_event(qcode: str, *, down: bool) -> dict[str, object]:
    return {
        "type": "key",
        "data": {"down": down, "key": {"type": "qcode", "data": qcode}},
    }


def _qmp_btn_event(button: str, *, down: bool) -> dict[str, object]:
    return {"type": "btn", "data": {"down": down, "button": button}}


def _qmp_rel_event(axis: str, value: int) -> dict[str, object]:
    return {"type": "rel", "data": {"axis": axis, "value": value}}


def _qmp_input_send_event_cmd(
    events: list[dict[str, object]], *, device: Optional[str] = None
) -> dict[str, object]:
    args: dict[str, object] = {"events": events}
    if device is not None:
        args["device"] = device
    return {"execute": "input-send-event", "arguments": args}


def _qmp_input_send_event_command(
    events: list[dict[str, object]], *, device: Optional[str] = None
) -> dict[str, object]:
    """
    Build a QMP `input-send-event` command.

    This helper exists primarily so host-harness unit tests can sanity-check the command structure.
    """
    return _qmp_input_send_event_cmd(events, device=device)


def _qmp_deterministic_keyboard_events(*, qcode: str) -> list[dict[str, object]]:
    # Press + release a single key via qcode (stable across host layouts).
    return [
        _qmp_key_event(qcode, down=True),
        _qmp_key_event(qcode, down=False),
    ]


def _qmp_deterministic_mouse_events() -> list[dict[str, object]]:
    # Small relative motion + a left click.
    return [
        _qmp_rel_event("x", 10),
        _qmp_rel_event("y", 5),
        _qmp_btn_event("left", down=True),
        _qmp_btn_event("left", down=False),
    ]


@dataclass(frozen=True)
class _VirtioInputQmpInjectInfo:
    keyboard_device: Optional[str]
    mouse_device: Optional[str]


def _try_qmp_input_inject_virtio_input_events(endpoint: _QmpEndpoint) -> _VirtioInputQmpInjectInfo:
    """
    Inject a small, deterministic keyboard + mouse sequence via QMP.

    Guest-side verification lives in `aero-virtio-selftest.exe` under the marker:
      AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|...
    """
    def send(
        sock: socket.socket, events: list[dict[str, object]], *, device: Optional[str]
    ) -> Optional[str]:
        """
        Send one `input-send-event` command.

        Prefer targeting the virtio input devices by QOM id (so we don't accidentally exercise PS/2),
        but fall back to broadcasting the event when QEMU rejects the `device` argument. This keeps
        the harness compatible with QEMU builds that don't accept device routing for input injection.
        """
        if device is None:
            _qmp_send_command(sock, _qmp_input_send_event_cmd(events, device=None))
            return None
        try:
            _qmp_send_command(sock, _qmp_input_send_event_cmd(events, device=device))
            return device
        except Exception as e_with_device:
            try:
                _qmp_send_command(sock, _qmp_input_send_event_cmd(events, device=None))
            except Exception as e_without_device:
                raise RuntimeError(
                    f"QMP input-send-event failed with device={device} ({e_with_device}) and without device ({e_without_device})"
                ) from e_without_device
            print(
                f"WARNING: QMP input-send-event rejected device={device}; falling back to broadcast: {e_with_device}",
                file=sys.stderr,
            )
            return None

    with _qmp_connect(endpoint, timeout_seconds=5.0) as s:
        kbd_device: Optional[str] = _VIRTIO_INPUT_QMP_KEYBOARD_ID
        mouse_device: Optional[str] = _VIRTIO_INPUT_QMP_MOUSE_ID

        kbd_events = _qmp_deterministic_keyboard_events(qcode="a")
        mouse_events = _qmp_deterministic_mouse_events()

        # Keyboard: 'a' press + release.
        kbd_device = send(s, [kbd_events[0]], device=kbd_device)
        time.sleep(0.05)
        kbd_device = send(s, [kbd_events[1]], device=kbd_device)

        # Mouse: small movement then left click.
        time.sleep(0.05)
        mouse_device = send(s, mouse_events[0:2], device=mouse_device)
        time.sleep(0.05)
        mouse_device = send(s, [mouse_events[2]], device=mouse_device)
        time.sleep(0.05)
        mouse_device = send(s, [mouse_events[3]], device=mouse_device)

        return _VirtioInputQmpInjectInfo(keyboard_device=kbd_device, mouse_device=mouse_device)


def _find_free_tcp_port() -> Optional[int]:
    try:
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            s.bind(("127.0.0.1", 0))
            return int(s.getsockname()[1])
    except Exception:
        return None


def _qemu_quote_keyval_value(value: str) -> str:
    """
    Quote a QEMU keyval value (for args like `-drive file=...`, `-chardev ...,path=...`).

    This is primarily to keep file paths containing spaces or commas robust across host platforms.
    QEMU keyval parsing supports `"..."` quoting and backslash-escaped quotes.
    """

    # QEMU treats `\` as the escape character inside quoted values, so ensure we always escape it
    # (Windows paths use backslashes).
    escaped = value.replace("\\", "\\\\").replace('"', '\\"')
    return '"' + escaped + '"'


def _virtio_snd_skip_failure_message(tail: bytes) -> str:
    # The guest selftest's virtio-snd marker is intentionally strict and machine-friendly:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-snd|PASS/FAIL/SKIP
    #
    # Any reason for SKIP is logged as human-readable text, so the host harness must infer
    # a useful error message from the tail log.
    if b"virtio-snd: skipped (enable with --test-snd)" in tail:
        return (
            "FAIL: VIRTIO_SND_SKIPPED: virtio-snd test was skipped (guest not configured with --test-snd) "
            "but --with-virtio-snd was enabled"
        )
    if b"virtio-snd: disabled by --disable-snd" in tail:
        return "FAIL: VIRTIO_SND_SKIPPED: virtio-snd test was skipped (--disable-snd) but --with-virtio-snd was enabled"
    if b"virtio-snd:" in tail and b"device not detected" in tail:
        return "FAIL: VIRTIO_SND_SKIPPED: virtio-snd test was skipped (device missing) but --with-virtio-snd was enabled"
    return "FAIL: VIRTIO_SND_SKIPPED: virtio-snd test was skipped but --with-virtio-snd was enabled"


def _virtio_snd_capture_skip_failure_message(tail: bytes) -> str:
    # The capture marker is separate from the playback marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|PASS/FAIL/SKIP|...
    prefix = b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|"
    if b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|endpoint_missing" in tail:
        return "FAIL: VIRTIO_SND_CAPTURE_SKIPPED: virtio-snd capture endpoint missing but --with-virtio-snd was enabled"
    if b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|flag_not_set" in tail:
        return (
            "FAIL: VIRTIO_SND_CAPTURE_SKIPPED: virtio-snd capture test was skipped (flag_not_set) but --with-virtio-snd was enabled "
            "(ensure the guest is configured with --test-snd/--require-snd or capture flags)"
        )
    if b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|disabled" in tail:
        return (
            "FAIL: VIRTIO_SND_CAPTURE_SKIPPED: virtio-snd capture test was skipped (disabled via --disable-snd or --disable-snd-capture) "
            "but --with-virtio-snd was enabled"
        )
    if b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|device_missing" in tail:
        return "FAIL: VIRTIO_SND_CAPTURE_SKIPPED: virtio-snd capture test was skipped (device missing) but --with-virtio-snd was enabled"

    # Fallback: extract the skip reason token from the marker itself so callers get something actionable
    # (e.g. wrong_service/driver_not_bound/device_error/topology_interface_missing).
    idx = tail.rfind(prefix)
    if idx != -1:
        end = tail.find(b"\n", idx)
        if end == -1:
            end = len(tail)
        reason = tail[idx + len(prefix) : end].strip()
        if reason:
            reason_str = reason.decode("utf-8", errors="replace").strip()
            return (
                f"FAIL: VIRTIO_SND_CAPTURE_SKIPPED: virtio-snd capture test was skipped ({reason_str}) "
                "but --with-virtio-snd was enabled"
            )
    return "FAIL: VIRTIO_SND_CAPTURE_SKIPPED: virtio-snd capture test was skipped but --with-virtio-snd was enabled"


def _virtio_snd_duplex_skip_failure_message(tail: bytes) -> str:
    # Full-duplex marker (render + capture concurrently):
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|PASS/FAIL/SKIP|...
    prefix = b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|"
    if b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|endpoint_missing" in tail:
        return "FAIL: VIRTIO_SND_DUPLEX_SKIPPED: virtio-snd duplex test was skipped (endpoint_missing) but --with-virtio-snd was enabled"
    if b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|flag_not_set" in tail:
        return (
            "FAIL: VIRTIO_SND_DUPLEX_SKIPPED: virtio-snd duplex test was skipped (flag_not_set) but --with-virtio-snd was enabled "
            "(ensure the guest is configured with --test-snd-capture or AERO_VIRTIO_SELFTEST_TEST_SND_CAPTURE=1)"
        )
    if b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|disabled" in tail:
        return (
            "FAIL: VIRTIO_SND_DUPLEX_SKIPPED: virtio-snd duplex test was skipped (disabled via --disable-snd or --disable-snd-capture) "
            "but --with-virtio-snd was enabled"
        )
    if b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|device_missing" in tail:
        return "FAIL: VIRTIO_SND_DUPLEX_SKIPPED: virtio-snd duplex test was skipped (device missing) but --with-virtio-snd was enabled"

    # Fallback: extract the skip reason token from the marker itself.
    idx = tail.rfind(prefix)
    if idx != -1:
        end = tail.find(b"\n", idx)
        if end == -1:
            end = len(tail)
        reason = tail[idx + len(prefix) : end].strip()
        if reason:
            reason_str = reason.decode("utf-8", errors="replace").strip()
            return f"FAIL: VIRTIO_SND_DUPLEX_SKIPPED: virtio-snd duplex test was skipped ({reason_str}) but --with-virtio-snd was enabled"
    return "FAIL: VIRTIO_SND_DUPLEX_SKIPPED: virtio-snd duplex test was skipped but --with-virtio-snd was enabled"


def _qemu_device_help_text(qemu_system: str, device_name: str) -> str:
    try:
        proc = subprocess.run(
            [qemu_system, "-device", f"{device_name},help"],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            check=False,
        )
    except FileNotFoundError as e:
        raise RuntimeError(f"qemu-system binary not found: {qemu_system}") from e
    except OSError as e:
        raise RuntimeError(f"failed to run '{qemu_system} -device {device_name},help': {e}") from e

    if proc.returncode != 0:
        out = (proc.stdout or "").strip()
        raise RuntimeError(
            f"failed to query QEMU device help for '{device_name}' (exit={proc.returncode}). Output:\n{out}"
        )

    return proc.stdout or ""


def _qemu_has_device(qemu_system: str, device_name: str) -> bool:
    try:
        _qemu_device_help_text(qemu_system, device_name)
        return True
    except RuntimeError:
        return False


def _assert_qemu_supports_aero_w7_virtio_contract_v1(qemu_system: str, *, with_virtio_snd: bool = False) -> None:
    """
    Fail fast with a clear error if the user's QEMU binary can't run the harness in
    a strict AERO-W7-VIRTIO v1 environment.
    """

    required = [
        ("virtio-net-pci", True),
        ("virtio-blk-pci", True),
        ("virtio-keyboard-pci", True),
        ("virtio-mouse-pci", True),
    ]

    if with_virtio_snd:
        required.append((_detect_virtio_snd_device(qemu_system), True))

    for device_name, require_disable_legacy in required:
        help_text = _qemu_device_help_text(qemu_system, device_name)
        if require_disable_legacy and "disable-legacy" not in help_text:
            raise RuntimeError(
                f"QEMU device '{device_name}' does not expose 'disable-legacy'. "
                "AERO-W7-VIRTIO v1 requires modern-only virtio-pci enumeration. Upgrade QEMU."
            )
        if "x-pci-revision" not in help_text:
            raise RuntimeError(
                f"QEMU device '{device_name}' does not expose 'x-pci-revision'. "
                "AERO-W7-VIRTIO v1 requires PCI Revision ID 0x01. Upgrade QEMU."
            )

    if with_virtio_snd:
        device_name = _detect_virtio_snd_device(qemu_system)
        help_text = _qemu_device_help_text(qemu_system, device_name)
        if "disable-legacy" not in help_text:
            raise RuntimeError(
                f"QEMU device '{device_name}' does not expose 'disable-legacy'. "
                "Aero virtio-snd requires modern-only virtio-pci enumeration (DEV_1059). Upgrade QEMU."
            )
        if "x-pci-revision" not in help_text:
            raise RuntimeError(
                f"QEMU device '{device_name}' does not expose 'x-pci-revision'. "
                "Aero virtio-snd contract v1 requires PCI Revision ID 0x01 (REV_01). Upgrade QEMU."
            )


def _detect_virtio_snd_device(qemu_system: str) -> str:
    # QEMU device naming has changed over time. Prefer the modern name but fall back
    # if a distro build exposes a legacy alias.
    try:
        proc = subprocess.run(
            [qemu_system, "-device", "help"],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            check=False,
        )
    except FileNotFoundError as e:
        raise RuntimeError(f"qemu-system binary not found: {qemu_system}") from e
    except OSError as e:
        raise RuntimeError(f"failed to run '{qemu_system} -device help': {e}") from e

    help_text = proc.stdout.decode("utf-8", errors="replace")
    if "virtio-sound-pci" in help_text:
        return "virtio-sound-pci"
    if "virtio-snd-pci" in help_text:
        return "virtio-snd-pci"

    raise RuntimeError(
        "QEMU does not advertise a virtio-snd PCI device (expected 'virtio-sound-pci' or 'virtio-snd-pci'). "
        "Upgrade QEMU or omit --with-virtio-snd/--enable-virtio-snd and pass custom QEMU args."
    )


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
    parser.add_argument(
        "--http-port",
        type=int,
        default=18080,
        help="Host HTTP server port (guest reaches it via slirp at 10.0.2.2:<port>)",
    )
    parser.add_argument(
        "--http-path",
        default="/aero-virtio-selftest",
        help=(
            "HTTP path served by the host harness (e.g. /aero-virtio-selftest). "
            "The guest virtio-net selftest also requests the deterministic large payload at "
            "<http_path>-large (1MiB, bytes 0..255 repeating)."
        ),
    )
    parser.add_argument(
        "--http-log",
        default=None,
        help="Optional path to write HTTP request logs (one line per request)",
    )
    parser.add_argument("--snapshot", action="store_true", help="Discard disk writes (snapshot=on)")
    parser.add_argument(
        "--virtio-transitional",
        action="store_true",
        help="Use transitional virtio-pci devices (legacy + modern). "
        "By default the harness uses modern-only virtio-pci (disable-legacy=on, x-pci-revision=0x01) "
        "so Win7 drivers can bind to the Aero contract v1 IDs. In transitional mode the harness attempts to attach "
        "virtio-keyboard-pci/virtio-mouse-pci when QEMU "
        "advertises those devices; otherwise it warns that the guest virtio-input selftest will likely FAIL.",
    )
    parser.add_argument(
        "--with-input-events",
        "--with-virtio-input-events",
        "--require-virtio-input-events",
        "--enable-virtio-input-events",
        dest="with_input_events",
        action="store_true",
        help=(
            "Inject deterministic keyboard/mouse events via QMP (input-send-event) and require the guest "
            "virtio-input-events selftest marker to PASS. Also emits a host marker: "
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_EVENTS_INJECT|PASS/FAIL|attempt=<n>|kbd_mode=device/broadcast|mouse_mode=device/broadcast "
            "(may appear multiple times due to retries). "
            "This requires a guest image provisioned with --test-input-events (or env var)."
        ),
    )
    parser.add_argument(
        "--with-virtio-snd",
        "--enable-virtio-snd",
        dest="enable_virtio_snd",
        action="store_true",
        help=(
            "Attach a virtio-snd device (virtio-sound-pci). When enabled, the harness requires the guest virtio-snd "
            "selftests (playback + capture + duplex) to PASS (not SKIP)."
        ),
    )
    parser.add_argument(
        "--virtio-snd-audio-backend",
        choices=["none", "wav"],
        default="none",
        help="Audio backend for virtio-snd (default: none)",
    )
    parser.add_argument(
        "--virtio-snd-wav-path",
        default=None,
        help="Output wav path when --virtio-snd-audio-backend=wav",
    )
    parser.add_argument(
        "--virtio-snd-verify-wav",
        action="store_true",
        help=(
            "After QEMU exits, verify the captured wav file contains non-silent audio "
            "(thresholds are expressed in 16-bit PCM units) "
            "(requires --enable-virtio-snd and --virtio-snd-audio-backend=wav)"
        ),
    )
    parser.add_argument(
        "--virtio-snd-wav-peak-threshold",
        type=int,
        default=200,
        help="Peak absolute sample threshold for --virtio-snd-verify-wav in 16-bit PCM units (default: 200)",
    )
    parser.add_argument(
        "--virtio-snd-wav-rms-threshold",
        type=int,
        default=50,
        help="RMS threshold for --virtio-snd-verify-wav in 16-bit PCM units (default: 50)",
    )
    # NOTE: `--with-virtio-input-events`/`--enable-virtio-input-events` used to be separate flags; they remain accepted
    # as aliases for `--with-input-events` for backwards compatibility.
    parser.add_argument(
        "--follow-serial",
        action="store_true",
        help="Stream newly captured COM1 serial output to stdout while waiting",
    )

    # Any remaining args are passed directly to QEMU.
    args, qemu_extra = parser.parse_known_args()
    need_input_events = bool(args.with_input_events)

    if not args.enable_virtio_snd:
        if args.virtio_snd_audio_backend != "none" or args.virtio_snd_wav_path is not None:
            parser.error("--virtio-snd-* options require --with-virtio-snd/--enable-virtio-snd")
    elif args.virtio_snd_audio_backend == "wav" and not args.virtio_snd_wav_path:
        parser.error("--virtio-snd-wav-path is required when --virtio-snd-audio-backend=wav")

    if args.virtio_snd_verify_wav:
        if not args.enable_virtio_snd:
            parser.error("--virtio-snd-verify-wav requires --with-virtio-snd/--enable-virtio-snd")
        if args.virtio_snd_audio_backend != "wav":
            parser.error("--virtio-snd-verify-wav requires --virtio-snd-audio-backend=wav")

    if args.virtio_transitional and args.enable_virtio_snd:
        parser.error(
            "--virtio-transitional is incompatible with --with-virtio-snd/--enable-virtio-snd "
            "(virtio-snd testing requires modern-only virtio-pci + contract revision overrides)"
        )

    if need_input_events:
        # In default (contract-v1) mode we already validate virtio-keyboard-pci/virtio-mouse-pci via
        # `_assert_qemu_supports_aero_w7_virtio_contract_v1`. In transitional mode virtio-input is
        # optional, but input event injection requires these devices to exist.
        if not _qemu_has_device(args.qemu_system, "virtio-keyboard-pci") or not _qemu_has_device(
            args.qemu_system, "virtio-mouse-pci"
        ):
            parser.error(
                "--with-input-events/--with-virtio-input-events requires QEMU virtio-keyboard-pci and virtio-mouse-pci support. "
                "Upgrade QEMU or omit input event injection."
            )

    if not args.virtio_transitional:
        try:
            _assert_qemu_supports_aero_w7_virtio_contract_v1(
                args.qemu_system,
                with_virtio_snd=args.enable_virtio_snd,
            )
        except RuntimeError as e:
            print(f"ERROR: {e}", file=sys.stderr)
            return 2

    disk_image = Path(args.disk_image).resolve()
    if not disk_image.exists():
        print(f"ERROR: disk image not found: {disk_image}", file=sys.stderr)
        return 2
    serial_log = Path(args.serial_log).resolve()
    serial_log.parent.mkdir(parents=True, exist_ok=True)

    if serial_log.exists():
        serial_log.unlink()

    qemu_stderr_log = serial_log.with_name(serial_log.stem + ".qemu.stderr.log")
    try:
        qemu_stderr_log.unlink()
    except FileNotFoundError:
        pass

    # QMP endpoint used to:
    # - request a graceful shutdown (so the wav audiodev can flush/finalize)
    # - optionally inject virtio-input events (keyboard + mouse) via `input-send-event`
    #
    # Historically we enabled QMP only when we needed a graceful exit for `-audiodev wav` output, so we
    # wouldn't introduce extra host port/socket dependencies in non-audio harness runs. Input injection
    # also requires QMP, but remains opt-in via --with-input-events/--with-virtio-input-events.
    use_qmp = (args.enable_virtio_snd and args.virtio_snd_audio_backend == "wav") or need_input_events
    qmp_endpoint: Optional[_QmpEndpoint] = None
    qmp_socket: Optional[Path] = None
    if use_qmp:
        # - On POSIX hosts prefer a UNIX domain socket (avoids picking a TCP port).
        # - Fall back to a loopback TCP socket on Windows (and when the unix socket path is unsafe).
        if os.name != "nt":
            qmp_socket = serial_log.with_name(serial_log.stem + ".qmp.sock")
            # UNIX domain sockets have a small path length limit (typically ~108 bytes). Avoid failing
            # QEMU startup if the user provided an unusually long serial log path.
            qmp_path_str = str(qmp_socket)
            if len(qmp_path_str) >= 100 or "," in qmp_path_str or len(qmp_path_str.encode("utf-8")) >= 100:
                qmp_socket = None
            else:
                try:
                    qmp_socket.unlink()
                except FileNotFoundError:
                    pass
                qmp_endpoint = _QmpEndpoint(unix_socket=qmp_socket)

        if qmp_endpoint is None:
            port = _find_free_tcp_port()
            if port is None:
                if need_input_events:
                    print(
                        "ERROR: --with-input-events/--with-virtio-input-events requires QMP, but a free TCP port could not be allocated",
                        file=sys.stderr,
                    )
                    return 2
                print(
                    "WARNING: disabling QMP shutdown because a free TCP port could not be allocated",
                    file=sys.stderr,
                )
            else:
                qmp_endpoint = _QmpEndpoint(tcp_host="127.0.0.1", tcp_port=port)
    if need_input_events and qmp_endpoint is None:
        print(
            "ERROR: --with-input-events/--with-virtio-input-events requires QMP, but a QMP endpoint could not be allocated",
            file=sys.stderr,
        )
        return 2

    http_log_path: Optional[Path] = None
    if args.http_log:
        try:
            http_log_path = Path(args.http_log).resolve()
            http_log_path.parent.mkdir(parents=True, exist_ok=True)
            if http_log_path.exists():
                http_log_path.unlink()
        except OSError as e:
            print(f"WARNING: failed to prepare HTTP request log at {args.http_log}: {e}", file=sys.stderr)
            http_log_path = None

    handler = type(
        "_Handler",
        (_QuietHandler,),
        {
            "expected_path": args.http_path,
            "http_log_path": http_log_path,
        },
    )

    try:
        httpd = _ReusableTcpServer(("127.0.0.1", args.http_port), handler)
    except OSError as e:
        print(
            f"ERROR: failed to bind HTTP server on 127.0.0.1:{args.http_port} (port in use?): {e}",
            file=sys.stderr,
        )
        return 2

    with httpd:
        thread = Thread(target=httpd.serve_forever, daemon=True)
        thread.start()
        large_path = _append_suffix_before_query_fragment(args.http_path, "-large")
        print(
            f"Starting HTTP server on 127.0.0.1:{args.http_port}{args.http_path} "
            f"(large payload at 127.0.0.1:{args.http_port}{large_path}, 1 MiB deterministic bytes)"
        )
        print(
            f"  Guest URLs: http://10.0.2.2:{args.http_port}{args.http_path} "
            f"and http://10.0.2.2:{args.http_port}{large_path}"
        )

        wav_path: Optional[Path] = None
        if args.virtio_transitional:
            drive = f"file={_qemu_quote_keyval_value(str(disk_image))},if=virtio,cache=writeback"
            if args.snapshot:
                drive += ",snapshot=on"

            virtio_input_args: list[str] = []
            if _qemu_has_device(args.qemu_system, "virtio-keyboard-pci") and _qemu_has_device(
                args.qemu_system, "virtio-mouse-pci"
            ):
                virtio_input_args = [
                    "-device",
                    f"virtio-keyboard-pci,id={_VIRTIO_INPUT_QMP_KEYBOARD_ID}",
                    "-device",
                    f"virtio-mouse-pci,id={_VIRTIO_INPUT_QMP_MOUSE_ID}",
                ]
            else:
                print(
                    "WARNING: QEMU does not advertise virtio-keyboard-pci/virtio-mouse-pci. "
                    "The guest virtio-input selftest will likely FAIL. Upgrade QEMU or adjust the guest image/selftest expectations.",
                    file=sys.stderr,
                )

            virtio_snd_args: list[str] = []
            if args.enable_virtio_snd:
                try:
                    device_arg = _get_qemu_virtio_sound_device_arg(
                        args.qemu_system, disable_legacy=False, pci_revision=None
                    )
                except RuntimeError as e:
                    print(f"ERROR: {e}", file=sys.stderr)
                    return 2

                backend = args.virtio_snd_audio_backend
                if backend == "none":
                    audiodev_arg = "none,id=snd0"
                elif backend == "wav":
                    wav_path = Path(args.virtio_snd_wav_path).resolve()
                    wav_path.parent.mkdir(parents=True, exist_ok=True)
                    try:
                        wav_path.unlink()
                    except FileNotFoundError:
                        pass
                    audiodev_arg = f"wav,id=snd0,path={_qemu_quote_keyval_value(str(wav_path))}"
                else:
                    raise AssertionError(f"Unhandled backend: {backend}")

                virtio_snd_args = ["-audiodev", audiodev_arg, "-device", device_arg]

            serial_chardev = f"file,id=charserial0,path={_qemu_quote_keyval_value(str(serial_log))}"
            qemu_args = [
                args.qemu_system,
                "-m",
                str(args.memory_mb),
                "-smp",
                str(args.smp),
                "-display",
                "none",
                "-no-reboot",
            ]
            if qmp_endpoint is not None:
                qemu_args += ["-qmp", qmp_endpoint.qemu_arg()]
            qemu_args += [
                "-chardev",
                serial_chardev,
                "-serial",
                "chardev:charserial0",
                "-netdev",
                "user,id=net0",
                "-device",
                "virtio-net-pci,netdev=net0",
            ] + virtio_input_args + [
                "-drive",
                drive,
            ] + virtio_snd_args + qemu_extra
        else:
            # Aero contract v1 encodes the major version in the PCI Revision ID (= 0x01).
            #
            # QEMU virtio devices historically report PCI Revision ID 0x00 ("REV_00") even when
            # using modern-only virtio-pci (disable-legacy=on). Strict contract drivers will
            # refuse to bind unless we override the revision ID.
            #
            # QEMU supports overriding PCI revision via the x-pci-revision property.
            aero_pci_rev = "0x01"
            drive_id = "drive0"
            drive = f"file={_qemu_quote_keyval_value(str(disk_image))},if=none,id={drive_id},cache=writeback"
            if args.snapshot:
                drive += ",snapshot=on"

            virtio_net = f"virtio-net-pci,netdev=net0,disable-legacy=on,x-pci-revision={aero_pci_rev}"
            virtio_blk = f"virtio-blk-pci,drive={drive_id},disable-legacy=on,x-pci-revision={aero_pci_rev}"
            virtio_kbd = (
                f"virtio-keyboard-pci,id={_VIRTIO_INPUT_QMP_KEYBOARD_ID},disable-legacy=on,x-pci-revision={aero_pci_rev}"
            )
            virtio_mouse = (
                f"virtio-mouse-pci,id={_VIRTIO_INPUT_QMP_MOUSE_ID},disable-legacy=on,x-pci-revision={aero_pci_rev}"
            )

            virtio_snd_args: list[str] = []
            if args.enable_virtio_snd:
                try:
                    device_arg = _get_qemu_virtio_sound_device_arg(
                        args.qemu_system, disable_legacy=True, pci_revision=aero_pci_rev
                    )
                except RuntimeError as e:
                    print(f"ERROR: {e}", file=sys.stderr)
                    return 2

                backend = args.virtio_snd_audio_backend
                if backend == "none":
                    audiodev_arg = "none,id=snd0"
                elif backend == "wav":
                    wav_path = Path(args.virtio_snd_wav_path).resolve()
                    wav_path.parent.mkdir(parents=True, exist_ok=True)
                    try:
                        wav_path.unlink()
                    except FileNotFoundError:
                        pass
                    audiodev_arg = f"wav,id=snd0,path={_qemu_quote_keyval_value(str(wav_path))}"
                else:
                    raise AssertionError(f"Unhandled backend: {backend}")

                virtio_snd_args = ["-audiodev", audiodev_arg, "-device", device_arg]

            serial_chardev = f"file,id=charserial0,path={_qemu_quote_keyval_value(str(serial_log))}"
            qemu_args = [
                args.qemu_system,
                "-m",
                str(args.memory_mb),
                "-smp",
                str(args.smp),
                "-display",
                "none",
                "-no-reboot",
            ]
            if qmp_endpoint is not None:
                qemu_args += ["-qmp", qmp_endpoint.qemu_arg()]
            qemu_args += [
                "-chardev",
                serial_chardev,
                "-serial",
                "chardev:charserial0",
                "-netdev",
                "user,id=net0",
                "-device",
                virtio_net,
                "-device",
                virtio_kbd,
                "-device",
                virtio_mouse,
                "-drive",
                drive,
                "-device",
                virtio_blk,
            ] + virtio_snd_args + qemu_extra

        print("Launching QEMU:")
        print("  " + " ".join(shlex.quote(str(a)) for a in qemu_args))
        stderr_f = qemu_stderr_log.open("wb")
        proc = subprocess.Popen(qemu_args, stderr=stderr_f)
        result_code: Optional[int] = None
        try:
            pos = 0
            tail = b""
            saw_virtio_blk_pass = False
            saw_virtio_blk_fail = False
            saw_virtio_input_pass = False
            saw_virtio_input_fail = False
            virtio_input_marker_time: Optional[float] = None
            saw_virtio_input_events_ready = False
            saw_virtio_input_events_pass = False
            saw_virtio_input_events_fail = False
            saw_virtio_input_events_skip = False
            input_events_inject_attempts = 0
            next_input_events_inject = 0.0
            saw_virtio_snd_pass = False
            saw_virtio_snd_skip = False
            saw_virtio_snd_fail = False
            saw_virtio_snd_capture_pass = False
            saw_virtio_snd_capture_skip = False
            saw_virtio_snd_capture_fail = False
            saw_virtio_snd_duplex_pass = False
            saw_virtio_snd_duplex_skip = False
            saw_virtio_snd_duplex_fail = False
            saw_virtio_net_pass = False
            saw_virtio_net_fail = False
            require_per_test_markers = not args.virtio_transitional
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

                    if not saw_virtio_blk_pass and b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS" in tail:
                        saw_virtio_blk_pass = True
                    if not saw_virtio_blk_fail and b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|FAIL" in tail:
                        saw_virtio_blk_fail = True
                    if not saw_virtio_input_pass and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input|PASS" in tail:
                        saw_virtio_input_pass = True
                        if virtio_input_marker_time is None:
                            virtio_input_marker_time = time.monotonic()
                    if not saw_virtio_input_fail and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input|FAIL" in tail:
                        saw_virtio_input_fail = True
                        if virtio_input_marker_time is None:
                            virtio_input_marker_time = time.monotonic()
                    if (
                        not saw_virtio_input_events_ready
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|READY" in tail
                    ):
                        saw_virtio_input_events_ready = True
                    if (
                        not saw_virtio_input_events_pass
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|PASS" in tail
                    ):
                        saw_virtio_input_events_pass = True
                    if (
                        not saw_virtio_input_events_fail
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|FAIL" in tail
                    ):
                        saw_virtio_input_events_fail = True
                    if (
                        not saw_virtio_input_events_skip
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|SKIP" in tail
                    ):
                        saw_virtio_input_events_skip = True

                    # If input events are required, fail fast when the guest reports SKIP/FAIL for
                    # virtio-input-events. This saves CI time when the guest image was provisioned
                    # without `--test-input-events`, or when the end-to-end input path is broken.
                    if need_input_events:
                        if saw_virtio_input_events_skip:
                            print(
                                "FAIL: VIRTIO_INPUT_EVENTS_SKIPPED: virtio-input-events test was skipped (flag_not_set) but "
                                "--with-input-events was enabled (provision the guest with --test-input-events)",
                                file=sys.stderr,
                            )
                            _print_tail(serial_log)
                            result_code = 1
                            break
                        if saw_virtio_input_events_fail:
                            print(
                                "FAIL: VIRTIO_INPUT_EVENTS_FAILED: virtio-input-events test reported FAIL while --with-input-events was enabled",
                                file=sys.stderr,
                            )
                            _print_tail(serial_log)
                            result_code = 1
                            break

                    if not saw_virtio_snd_pass and b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd|PASS" in tail:
                        saw_virtio_snd_pass = True
                    if not saw_virtio_snd_skip and b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP" in tail:
                        saw_virtio_snd_skip = True
                    if not saw_virtio_snd_fail and b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL" in tail:
                        saw_virtio_snd_fail = True
                    if (
                        not saw_virtio_snd_capture_pass
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|PASS" in tail
                    ):
                        saw_virtio_snd_capture_pass = True
                    if (
                        not saw_virtio_snd_capture_skip
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP" in tail
                    ):
                        saw_virtio_snd_capture_skip = True
                    if (
                        not saw_virtio_snd_capture_fail
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|FAIL" in tail
                    ):
                        saw_virtio_snd_capture_fail = True
                    if (
                        not saw_virtio_snd_duplex_pass
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|PASS" in tail
                    ):
                        saw_virtio_snd_duplex_pass = True
                    if (
                        not saw_virtio_snd_duplex_skip
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP" in tail
                    ):
                        saw_virtio_snd_duplex_skip = True
                    if (
                        not saw_virtio_snd_duplex_fail
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|FAIL" in tail
                    ):
                        saw_virtio_snd_duplex_fail = True
                    if not saw_virtio_net_pass and b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS" in tail:
                        saw_virtio_net_pass = True
                    if not saw_virtio_net_fail and b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|FAIL" in tail:
                        saw_virtio_net_fail = True

                    if b"AERO_VIRTIO_SELFTEST|RESULT|PASS" in tail:
                        if require_per_test_markers:
                            # Require per-test markers so older selftest binaries cannot
                            # accidentally pass the host harness.
                            if saw_virtio_blk_fail:
                                print(
                                    "FAIL: selftest RESULT=PASS but virtio-blk test reported FAIL",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if not saw_virtio_blk_pass:
                                print(
                                    "FAIL: MISSING_VIRTIO_BLK: selftest RESULT=PASS but did not emit virtio-blk test marker",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if saw_virtio_input_fail:
                                print(
                                    "FAIL: selftest RESULT=PASS but virtio-input test reported FAIL",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if not saw_virtio_input_pass:
                                print(
                                    "FAIL: MISSING_VIRTIO_INPUT: selftest RESULT=PASS but did not emit virtio-input test marker",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if need_input_events:
                                if saw_virtio_input_events_fail:
                                    print(
                                        "FAIL: VIRTIO_INPUT_EVENTS_FAILED: selftest RESULT=PASS but virtio-input-events test reported FAIL "
                                        "while --with-input-events was enabled",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_input_events_pass:
                                    if saw_virtio_input_events_skip:
                                        print(
                                            "FAIL: VIRTIO_INPUT_EVENTS_SKIPPED: virtio-input-events test was skipped (flag_not_set) but "
                                            "--with-input-events was enabled (provision the guest with --test-input-events)",
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(
                                            "FAIL: MISSING_VIRTIO_INPUT_EVENTS: selftest RESULT=PASS but did not emit virtio-input-events test marker "
                                            "while --with-input-events was enabled",
                                            file=sys.stderr,
                                        )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if saw_virtio_snd_fail:
                                print(
                                    "FAIL: selftest RESULT=PASS but virtio-snd test reported FAIL",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break

                            if args.enable_virtio_snd:
                                # When we explicitly attach virtio-snd, the guest test must actually run and PASS
                                # (it must not be skipped via --disable-snd).
                                if not saw_virtio_snd_pass:
                                    msg = "FAIL: virtio-snd test did not PASS while --with-virtio-snd was enabled"
                                    if saw_virtio_snd_skip:
                                        msg = _virtio_snd_skip_failure_message(tail)
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if saw_virtio_snd_capture_fail:
                                    print(
                                        "FAIL: selftest RESULT=PASS but virtio-snd-capture test reported FAIL",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_snd_capture_pass:
                                    msg = "FAIL: virtio-snd capture test did not PASS while --with-virtio-snd was enabled"
                                    if saw_virtio_snd_capture_skip:
                                        msg = _virtio_snd_capture_skip_failure_message(tail)
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if saw_virtio_snd_duplex_fail:
                                    print(
                                        "FAIL: selftest RESULT=PASS but virtio-snd-duplex test reported FAIL",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_snd_duplex_pass:
                                    if saw_virtio_snd_duplex_skip:
                                        msg = _virtio_snd_duplex_skip_failure_message(tail)
                                    else:
                                        msg = (
                                            "FAIL: MISSING_VIRTIO_SND_DUPLEX: selftest RESULT=PASS but did not emit virtio-snd-duplex test marker "
                                            "while --with-virtio-snd was enabled"
                                        )
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            else:
                                # Even when virtio-snd isn't attached, require the markers so older selftest binaries
                                # (that predate virtio-snd playback/capture coverage) cannot accidentally pass.
                                if not (saw_virtio_snd_pass or saw_virtio_snd_skip):
                                    print(
                                        "FAIL: MISSING_VIRTIO_SND: selftest RESULT=PASS but did not emit virtio-snd test marker",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if saw_virtio_snd_capture_fail:
                                    print(
                                        "FAIL: selftest RESULT=PASS but virtio-snd-capture test reported FAIL",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                # Also require the capture marker so selftest binaries that predate virtio-snd-capture
                                # testing cannot accidentally pass the harness.
                                if not (saw_virtio_snd_capture_pass or saw_virtio_snd_capture_skip):
                                    print(
                                        "FAIL: MISSING_VIRTIO_SND_CAPTURE: selftest RESULT=PASS but did not emit virtio-snd-capture test marker",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if saw_virtio_snd_duplex_fail:
                                    print(
                                        "FAIL: selftest RESULT=PASS but virtio-snd-duplex test reported FAIL",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not (saw_virtio_snd_duplex_pass or saw_virtio_snd_duplex_skip):
                                    print(
                                        "FAIL: MISSING_VIRTIO_SND_DUPLEX: selftest RESULT=PASS but did not emit virtio-snd-duplex test marker",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if saw_virtio_net_fail:
                                print(
                                    "FAIL: selftest RESULT=PASS but virtio-net test reported FAIL",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if not saw_virtio_net_pass:
                                print(
                                    "FAIL: MISSING_VIRTIO_NET: selftest RESULT=PASS but did not emit virtio-net test marker",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                        elif args.enable_virtio_snd:
                            # Transitional mode: don't require virtio-input markers, but if the caller
                            # explicitly attached virtio-snd, require the virtio-snd marker to avoid
                            # false positives.
                            if saw_virtio_snd_fail:
                                print(
                                    "FAIL: selftest RESULT=PASS but virtio-snd test reported FAIL",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if not saw_virtio_snd_pass:
                                msg = "FAIL: virtio-snd test did not PASS while --with-virtio-snd was enabled"
                                if saw_virtio_snd_skip:
                                    msg = _virtio_snd_skip_failure_message(tail)
                                print(msg, file=sys.stderr)
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if saw_virtio_snd_capture_fail:
                                print(
                                    "FAIL: selftest RESULT=PASS but virtio-snd-capture test reported FAIL",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if not saw_virtio_snd_capture_pass:
                                msg = "FAIL: virtio-snd capture test did not PASS while --with-virtio-snd was enabled"
                                if saw_virtio_snd_capture_skip:
                                    msg = _virtio_snd_capture_skip_failure_message(tail)
                                print(msg, file=sys.stderr)
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if saw_virtio_snd_duplex_fail:
                                print(
                                    "FAIL: selftest RESULT=PASS but virtio-snd-duplex test reported FAIL",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if not saw_virtio_snd_duplex_pass:
                                if saw_virtio_snd_duplex_skip:
                                    msg = _virtio_snd_duplex_skip_failure_message(tail)
                                else:
                                    msg = (
                                        "FAIL: MISSING_VIRTIO_SND_DUPLEX: selftest RESULT=PASS but did not emit virtio-snd-duplex test marker "
                                        "while --with-virtio-snd was enabled"
                                    )
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                        if need_input_events:
                            if saw_virtio_input_events_fail:
                                print(
                                    "FAIL: VIRTIO_INPUT_EVENTS_FAILED: virtio-input-events test reported FAIL while --with-input-events was enabled",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if not saw_virtio_input_events_pass:
                                if saw_virtio_input_events_skip:
                                    print(
                                        "FAIL: VIRTIO_INPUT_EVENTS_SKIPPED: virtio-input-events test was skipped (flag_not_set) but "
                                        "--with-input-events was enabled (provision the guest with --test-input-events)",
                                        file=sys.stderr,
                                    )
                                else:
                                    print(
                                        "FAIL: MISSING_VIRTIO_INPUT_EVENTS: did not observe virtio-input-events PASS marker while --with-input-events was enabled",
                                        file=sys.stderr,
                                    )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                        print("PASS: AERO_VIRTIO_SELFTEST|RESULT|PASS")
                        result_code = 0
                        break
                    if b"AERO_VIRTIO_SELFTEST|RESULT|FAIL" in tail:
                        print("FAIL: AERO_VIRTIO_SELFTEST|RESULT|FAIL")
                        _print_tail(serial_log)
                        result_code = 1
                        break

                # When requested, inject keyboard/mouse events after the guest has armed the user-mode HID
                # report read loop (virtio-input-events|READY). Inject multiple times on a short interval to
                # reduce flakiness from timing windows (reports may be dropped when no read is pending).
                #
                # If the guest never emits READY/SKIP/PASS/FAIL after completing virtio-input, assume the
                # guest selftest is too old (or misconfigured) and fail early to avoid burning the full
                # virtio-net timeout.
                if (
                    need_input_events
                    and virtio_input_marker_time is not None
                    and not saw_virtio_input_events_ready
                    and not saw_virtio_input_events_pass
                    and not saw_virtio_input_events_fail
                    and not saw_virtio_input_events_skip
                    and time.monotonic() - virtio_input_marker_time > 20.0
                ):
                    print(
                        "FAIL: MISSING_VIRTIO_INPUT_EVENTS: did not observe virtio-input-events marker after virtio-input completed while "
                        "--with-input-events was enabled (guest selftest too old or missing --test-input-events)",
                        file=sys.stderr,
                    )
                    _print_tail(serial_log)
                    result_code = 1
                    break

                if (
                    need_input_events
                    and saw_virtio_input_events_ready
                    and not saw_virtio_input_events_pass
                    and not saw_virtio_input_events_fail
                    and not saw_virtio_input_events_skip
                    and qmp_endpoint is not None
                    and input_events_inject_attempts < 20
                    and time.monotonic() >= next_input_events_inject
                ):
                    input_events_inject_attempts += 1
                    next_input_events_inject = time.monotonic() + 0.5
                    try:
                        info = _try_qmp_input_inject_virtio_input_events(qmp_endpoint)
                        kbd_mode = "broadcast" if info.keyboard_device is None else "device"
                        mouse_mode = "broadcast" if info.mouse_device is None else "device"
                        print(
                            f"AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_EVENTS_INJECT|PASS|attempt={input_events_inject_attempts}|"
                            f"kbd_mode={kbd_mode}|mouse_mode={mouse_mode}"
                        )
                    except Exception as e:
                        reason = _sanitize_marker_value(str(e) or type(e).__name__)
                        print(
                            f"AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_EVENTS_INJECT|FAIL|attempt={input_events_inject_attempts}|reason={reason}",
                            file=sys.stderr,
                        )
                        print(
                            f"FAIL: QMP_INPUT_INJECT_FAILED: failed to inject virtio-input events via QMP: {e}",
                            file=sys.stderr,
                        )
                        _print_tail(serial_log)
                        result_code = 1
                        break

                if proc.poll() is not None:
                    # One last read after exit in case QEMU shut down immediately after writing the marker.
                    chunk2, pos = _read_new_bytes(serial_log, pos)
                    if chunk2:
                        tail += chunk2
                        if not saw_virtio_blk_pass and b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS" in tail:
                            saw_virtio_blk_pass = True
                        if not saw_virtio_blk_fail and b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|FAIL" in tail:
                            saw_virtio_blk_fail = True
                        if not saw_virtio_input_pass and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input|PASS" in tail:
                            saw_virtio_input_pass = True
                        if not saw_virtio_input_fail and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input|FAIL" in tail:
                            saw_virtio_input_fail = True
                        if (
                            not saw_virtio_input_events_ready
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|READY" in tail
                        ):
                            saw_virtio_input_events_ready = True
                        if (
                            not saw_virtio_input_events_pass
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|PASS" in tail
                        ):
                            saw_virtio_input_events_pass = True
                        if (
                            not saw_virtio_input_events_fail
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|FAIL" in tail
                        ):
                            saw_virtio_input_events_fail = True
                        if (
                            not saw_virtio_input_events_skip
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|SKIP" in tail
                        ):
                            saw_virtio_input_events_skip = True
                        if not saw_virtio_snd_pass and b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd|PASS" in tail:
                            saw_virtio_snd_pass = True
                        if not saw_virtio_snd_skip and b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP" in tail:
                            saw_virtio_snd_skip = True
                        if not saw_virtio_snd_fail and b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL" in tail:
                            saw_virtio_snd_fail = True
                        if (
                            not saw_virtio_snd_capture_pass
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|PASS" in tail
                        ):
                            saw_virtio_snd_capture_pass = True
                        if (
                            not saw_virtio_snd_capture_skip
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP" in tail
                        ):
                            saw_virtio_snd_capture_skip = True
                        if (
                            not saw_virtio_snd_capture_fail
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|FAIL" in tail
                        ):
                            saw_virtio_snd_capture_fail = True
                        if (
                            not saw_virtio_snd_duplex_pass
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|PASS" in tail
                        ):
                            saw_virtio_snd_duplex_pass = True
                        if (
                            not saw_virtio_snd_duplex_skip
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP" in tail
                        ):
                            saw_virtio_snd_duplex_skip = True
                        if (
                            not saw_virtio_snd_duplex_fail
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|FAIL" in tail
                        ):
                            saw_virtio_snd_duplex_fail = True
                        if not saw_virtio_net_pass and b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS" in tail:
                            saw_virtio_net_pass = True
                        if not saw_virtio_net_fail and b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|FAIL" in tail:
                            saw_virtio_net_fail = True
                        if b"AERO_VIRTIO_SELFTEST|RESULT|PASS" in tail:
                            if require_per_test_markers:
                                if saw_virtio_blk_fail:
                                    print(
                                        "FAIL: selftest RESULT=PASS but virtio-blk test reported FAIL",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_blk_pass:
                                    print(
                                        "FAIL: MISSING_VIRTIO_BLK: selftest RESULT=PASS but did not emit virtio-blk test marker",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if saw_virtio_input_fail:
                                    print(
                                        "FAIL: selftest RESULT=PASS but virtio-input test reported FAIL",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_input_pass:
                                    print(
                                        "FAIL: MISSING_VIRTIO_INPUT: selftest RESULT=PASS but did not emit virtio-input test marker",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if saw_virtio_snd_fail:
                                    print(
                                        "FAIL: selftest RESULT=PASS but virtio-snd test reported FAIL",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break

                                if args.enable_virtio_snd:
                                    if not saw_virtio_snd_pass:
                                        msg = "FAIL: virtio-snd test did not PASS while --with-virtio-snd was enabled"
                                        if saw_virtio_snd_skip:
                                            msg = _virtio_snd_skip_failure_message(tail)
                                        print(msg, file=sys.stderr)
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                                    if saw_virtio_snd_capture_fail:
                                        print(
                                            "FAIL: selftest RESULT=PASS but virtio-snd-capture test reported FAIL",
                                            file=sys.stderr,
                                        )
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                                    if not saw_virtio_snd_capture_pass:
                                        msg = (
                                            "FAIL: virtio-snd capture test did not PASS while --with-virtio-snd was enabled"
                                        )
                                        if saw_virtio_snd_capture_skip:
                                            msg = _virtio_snd_capture_skip_failure_message(tail)
                                        print(msg, file=sys.stderr)
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                                    if saw_virtio_snd_duplex_fail:
                                        print(
                                            "FAIL: selftest RESULT=PASS but virtio-snd-duplex test reported FAIL",
                                            file=sys.stderr,
                                        )
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                                    if not saw_virtio_snd_duplex_pass:
                                        if saw_virtio_snd_duplex_skip:
                                            msg = _virtio_snd_duplex_skip_failure_message(tail)
                                        else:
                                            msg = (
                                                "FAIL: MISSING_VIRTIO_SND_DUPLEX: selftest RESULT=PASS but did not emit virtio-snd-duplex test marker "
                                                "while --with-virtio-snd was enabled"
                                            )
                                        print(msg, file=sys.stderr)
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                                else:
                                    if not (saw_virtio_snd_pass or saw_virtio_snd_skip):
                                        print(
                                            "FAIL: MISSING_VIRTIO_SND: selftest RESULT=PASS but did not emit virtio-snd test marker",
                                            file=sys.stderr,
                                        )
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                                    if saw_virtio_snd_capture_fail:
                                        print(
                                            "FAIL: selftest RESULT=PASS but virtio-snd-capture test reported FAIL",
                                            file=sys.stderr,
                                        )
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                                    if not (saw_virtio_snd_capture_pass or saw_virtio_snd_capture_skip):
                                        print(
                                            "FAIL: MISSING_VIRTIO_SND_CAPTURE: selftest RESULT=PASS but did not emit virtio-snd-capture test marker",
                                            file=sys.stderr,
                                        )
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                                    if saw_virtio_snd_duplex_fail:
                                        print(
                                            "FAIL: selftest RESULT=PASS but virtio-snd-duplex test reported FAIL",
                                            file=sys.stderr,
                                        )
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                                    if not (saw_virtio_snd_duplex_pass or saw_virtio_snd_duplex_skip):
                                        print(
                                            "FAIL: MISSING_VIRTIO_SND_DUPLEX: selftest RESULT=PASS but did not emit virtio-snd-duplex test marker",
                                            file=sys.stderr,
                                        )
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                                if saw_virtio_net_fail:
                                    print(
                                        "FAIL: selftest RESULT=PASS but virtio-net test reported FAIL",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_net_pass:
                                    print(
                                        "FAIL: MISSING_VIRTIO_NET: selftest RESULT=PASS but did not emit virtio-net test marker",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            elif args.enable_virtio_snd:
                                if saw_virtio_snd_fail:
                                    print(
                                        "FAIL: selftest RESULT=PASS but virtio-snd test reported FAIL",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break

                                if not saw_virtio_snd_pass:
                                    msg = "FAIL: virtio-snd test did not PASS while --with-virtio-snd was enabled"
                                    if saw_virtio_snd_skip:
                                        msg = _virtio_snd_skip_failure_message(tail)
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if saw_virtio_snd_capture_fail:
                                    print(
                                        "FAIL: selftest RESULT=PASS but virtio-snd-capture test reported FAIL",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_snd_capture_pass:
                                    msg = "FAIL: virtio-snd capture test did not PASS while --with-virtio-snd was enabled"
                                    if saw_virtio_snd_capture_skip:
                                        msg = _virtio_snd_capture_skip_failure_message(tail)
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if saw_virtio_snd_duplex_fail:
                                    print(
                                        "FAIL: selftest RESULT=PASS but virtio-snd-duplex test reported FAIL",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_snd_duplex_pass:
                                    if saw_virtio_snd_duplex_skip:
                                        msg = _virtio_snd_duplex_skip_failure_message(tail)
                                    else:
                                        msg = (
                                            "FAIL: MISSING_VIRTIO_SND_DUPLEX: selftest RESULT=PASS but did not emit virtio-snd-duplex test marker "
                                            "while --with-virtio-snd was enabled"
                                        )
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if need_input_events:
                                if saw_virtio_input_events_fail:
                                    print(
                                        "FAIL: VIRTIO_INPUT_EVENTS_FAILED: virtio-input-events test reported FAIL while --with-input-events was enabled",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_input_events_pass:
                                    if saw_virtio_input_events_skip:
                                        print(
                                            "FAIL: VIRTIO_INPUT_EVENTS_SKIPPED: virtio-input-events test was skipped (flag_not_set) but "
                                            "--with-input-events was enabled (provision the guest with --test-input-events)",
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(
                                            "FAIL: MISSING_VIRTIO_INPUT_EVENTS: did not observe virtio-input-events PASS marker while --with-input-events was enabled",
                                            file=sys.stderr,
                                        )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            print("PASS: AERO_VIRTIO_SELFTEST|RESULT|PASS")
                            result_code = 0
                            break
                        if b"AERO_VIRTIO_SELFTEST|RESULT|FAIL" in tail:
                            print("FAIL: AERO_VIRTIO_SELFTEST|RESULT|FAIL")
                            _print_tail(serial_log)
                            result_code = 1
                            break

                    print(f"FAIL: QEMU exited before selftest result marker (exit code: {proc.returncode})")
                    _print_tail(serial_log)
                    _print_qemu_stderr_tail(qemu_stderr_log)
                    result_code = 3
                    break

                time.sleep(0.25)

            if result_code is None:
                print("FAIL: timed out waiting for AERO_VIRTIO_SELFTEST result marker")
                _print_tail(serial_log)
                result_code = 2
        finally:
            # Prefer a graceful QMP shutdown so that the wav audiodev backend can finalize its header.
            if proc.poll() is None:
                if qmp_endpoint is not None and _try_qmp_quit(qmp_endpoint):
                    try:
                        proc.wait(timeout=10)
                    except Exception:
                        _stop_process(proc)
                else:
                    _stop_process(proc)
            httpd.shutdown()
            try:
                stderr_f.close()
            except Exception:
                pass
            if qmp_socket is not None:
                try:
                    qmp_socket.unlink()
                except FileNotFoundError:
                    pass
                except OSError:
                    # Best-effort cleanup.
                    pass

        if args.virtio_snd_verify_wav:
            if wav_path is None:
                raise AssertionError("--virtio-snd-verify-wav requires --virtio-snd-audio-backend=wav")
            ok = _verify_virtio_snd_wav_non_silent(
                wav_path,
                peak_threshold=args.virtio_snd_wav_peak_threshold,
                rms_threshold=args.virtio_snd_wav_rms_threshold,
            )
            if not ok and result_code == 0:
                # Surface host-side audio failures even if the guest selftest passed.
                result_code = 4

        _emit_virtio_net_large_host_marker(tail)

        return result_code if result_code is not None else 2


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


def _print_qemu_stderr_tail(path: Path) -> None:
    try:
        data = path.read_bytes()
    except FileNotFoundError:
        return
    except OSError:
        return

    if not data:
        return

    tail = data[-8192:]
    sys.stdout.write("\n--- QEMU stderr tail ---\n")
    try:
        sys.stdout.write(tail.decode("utf-8", errors="replace"))
    except Exception:
        sys.stdout.buffer.write(tail)


def _get_qemu_virtio_sound_device_arg(
    qemu_system: str, *, disable_legacy: bool, pci_revision: Optional[str]
) -> str:
    """
    Return the virtio-snd PCI device arg string.

    The Aero Win7 virtio-snd contract v1 expects the modern virtio-pci ID space (`DEV_1059`) and
    PCI Revision ID 0x01 (`REV_01`). The canonical Win7 INF (`aero_virtio_snd.inf`) is intentionally
    strict and matches only `PCI\\VEN_1AF4&DEV_1059&REV_01`.

    Callers choose whether to force modern-only enumeration (`disable-legacy=on`) and a specific
    contract revision (`x-pci-revision=<...>`). The strict host harness path passes both.
    """
    device_name = _detect_virtio_snd_device(qemu_system)
    help_text = _qemu_device_help_text(qemu_system, device_name)
    if disable_legacy and "disable-legacy" not in help_text:
        raise RuntimeError(
            f"QEMU device '{device_name}' does not expose 'disable-legacy'. "
            "Aero virtio-snd requires modern-only virtio-pci enumeration (DEV_1059). Upgrade QEMU."
        )
    if pci_revision is not None and "x-pci-revision" not in help_text:
        raise RuntimeError(
            f"QEMU device '{device_name}' does not expose 'x-pci-revision'. "
            "Aero virtio-snd contract v1 requires PCI Revision ID 0x01 (REV_01). Upgrade QEMU."
        )

    parts = [device_name, "audiodev=snd0"]
    if disable_legacy:
        parts.append("disable-legacy=on")
    if pci_revision is not None:
        parts.append(f"x-pci-revision={pci_revision}")
    return ",".join(parts)


def _looks_like_chunk_id(chunk_id: bytes) -> bool:
    if len(chunk_id) != 4:
        return False
    # Most RIFF chunk IDs are ASCII (e.g. b"fmt ", b"data", b"LIST"). Treat this as a heuristic only.
    return all(0x20 <= b <= 0x7E for b in chunk_id)


def _parse_wave_file(path: Path) -> _WaveFileInfo:
    file_size = path.stat().st_size
    if file_size <= 0:
        raise ValueError("wav file is empty")

    with path.open("rb") as f:
        header = f.read(12)
        if len(header) < 12:
            raise ValueError("wav file too small for RIFF header")
        if header[0:4] != b"RIFF":
            raise ValueError("missing RIFF header")
        if header[8:12] != b"WAVE":
            raise ValueError("missing WAVE form type")

        fmt: Optional[_WaveFmtChunk] = None
        data_offset: Optional[int] = None
        data_size: Optional[int] = None

        while True:
            chunk_hdr = f.read(8)
            if len(chunk_hdr) == 0:
                break
            if len(chunk_hdr) < 8:
                break

            chunk_id = chunk_hdr[0:4]
            chunk_size = struct.unpack("<I", chunk_hdr[4:8])[0]
            chunk_data_start = f.tell()
            remaining_in_file = max(0, file_size - chunk_data_start)

            # Clamp chunk size to the actual file length to avoid seek errors on truncated writes.
            if chunk_size > remaining_in_file:
                chunk_size = remaining_in_file

            if chunk_id == b"fmt ":
                if chunk_size < 16:
                    raise ValueError("fmt chunk too small")
                fmt_head = f.read(16)
                if len(fmt_head) < 16:
                    raise ValueError("truncated fmt chunk")
                (
                    w_format_tag,
                    n_channels,
                    n_samples_per_sec,
                    _n_avg_bytes_per_sec,
                    n_block_align,
                    w_bits_per_sample,
                ) = struct.unpack("<HHIIHH", fmt_head)
                subformat: Optional[bytes] = None
                # WAVE_FORMAT_EXTENSIBLE (0xFFFE) appends the SubFormat GUID after the base
                # WAVEFORMATEX fields (common for tools that always emit extensible WAV).
                if w_format_tag == 0xFFFE and chunk_size >= 40:
                    ext = f.read(24)
                    if len(ext) < 24:
                        raise ValueError("truncated fmt extensible chunk")
                    subformat = ext[8:24]
                    if chunk_size > 40:
                        f.seek(chunk_size - 40, os.SEEK_CUR)
                elif chunk_size > 16:
                    f.seek(chunk_size - 16, os.SEEK_CUR)
                fmt = _WaveFmtChunk(
                    format_tag=w_format_tag,
                    channels=n_channels,
                    sample_rate=n_samples_per_sec,
                    block_align=n_block_align,
                    bits_per_sample=w_bits_per_sample,
                    subformat=subformat,
                )
            elif chunk_id == b"data":
                # QEMU normally finalizes the data chunk size on graceful exit. If QEMU is killed hard, it
                # may leave a placeholder (0). Recover by treating the rest of the file as audio data when it
                # doesn't look like another valid RIFF chunk header.
                effective_size = chunk_size
                if chunk_size == 0 and remaining_in_file > 0:
                    peek = f.read(min(8, remaining_in_file))
                    f.seek(-len(peek), os.SEEK_CUR)
                    if len(peek) < 8 or not _looks_like_chunk_id(peek[0:4]):
                        effective_size = remaining_in_file
                    else:
                        next_size = struct.unpack("<I", peek[4:8])[0]
                        if next_size > remaining_in_file - 8:
                            effective_size = remaining_in_file

                if data_offset is None or (data_size is not None and data_size == 0 and effective_size > 0):
                    data_offset = chunk_data_start
                    data_size = effective_size

                f.seek(effective_size, os.SEEK_CUR)
            else:
                f.seek(chunk_size, os.SEEK_CUR)

            # Chunks are padded to an even boundary.
            if chunk_size % 2 == 1 and f.tell() < file_size:
                f.seek(1, os.SEEK_CUR)

        if fmt is None:
            raise ValueError("missing fmt chunk")
        if data_offset is None or data_size is None:
            raise ValueError("missing data chunk")

        return _WaveFileInfo(fmt=fmt, data_offset=data_offset, data_size=data_size)


def _compute_pcm16_metrics(path: Path, data_offset: int, data_size: int) -> tuple[int, float, int]:
    peak = 0
    sum_sq = 0
    count = 0

    with path.open("rb") as f:
        f.seek(data_offset)
        remaining = data_size
        carry = b""

        while remaining > 0:
            chunk = f.read(min(remaining, 1024 * 1024))
            if not chunk:
                break
            remaining -= len(chunk)

            if carry:
                chunk = carry + chunk
                carry = b""
            if len(chunk) % 2 == 1:
                carry = chunk[-1:]
                chunk = chunk[:-1]
            if not chunk:
                continue

            arr = array("h")
            arr.frombytes(chunk)
            if sys.byteorder != "little":
                arr.byteswap()

            for s in arr:
                abs_s = -s if s < 0 else s
                if abs_s > peak:
                    peak = abs_s
                sum_sq += s * s
            count += len(arr)

    rms = math.sqrt(sum_sq / count) if count else 0.0
    return peak, rms, count


def _compute_pcm_metrics_16bit_equiv_audioop(
    path: Path, data_offset: int, data_size: int, *, bits_per_sample: int
) -> Optional[tuple[int, float, int]]:
    """
    Fast 16-bit-equivalent PCM metrics using `audioop` (C implementation).

    `audioop` is part of the Python standard library on most CPython builds, but it is deprecated
    and may be missing in some environments. This helper returns `None` when it cannot be used so
    callers can fall back to the pure-Python implementation.
    """
    if bits_per_sample <= 0 or bits_per_sample % 8 != 0:
        raise ValueError(f"unsupported bits_per_sample {bits_per_sample}")
    sample_bytes = bits_per_sample // 8

    try:
        with warnings.catch_warnings():
            warnings.simplefilter("ignore", DeprecationWarning)
            import audioop  # type: ignore
    except Exception:
        return None

    peak = 0
    sum_sq = 0.0
    count = 0

    with path.open("rb") as f:
        f.seek(data_offset)
        remaining = data_size
        carry = b""

        while remaining > 0:
            chunk = f.read(min(remaining, 1024 * 1024))
            if not chunk:
                break
            remaining -= len(chunk)

            if carry:
                chunk = carry + chunk
                carry = b""

            rem = len(chunk) % sample_bytes
            if rem:
                carry = chunk[-rem:]
                chunk = chunk[:-rem]
            if not chunk:
                continue

            try:
                # WAV 8-bit PCM is unsigned (silence is 0x80), while `audioop` treats 8-bit
                # samples as signed. Bias first so silence becomes 0 before converting.
                if sample_bytes == 1:
                    chunk = audioop.bias(chunk, 1, -128)
                frag16 = chunk if sample_bytes == 2 else audioop.lin2lin(chunk, sample_bytes, 2)
                chunk_peak = audioop.max(frag16, 2)
                if chunk_peak > peak:
                    peak = chunk_peak
                chunk_rms = audioop.rms(frag16, 2)
            except Exception:
                # Fall back to the pure-Python path if the audioop pipeline rejects the fragment.
                return None

            samples = len(frag16) // 2
            count += samples
            sum_sq += float(chunk_rms) * float(chunk_rms) * float(samples)

    rms = math.sqrt(sum_sq / count) if count else 0.0
    return peak, rms, count


def _compute_wave_metrics_16bit_equiv(
    path: Path, data_offset: int, data_size: int, *, kind: str, bits_per_sample: int
) -> tuple[int, float, int]:
    """
    Compute peak/RMS in 16-bit PCM-equivalent units.

    The harness thresholds are specified in 16-bit sample units (0..32767). QEMU's wav backend is
    usually 16-bit PCM, but some builds emit WAVE_FORMAT_EXTENSIBLE (and potentially IEEE float).
    This helper keeps verification deterministic across those variants by converting to a 16-bit
    equivalent scale.
    """
    if kind == "pcm":
        if bits_per_sample == 16:
            # Prefer `audioop` for speed, but keep the pure-Python fallback for environments
            # where `audioop` is unavailable or refuses the fragment.
            audioop_metrics = _compute_pcm_metrics_16bit_equiv_audioop(
                path, data_offset, data_size, bits_per_sample=bits_per_sample
            )
            if audioop_metrics is not None:
                return audioop_metrics
            return _compute_pcm16_metrics(path, data_offset, data_size)

        audioop_metrics = _compute_pcm_metrics_16bit_equiv_audioop(
            path, data_offset, data_size, bits_per_sample=bits_per_sample
        )
        if audioop_metrics is not None:
            return audioop_metrics

    if bits_per_sample <= 0 or bits_per_sample % 8 != 0:
        raise ValueError(f"unsupported bits_per_sample {bits_per_sample}")

    sample_bytes = bits_per_sample // 8
    peak_f = 0.0
    sum_sq = 0.0
    count = 0

    with path.open("rb") as f:
        f.seek(data_offset)
        remaining = data_size
        carry = b""

        while remaining > 0:
            chunk = f.read(min(remaining, 1024 * 1024))
            if not chunk:
                break
            remaining -= len(chunk)

            if carry:
                chunk = carry + chunk
                carry = b""

            rem = len(chunk) % sample_bytes
            if rem:
                carry = chunk[-rem:]
                chunk = chunk[:-rem]
            if not chunk:
                continue

            mv = memoryview(chunk)

            if kind == "pcm":
                if bits_per_sample == 8:
                    # 8-bit PCM is unsigned; silence is 0x80.
                    for b in mv:
                        s = float(int(b) - 128) / 128.0  # approx [-1.0, 1.0)
                        v = s * 32767.0
                        av = -v if v < 0.0 else v
                        if av > peak_f:
                            peak_f = av
                        sum_sq += v * v
                    count += len(mv)
                elif bits_per_sample == 24:
                    denom = float(1 << 23)
                    for i in range(0, len(mv), 3):
                        raw = int.from_bytes(mv[i : i + 3], "little", signed=True)
                        v = (float(raw) / denom) * 32767.0
                        av = -v if v < 0.0 else v
                        if av > peak_f:
                            peak_f = av
                        sum_sq += v * v
                        count += 1
                elif bits_per_sample == 32:
                    denom = float(1 << 31)
                    for i in range(0, len(mv), 4):
                        raw = int.from_bytes(mv[i : i + 4], "little", signed=True)
                        v = (float(raw) / denom) * 32767.0
                        av = -v if v < 0.0 else v
                        if av > peak_f:
                            peak_f = av
                        sum_sq += v * v
                        count += 1
                else:
                    raise ValueError(f"unsupported PCM bits_per_sample {bits_per_sample}")
            elif kind == "float":
                if bits_per_sample != 32:
                    raise ValueError(f"unsupported float bits_per_sample {bits_per_sample}")
                for i in range(0, len(mv), 4):
                    (raw_f,) = struct.unpack_from("<f", mv, i)
                    if not math.isfinite(raw_f):
                        continue
                    v = float(raw_f) * 32767.0
                    av = -v if v < 0.0 else v
                    if av > peak_f:
                        peak_f = av
                    sum_sq += v * v
                    count += 1
            else:
                raise ValueError(f"unknown wav sample kind {kind}")

    peak = int(round(peak_f))
    rms = math.sqrt(sum_sq / count) if count else 0.0
    return peak, rms, count


def _sanitize_marker_value(value: str) -> str:
    return value.replace("|", "/").replace("\n", " ").replace("\r", " ").strip()


def _try_extract_last_marker_line(tail: bytes, prefix: bytes) -> Optional[str]:
    """
    Return the last full line in `tail` that starts with `prefix`.

    The returned line is decoded as UTF-8 with replacement and stripped.
    """
    last: Optional[bytes] = None
    for line in tail.splitlines():
        if line.startswith(prefix):
            last = line
    if last is None:
        return None
    try:
        return last.decode("utf-8", errors="replace").strip()
    except Exception:
        return None


def _parse_marker_kv_fields(marker_line: str) -> dict[str, str]:
    """
    Parse a marker line with `|` separators and `key=value` fields.

    Example:
      AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS|large_ok=1|large_bytes=1048576|...
    """
    out: dict[str, str] = {}
    for tok in marker_line.split("|"):
        if "=" not in tok:
            continue
        k, v = tok.split("=", 1)
        k = k.strip()
        v = v.strip()
        if not k:
            continue
        out[k] = v
    return out


def _emit_virtio_net_large_host_marker(tail: bytes) -> None:
    """
    Best-effort: emit a host-side marker describing the guest's virtio-net large transfer metrics.

    This does not affect harness PASS/FAIL; it's only for log scraping/diagnostics.
    """
    marker_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|")
    if marker_line is None:
        return

    fields = _parse_marker_kv_fields(marker_line)
    if (
        "large_bytes" not in fields
        and "large_mbps" not in fields
        and "large_fnv1a64" not in fields
        and "upload_ok" not in fields
        and "upload_bytes" not in fields
        and "upload_mbps" not in fields
    ):
        return

    status = "INFO"
    # Prefer the overall marker PASS/FAIL token so this stays correct even when the
    # large download passes but the optional large upload fails (TX vs RX stress).
    if "FAIL" in marker_line.split("|"):
        status = "FAIL"
    elif "PASS" in marker_line.split("|"):
        status = "PASS"
    elif fields.get("large_ok") == "0" or fields.get("upload_ok") == "0":
        status = "FAIL"
    elif fields.get("large_ok") == "1" and fields.get("upload_ok", "1") == "1":
        status = "PASS"

    parts = [f"AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_LARGE|{status}"]
    for k in ("large_ok", "large_bytes", "large_fnv1a64", "large_mbps", "upload_ok", "upload_bytes", "upload_mbps"):
        if k in fields:
            parts.append(f"{k}={_sanitize_marker_value(fields[k])}")
    print("|".join(parts))


def _verify_virtio_snd_wav_non_silent(path: Path, *, peak_threshold: int, rms_threshold: int) -> bool:
    marker_prefix = "AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_WAV"
    try:
        if not path.exists():
            print(f"{marker_prefix}|FAIL|reason=missing_wav_file|path={_sanitize_marker_value(str(path))}")
            return False

        if path.stat().st_size <= 0:
            print(f"{marker_prefix}|FAIL|reason=empty_wav_file|path={_sanitize_marker_value(str(path))}")
            return False

        info = _parse_wave_file(path)
        fmt = info.fmt

        # WAVE_FORMAT_EXTENSIBLE SubFormat GUIDs (little-endian GUID struct layout).
        k_subformat_pcm = b"\x01\x00\x00\x00\x00\x00\x10\x00\x80\x00\x00\xaa\x00\x38\x9b\x71"
        k_subformat_float = b"\x03\x00\x00\x00\x00\x00\x10\x00\x80\x00\x00\xaa\x00\x38\x9b\x71"

        kind: Optional[str] = None
        if fmt.format_tag == 1:  # WAVE_FORMAT_PCM
            kind = "pcm"
        elif fmt.format_tag == 3:  # WAVE_FORMAT_IEEE_FLOAT
            kind = "float"
        elif fmt.format_tag == 0xFFFE:  # WAVE_FORMAT_EXTENSIBLE
            if fmt.subformat == k_subformat_pcm:
                kind = "pcm"
            elif fmt.subformat == k_subformat_float:
                kind = "float"
            elif fmt.subformat is None:
                print(f"{marker_prefix}|FAIL|reason=unsupported_extensible_missing_subformat")
                return False
            else:
                sub = _sanitize_marker_value(fmt.subformat.hex())
                print(f"{marker_prefix}|FAIL|reason=unsupported_extensible_subformat_{sub}")
                return False
        else:
            print(f"{marker_prefix}|FAIL|reason=unsupported_format_tag_{fmt.format_tag}")
            return False

        if kind == "pcm" and fmt.bits_per_sample not in (8, 16, 24, 32):
            print(f"{marker_prefix}|FAIL|reason=unsupported_bits_per_sample_{fmt.bits_per_sample}")
            return False
        if kind == "float" and fmt.bits_per_sample != 32:
            print(f"{marker_prefix}|FAIL|reason=unsupported_bits_per_sample_{fmt.bits_per_sample}")
            return False
        if info.data_size <= 0:
            print(f"{marker_prefix}|FAIL|reason=missing_or_empty_data_chunk")
            return False

        assert kind is not None
        peak, rms, sample_values = _compute_wave_metrics_16bit_equiv(
            path, info.data_offset, info.data_size, kind=kind, bits_per_sample=fmt.bits_per_sample
        )
        rms_i = int(round(rms))
        frames = sample_values // fmt.channels if fmt.channels else sample_values

        ok = peak > peak_threshold or rms > rms_threshold
        if ok:
            print(
                f"{marker_prefix}|PASS|peak={peak}|rms={rms_i}|samples={frames}|sr={fmt.sample_rate}|ch={fmt.channels}"
            )
            return True

        print(
            f"{marker_prefix}|FAIL|reason=silent_pcm|peak={peak}|rms={rms_i}|samples={frames}|sr={fmt.sample_rate}|ch={fmt.channels}"
        )
        return False
    except Exception as e:
        reason = _sanitize_marker_value(str(e) or type(e).__name__)
        print(f"{marker_prefix}|FAIL|reason={reason}")
        return False


if __name__ == "__main__":
    raise SystemExit(main())
