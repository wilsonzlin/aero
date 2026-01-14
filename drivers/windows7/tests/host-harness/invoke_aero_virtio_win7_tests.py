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
- starts a tiny UDP echo server on 127.0.0.1:<udp_port> (guest reaches it as 10.0.2.2:<udp_port> via slirp)
  - echoes exactly what was received (bounded max datagram size)
- launches QEMU with virtio-blk + virtio-net + virtio-input (and optionally virtio-snd and/or virtio-tablet) and COM1 redirected to a log file
  - in transitional mode virtio-input is skipped (with a warning) if QEMU does not advertise virtio-keyboard-pci/virtio-mouse-pci
 - captures QEMU stderr to `<serial-base>.qemu.stderr.log` (next to the serial log) for debugging early exits
- optionally enables a QMP monitor to:
  - request a graceful QEMU shutdown so side-effectful devices (notably the `wav` audiodev backend) can flush/finalize
    their output files before verification
  - trigger a virtio-blk runtime resize via `blockdev-resize` / legacy `block_resize` (when `--with-blk-resize` is enabled)
  - inject deterministic virtio-input events via `input-send-event` (when `--with-input-events` /
    `--with-virtio-input-events` or `--with-input-tablet-events`/`--with-tablet-events` is enabled)
  (unix socket on POSIX; TCP loopback fallback on Windows)
 - tails the serial log until it sees AERO_VIRTIO_SELFTEST|RESULT|PASS/FAIL
   - in default (non-transitional) mode, a PASS result also requires per-test markers for virtio-blk, virtio-input,
      virtio-snd (PASS or SKIP), virtio-snd-capture (PASS or SKIP), virtio-snd-duplex (PASS or SKIP), and virtio-net
     and virtio-net-udp so older selftest binaries cannot accidentally pass
   - when --with-virtio-snd is enabled, virtio-snd, virtio-snd-capture, and virtio-snd-duplex must PASS (not SKIP)
   - when --with-input-events (alias: --with-virtio-input-events) is enabled, virtio-input-events must PASS (not FAIL/missing)
  - when --with-input-tablet-events/--with-tablet-events is enabled, virtio-input-tablet-events must PASS (not FAIL/missing)
  - when --with-blk-resize is enabled, virtio-blk-resize must PASS (not SKIP/FAIL/missing)

For convenience when scraping CI logs, the harness may also emit a host-side virtio-net marker when the guest
includes large-transfer fields:

- `AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_LARGE|PASS/FAIL/INFO|large_ok=...|large_bytes=...|large_fnv1a64=...|large_mbps=...|upload_ok=...|upload_bytes=...|upload_mbps=...`
  - also mirrors best-effort interrupt allocation fields when present:
    `msi=...|msi_messages=...`

It may also mirror guest-side IRQ diagnostics (when present) into per-device host markers:

- `AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ|PASS/FAIL/INFO|irq_mode=...|irq_message_count=...|msix_config_vector=...|msix_queue_vector=...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_IRQ|PASS/FAIL/INFO|irq_mode=...|irq_message_count=...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_IRQ|PASS/FAIL/INFO|irq_mode=...|irq_message_count=...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_IRQ|PASS/FAIL/INFO|irq_mode=...|irq_message_count=...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_EVENTQ|INFO/SKIP|completions=...|pcm_period=...|xrun=...|...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_FORMAT|INFO|render=...|capture=...`

It also mirrors the standalone guest IRQ diagnostic lines (when present):

- `AERO_VIRTIO_WIN7_HOST|VIRTIO_*_IRQ_DIAG|INFO/WARN|...`
"""

from __future__ import annotations

import argparse
import http.server
import hashlib
import json
import math
import re
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
from functools import lru_cache
from pathlib import Path
from threading import Event, Thread
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


class _UdpEchoServer:
    """
    Deterministic UDP echo server for virtio-net selftests.

    The guest (via QEMU slirp) reaches the host loopback address 127.0.0.1 as 10.0.2.2.
    """

    def __init__(
        self,
        host: str,
        port: int,
        *,
        max_datagram_size: int = 2048,
        socket_timeout_seconds: float = 0.5,
    ) -> None:
        self._host = host
        self._port = int(port)
        self._max_datagram_size = int(max_datagram_size)
        self._socket_timeout_seconds = float(socket_timeout_seconds)
        self._stop = Event()
        self._sock: Optional[socket.socket] = None
        self._thread: Optional[Thread] = None

    @property
    def port(self) -> int:
        if self._sock is None:
            return self._port
        return int(self._sock.getsockname()[1])

    def __enter__(self) -> "_UdpEchoServer":
        self.start()
        return self

    def __exit__(self, exc_type, exc, tb) -> None:  # type: ignore[override]
        self.close()

    def start(self) -> None:
        if self._sock is not None:
            return
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        try:
            sock.bind((self._host, self._port))
            sock.settimeout(self._socket_timeout_seconds)
        except Exception:
            try:
                sock.close()
            except Exception:
                pass
            raise

        self._sock = sock
        self._thread = Thread(target=self._run, daemon=True)
        self._thread.start()

    def close(self) -> None:
        self._stop.set()
        if self._sock is not None:
            try:
                self._sock.close()
            except Exception:
                pass
        if self._thread is not None:
            self._thread.join(timeout=2.0)
        self._sock = None
        self._thread = None

    def _run(self) -> None:
        sock = self._sock
        if sock is None:
            return

        # Read up to max_datagram_size + 1 to detect oversize datagrams (drop them).
        recv_size = max(1, self._max_datagram_size + 1)

        while not self._stop.is_set():
            try:
                data, addr = sock.recvfrom(recv_size)
            except socket.timeout:
                continue
            except OSError:
                # Socket closed or other fatal error.
                return
            except Exception:
                return

            if not data:
                continue
            if len(data) > self._max_datagram_size:
                # Drop oversize datagrams (bounded/deterministic).
                continue
            try:
                sock.sendto(data, addr)
            except Exception:
                # Best-effort echo; never fail the harness due to send errors.
                continue


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


@dataclass(frozen=True)
class _PciMsixInfo:
    vendor_id: int
    device_id: int
    bus: Optional[int]
    slot: Optional[int]
    function: Optional[int]
    msix_enabled: Optional[bool]
    # Where this info was sourced from (e.g. "query-pci" or "info pci").
    source: str

    def bdf(self) -> str:
        if self.bus is None or self.slot is None or self.function is None:
            return "?:?.?"
        return f"{self.bus:02x}:{self.slot:02x}.{self.function}"

    def pci_id(self) -> str:
        return f"{self.vendor_id:04x}:{self.device_id:04x}"


@dataclass(frozen=True)
class _PciId:
    vendor_id: int
    device_id: int
    subsystem_vendor_id: Optional[int]
    subsystem_id: Optional[int]
    revision: Optional[int]


def _iter_qmp_query_pci_devices(query_pci_result: object) -> list[_PciId]:
    """
    Attempt to extract vendor/device/subsystem/revision from QMP `query-pci` output.

    QEMU's `query-pci` QMP schema is stable but can vary slightly between versions; we treat
    unknown/missing fields as optional and ignore devices we can't parse.
    """
    devices: list[_PciId] = []

    def _as_int(v: object) -> Optional[int]:
        if isinstance(v, int):
            return v
        if isinstance(v, str):
            try:
                return int(v, 0)
            except ValueError:
                return None
        return None

    if not isinstance(query_pci_result, list):
        return devices

    for bus in query_pci_result:
        if not isinstance(bus, dict):
            continue
        bus_devices = bus.get("devices")
        if not isinstance(bus_devices, list):
            continue
        for dev in bus_devices:
            if not isinstance(dev, dict):
                continue

            vendor = _as_int(dev.get("vendor_id"))
            device = _as_int(dev.get("device_id"))
            if vendor is None or device is None:
                continue

            subsys_vendor = _as_int(dev.get("subsystem_vendor_id"))
            subsys = _as_int(dev.get("subsystem_id"))
            rev = _as_int(dev.get("revision"))
            devices.append(_PciId(vendor, device, subsys_vendor, subsys, rev))

    return devices


def _summarize_pci_ids(devices: list[_PciId]) -> str:
    """
    Return a compact, comma-separated summary of vendor/device/rev triples.

    Example: "1af4:1041@01,1af4:1052@01"
    """
    seen: set[tuple[int, int, Optional[int]]] = set()
    for d in devices:
        seen.add((d.vendor_id, d.device_id, d.revision))
    parts: list[str] = []
    for ven, dev, rev in sorted(seen, key=lambda t: (t[0], t[1], -1 if t[2] is None else int(t[2]))):
        rev_str = "??" if rev is None else f"{rev:02x}"
        parts.append(f"{ven:04x}:{dev:04x}@{rev_str}")
    return ",".join(parts)


def _format_pci_id_dump(devices: list[_PciId], *, max_lines: int = 32) -> str:
    """
    Return a human-readable multi-line dump of vendor/device/subsys/rev.
    """
    lines: list[str] = []
    for d in sorted(
        devices,
        key=lambda x: (
            x.vendor_id,
            x.device_id,
            -1 if x.subsystem_id is None else int(x.subsystem_id),
            -1 if x.revision is None else int(x.revision),
        ),
    ):
        rev_str = "?" if d.revision is None else f"0x{d.revision:02x}"
        subsys_str = "?:?" if d.subsystem_vendor_id is None or d.subsystem_id is None else (
            f"0x{d.subsystem_vendor_id:04x}:0x{d.subsystem_id:04x}"
        )
        lines.append(f"0x{d.vendor_id:04x}:0x{d.device_id:04x} subsys={subsys_str} rev={rev_str}")
    if not lines:
        return "  (no devices parsed from query-pci output)"
    if len(lines) > max_lines:
        extra = len(lines) - max_lines
        lines = lines[:max_lines]
        lines.append(f"... ({extra} more)")
    return "\n".join("  " + l for l in lines)


def _qmp_query_pci(endpoint: _QmpEndpoint) -> object:
    with _qmp_connect(endpoint, timeout_seconds=5.0) as s:
        resp = _qmp_send_command(s, {"execute": "query-pci"})
        return resp.get("return")


def _qmp_pci_preflight(
    endpoint: _QmpEndpoint,
    *,
    virtio_transitional: bool,
    with_virtio_snd: bool,
    with_virtio_tablet: bool,
) -> None:
    """
    Validate that QEMU is exposing the expected virtio PCI IDs for the Win7 harness.

    In the default (contract v1) mode we require:
      - VEN_1AF4 (virtio)
      - DEV_1041 (net), DEV_1042 (blk), DEV_1052 (virtio-input keyboard + mouse [+ tablet])
      - DEV_1059 (virtio-snd) when enabled
      - REV_01 for those devices (Aero contract major version gating)

    In transitional mode, we still assert that virtio (VEN_1AF4) devices exist, but we do not
    require the contract-v1 device ID space or REV_01.
    """
    query = _qmp_query_pci(endpoint)
    pci_devices = _iter_qmp_query_pci_devices(query)
    virtio_devices = [d for d in pci_devices if d.vendor_id == 0x1AF4]

    if not virtio_devices:
        dump = _format_pci_id_dump(pci_devices)
        raise RuntimeError(
            "QMP query-pci did not report any virtio PCI devices (expected vendor_id=0x1AF4).\n"
            "query-pci devices:\n"
            f"{dump}"
        )

    # Transitional mode is intentionally permissive: the harness may attach legacy/transitional
    # virtio devices with older device IDs and revision values.
    if virtio_transitional:
        summary = _summarize_pci_ids(virtio_devices)
        print(
            "AERO_VIRTIO_WIN7_HOST|QEMU_PCI_PREFLIGHT|PASS|mode=transitional|vendor=1af4|devices="
            + _sanitize_marker_value(summary)
        )
        return

    # Default (contract-v1) mode: enforce the expected modern-only virtio-pci IDs and REV_01.
    expected_counts: dict[int, int] = {
        0x1041: 1,  # virtio-net-pci modern-only
        0x1042: 1,  # virtio-blk-pci modern-only
        # virtio-keyboard-pci + virtio-mouse-pci (plus optional virtio-tablet-pci) are separate PCI
        # functions but share the virtio-input PCI ID.
        0x1052: 3 if with_virtio_tablet else 2,
    }
    if with_virtio_snd:
        expected_counts[0x1059] = 1

    missing: list[str] = []
    for dev_id, want_count in sorted(expected_counts.items()):
        have = sum(1 for d in virtio_devices if d.device_id == dev_id)
        if have < want_count:
            missing.append(f"DEV_{dev_id:04X} (need>={want_count}, got={have})")

    bad_rev: list[_PciId] = []
    for d in virtio_devices:
        if d.device_id not in expected_counts:
            continue
        if d.revision != 0x01:
            bad_rev.append(d)

    if missing or bad_rev:
        summary = _summarize_pci_ids(virtio_devices)
        dump = _format_pci_id_dump(virtio_devices)
        lines: list[str] = [
            "QEMU PCI preflight failed (expected Aero contract v1 virtio PCI IDs).",
            "Expected (vendor/device/rev): VEN_1AF4 with "
            + "/".join(f"DEV_{k:04X}" for k in sorted(expected_counts.keys()))
            + " and REV_01.",
        ]
        if missing:
            lines.append("Missing expected device IDs: " + ", ".join(missing))
        if bad_rev:
            bad_str = ", ".join(
                f"{d.vendor_id:04X}:{d.device_id:04X}@{('??' if d.revision is None else f'{d.revision:02X}')}"
                for d in sorted(bad_rev, key=lambda x: (x.vendor_id, x.device_id, x.revision or -1))
            )
            lines.append("Unexpected revision IDs (expected REV_01): " + bad_str)
        lines.append("Detected virtio devices (from query-pci):")
        lines.append(dump)
        lines.append("Compact summary: " + summary)
        lines.append(
            "Hint: in contract-v1 mode the harness expects modern-only virtio-pci devices with "
            "'disable-legacy=on,x-pci-revision=0x01'."
        )
        raise RuntimeError("\n".join(lines))

    summary = _summarize_pci_ids([d for d in virtio_devices if d.device_id in expected_counts])
    print(
        "AERO_VIRTIO_WIN7_HOST|QEMU_PCI_PREFLIGHT|PASS|mode=contract-v1|vendor=1af4|devices="
        + _sanitize_marker_value(summary)
    )


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
_VIRTIO_INPUT_QMP_TABLET_ID = "aero_virtio_tablet0"


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


def _qmp_send_command_raw(sock: socket.socket, cmd: dict[str, object]) -> dict[str, object]:
    """
    Send a QMP command and return the raw response.

    Unlike `_qmp_send_command`, this helper does not raise when QEMU returns an error.
    """
    sock.sendall(json.dumps(cmd, separators=(",", ":")).encode("utf-8") + b"\n")
    return _qmp_read_response(sock, timeout_seconds=2.0)


def _qmp_send_command(sock: socket.socket, cmd: dict[str, object]) -> dict[str, object]:
    resp = _qmp_send_command_raw(sock, cmd)
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


_VIRTIO_PCI_VENDOR_ID = 0x1AF4
# Transitional and non-transitional device IDs (virtio spec: 0x1000 range for transitional, 0x1040 range for modern).
_VIRTIO_NET_PCI_DEVICE_IDS = {0x1041, 0x1000}
_VIRTIO_BLK_PCI_DEVICE_IDS = {0x1042, 0x1001}
_VIRTIO_SND_PCI_DEVICE_IDS = {0x1059}


def _qmp_maybe_int(v: object) -> Optional[int]:
    if isinstance(v, int):
        return v
    if isinstance(v, str):
        s = v.strip()
        if not s:
            return None
        try:
            return int(s, 0)
        except ValueError:
            # `int(..., 0)` rejects bare hex without a 0x prefix (e.g. "1af4").
            try:
                return int(s, 16)
            except ValueError:
                return None
    return None


def _qmp_maybe_bool(v: object) -> Optional[bool]:
    if isinstance(v, bool):
        return v
    if isinstance(v, int):
        return bool(v)
    if isinstance(v, str):
        s = v.strip().lower()
        if s in ("1", "true", "yes", "on", "enabled"):
            return True
        if s in ("0", "false", "no", "off", "disabled"):
            return False
    return None


def _qmp_device_vendor_device_id(dev: dict[str, object]) -> tuple[Optional[int], Optional[int]]:
    vendor = _qmp_maybe_int(dev.get("vendor_id"))
    device = _qmp_maybe_int(dev.get("device_id"))
    if vendor is not None and device is not None:
        return vendor, device

    # Some QEMU builds may nest the IDs under an `id` object.
    id_obj = dev.get("id")
    if isinstance(id_obj, dict):
        if vendor is None:
            vendor = _qmp_maybe_int(id_obj.get("vendor_id") or id_obj.get("vendor"))
        if device is None:
            device = _qmp_maybe_int(id_obj.get("device_id") or id_obj.get("device"))
    return vendor, device


def _qmp_parse_msix_enabled_from_query_pci_device(dev: dict[str, object]) -> Optional[bool]:
    caps = dev.get("capabilities")
    if not isinstance(caps, list):
        return None

    for cap in caps:
        if not isinstance(cap, dict):
            continue

        cap_id_obj = cap.get("id") or cap.get("capability") or cap.get("name")
        cap_id = cap_id_obj.lower() if isinstance(cap_id_obj, str) else ""
        cap_id_norm = cap_id.replace("-", "").replace("_", "")

        msix_obj: Optional[object] = None
        if "msix" in cap:
            msix_obj = cap.get("msix")
        elif "msi-x" in cap:
            msix_obj = cap.get("msi-x")
        elif cap_id_norm == "msix" or "msix" in cap_id_norm or "msix" in cap_id:
            msix_obj = cap

        if msix_obj is None:
            continue

        if isinstance(msix_obj, dict):
            enabled = _qmp_maybe_bool(msix_obj.get("enabled"))
            if enabled is not None:
                return enabled

        # Some QEMU versions may put `enabled` directly on the capability object.
        enabled = _qmp_maybe_bool(cap.get("enabled"))
        if enabled is not None:
            return enabled

        # Found MSI-X, but no readable enabled bit.
        return None

    return None


def _parse_qmp_query_pci_msix_info(query_pci_return: object) -> list[_PciMsixInfo]:
    """
    Parse QMP `query-pci` output and extract MSI-X enabled state when available.
    """
    infos: list[_PciMsixInfo] = []
    if not isinstance(query_pci_return, list):
        return infos

    for bus_obj in query_pci_return:
        if not isinstance(bus_obj, dict):
            continue
        bus_num = _qmp_maybe_int(bus_obj.get("bus"))
        devs = bus_obj.get("devices")
        if not isinstance(devs, list):
            continue
        for dev_obj in devs:
            if not isinstance(dev_obj, dict):
                continue
            vendor_id, device_id = _qmp_device_vendor_device_id(dev_obj)
            if vendor_id is None or device_id is None:
                continue

            dev_bus = _qmp_maybe_int(dev_obj.get("bus"))
            slot = _qmp_maybe_int(dev_obj.get("slot"))
            func = _qmp_maybe_int(dev_obj.get("function"))
            if dev_bus is None:
                dev_bus = bus_num
            msix_enabled = _qmp_parse_msix_enabled_from_query_pci_device(dev_obj)
            infos.append(
                _PciMsixInfo(
                    vendor_id=vendor_id,
                    device_id=device_id,
                    bus=dev_bus,
                    slot=slot,
                    function=func,
                    msix_enabled=msix_enabled,
                    source="query-pci",
                )
            )

    return infos


_HMP_PCI_HEADER_RE = re.compile(r"^Bus\s+(\d+),\s*device\s+(\d+),\s*function\s+(\d+):", re.IGNORECASE)
_HMP_PCI_VENDOR_DEVICE_RE = re.compile(
    r"\bVendor\s+ID:\s*([0-9a-fA-Fx]+)\s+Device\s+ID:\s*([0-9a-fA-Fx]+)\b", re.IGNORECASE
)
_HMP_PCI_DEVICE_PAIR_RE = re.compile(r"\b([0-9a-fA-F]{4}):([0-9a-fA-F]{4})\b")


def _hmp_parse_msix_enabled_from_line(line: str) -> Optional[bool]:
    low = line.lower()
    if "msi-x" not in low and "msix" not in low:
        return None
    if "disabled" in low or "enable-" in low or "off" in low:
        return False
    if "enabled" in low or "enable+" in low or re.search(r"\benable(d)?\b", low):
        return True
    return None


def _parse_hmp_info_pci_msix_info(info_pci_text: str) -> list[_PciMsixInfo]:
    """
    Parse HMP `info pci` output and extract MSI-X enabled state best-effort.
    """
    infos: list[_PciMsixInfo] = []

    bus: Optional[int] = None
    slot: Optional[int] = None
    function: Optional[int] = None
    vendor_id: Optional[int] = None
    device_id: Optional[int] = None
    msix_enabled: Optional[bool] = None

    def flush() -> None:
        nonlocal vendor_id, device_id, bus, slot, function, msix_enabled
        if vendor_id is None or device_id is None:
            return
        infos.append(
            _PciMsixInfo(
                vendor_id=vendor_id,
                device_id=device_id,
                bus=bus,
                slot=slot,
                function=function,
                msix_enabled=msix_enabled,
                source="info pci",
            )
        )

    for raw_line in info_pci_text.splitlines():
        line = raw_line.rstrip("\r\n")
        m = _HMP_PCI_HEADER_RE.match(line)
        if m:
            flush()
            bus = _qmp_maybe_int(m.group(1))
            slot = _qmp_maybe_int(m.group(2))
            function = _qmp_maybe_int(m.group(3))
            vendor_id = None
            device_id = None
            msix_enabled = None
            continue

        m = _HMP_PCI_VENDOR_DEVICE_RE.search(line)
        if m:
            vendor_id = _qmp_maybe_int(m.group(1))
            device_id = _qmp_maybe_int(m.group(2))
            continue

        # Fallback: some QEMU builds only show the numeric IDs as a pair somewhere in the section
        # (e.g. "Device 1af4:1041").
        if vendor_id is None or device_id is None:
            m2 = _HMP_PCI_DEVICE_PAIR_RE.search(line)
            if m2:
                vendor_id = _qmp_maybe_int("0x" + m2.group(1))
                device_id = _qmp_maybe_int("0x" + m2.group(2))

        msix = _hmp_parse_msix_enabled_from_line(line)
        if msix is not None:
            msix_enabled = msix

    flush()
    return infos


def _qmp_collect_pci_msix_info(
    endpoint: _QmpEndpoint,
) -> tuple[list[_PciMsixInfo], list[_PciMsixInfo], bool, bool]:
    """
    Collect PCI MSI-X state from QEMU via QMP.

    Returns a tuple of:
      - parsed `query-pci` info (may be empty if unsupported)
      - parsed `human-monitor-command: info pci` info (may be empty if unsupported)
      - whether `query-pci` was supported by QMP
      - whether `human-monitor-command` was supported by QMP
    """
    with _qmp_connect(endpoint, timeout_seconds=5.0) as s:
        query_infos: list[_PciMsixInfo] = []
        resp = _qmp_send_command_raw(s, {"execute": "query-pci"})
        query_supported = "return" in resp
        if query_supported:
            query_infos = _parse_qmp_query_pci_msix_info(resp.get("return"))

        info_infos: list[_PciMsixInfo] = []
        resp = _qmp_send_command_raw(
            s,
            {"execute": "human-monitor-command", "arguments": {"command-line": "info pci"}},
        )
        info_supported = "return" in resp
        if info_supported:
            txt_obj = resp.get("return")
            txt = txt_obj if isinstance(txt_obj, str) else str(txt_obj)
            info_infos = _parse_hmp_info_pci_msix_info(txt)

        return query_infos, info_infos, query_supported, info_supported


def _require_virtio_msix_check_failure_message(
    endpoint: _QmpEndpoint,
    *,
    require_virtio_net_msix: bool,
    require_virtio_blk_msix: bool,
    require_virtio_snd_msix: bool,
) -> Optional[str]:
    """
    Verify required virtio PCI functions have MSI-X enabled.

    Returns a deterministic `FAIL: ...` message when the check fails, else None.
    """
    if not (require_virtio_net_msix or require_virtio_blk_msix or require_virtio_snd_msix):
        return None

    try:
        query_infos, info_infos, query_supported, info_supported = _qmp_collect_pci_msix_info(endpoint)
    except Exception as e:
        return f"FAIL: QMP_MSIX_CHECK_FAILED: failed to query PCI state via QMP: {e}"

    if not query_supported and not info_supported:
        return (
            "FAIL: QMP_MSIX_CHECK_UNSUPPORTED: QEMU QMP does not support query-pci or human-monitor-command "
            "(required for MSI-X verification)"
        )

    requirements: list[tuple[str, str, set[int]]] = []
    if require_virtio_net_msix:
        requirements.append(("virtio-net", "VIRTIO_NET_MSIX_NOT_ENABLED", _VIRTIO_NET_PCI_DEVICE_IDS))
    if require_virtio_blk_msix:
        requirements.append(("virtio-blk", "VIRTIO_BLK_MSIX_NOT_ENABLED", _VIRTIO_BLK_PCI_DEVICE_IDS))
    if require_virtio_snd_msix:
        requirements.append(("virtio-snd", "VIRTIO_SND_MSIX_NOT_ENABLED", _VIRTIO_SND_PCI_DEVICE_IDS))

    for device_name, token, device_ids in requirements:
        q = [i for i in query_infos if i.vendor_id == _VIRTIO_PCI_VENDOR_ID and i.device_id in device_ids]
        h = [i for i in info_infos if i.vendor_id == _VIRTIO_PCI_VENDOR_ID and i.device_id in device_ids]
        any_matches = q + h
        if not any_matches:
            ids_str = ",".join(f"{_VIRTIO_PCI_VENDOR_ID:04x}:{d:04x}" for d in sorted(device_ids))
            return f"FAIL: {token}: did not find {device_name} PCI function(s) ({ids_str}) in QEMU PCI introspection output"

        # Prefer structured query-pci output when it provides an explicit enabled bit.
        matches: Optional[list[_PciMsixInfo]] = None
        if q and all(i.msix_enabled is not None for i in q):
            matches = q
        elif h and all(i.msix_enabled is not None for i in h):
            matches = h

        if matches is None:
            # We found matching devices, but could not determine MSI-X state from either output.
            ids_str = ",".join(f"{_VIRTIO_PCI_VENDOR_ID:04x}:{d:04x}" for d in sorted(device_ids))
            bdfs = ",".join(i.bdf() for i in any_matches[:4])
            extra = ""
            if len(any_matches) > 4:
                extra = f",...(+{len(any_matches)-4})"
            return (
                "FAIL: QMP_MSIX_CHECK_UNSUPPORTED: could not determine MSI-X enabled state for "
                f"{device_name} PCI function(s) ({ids_str}) (bdf={bdfs}{extra})"
            )

        not_enabled = [i for i in matches if not i.msix_enabled]
        if not_enabled:
            i = not_enabled[0]
            return (
                f"FAIL: {token}: {device_name} PCI function {i.pci_id()} at {i.bdf()} "
                f"reported MSI-X disabled (source={i.source})"
            )

    return None


def _qmp_key_event(qcode: str, *, down: bool) -> dict[str, object]:
    return {
        "type": "key",
        "data": {"down": down, "key": {"type": "qcode", "data": qcode}},
    }


def _qmp_btn_event(button: str, *, down: bool) -> dict[str, object]:
    return {"type": "btn", "data": {"down": down, "button": button}}


def _qmp_rel_event(axis: str, value: int) -> dict[str, object]:
    return {"type": "rel", "data": {"axis": axis, "value": value}}


def _qmp_abs_event(axis: str, value: int) -> dict[str, object]:
    return {"type": "abs", "data": {"axis": axis, "value": value}}


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


def _qmp_blockdev_resize_command(*, node_name: str, size: int) -> dict[str, object]:
    """
    Build a QMP `blockdev-resize` command (node-name based).

    This helper exists primarily so host-harness unit tests can sanity-check command structure.
    """
    return {"execute": "blockdev-resize", "arguments": {"node-name": node_name, "size": int(size)}}


def _qmp_block_resize_command(*, device: str, size: int) -> dict[str, object]:
    """
    Build a legacy QMP `block_resize` command (drive/BlockBackend id based).

    This helper exists primarily so host-harness unit tests can sanity-check command structure.
    """
    return {"execute": "block_resize", "arguments": {"device": device, "size": int(size)}}


def _qmp_deterministic_keyboard_events(*, qcode: str) -> list[dict[str, object]]:
    # Press + release a single key via qcode (stable across host layouts).
    return [
        _qmp_key_event(qcode, down=True),
        _qmp_key_event(qcode, down=False),
    ]


_QMP_TEST_MOUSE_WHEEL_DELTA = 1
_QMP_TEST_MOUSE_HWHEEL_DELTA = -2
_QMP_TEST_MOUSE_VSCROLL_AXIS = "wheel"  # QMP InputAxis enum (vertical scroll)
_QMP_TEST_MOUSE_VSCROLL_AXIS_FALLBACK = "vscroll"  # Alternate name (best-effort)
_QMP_TEST_MOUSE_HSCROLL_AXIS = "hscroll"  # QMP InputAxis enum (horizontal scroll)
_QMP_TEST_MOUSE_HWHEEL_AXIS_FALLBACK = "hwheel"  # Older/alternate name (best-effort)


def _qmp_deterministic_keyboard_modifier_events() -> list[dict[str, object]]:
    """
    A deterministic keyboard sequence that exercises:
    - modifiers (Shift/Ctrl/Alt)
    - a function key (F1)

    Guest-side validation lives under:
      AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-modifiers|...
    """
    return [
        # Shift + b.
        _qmp_key_event("shift", down=True),
        _qmp_key_event("b", down=True),
        _qmp_key_event("b", down=False),
        _qmp_key_event("shift", down=False),
        # Ctrl.
        _qmp_key_event("ctrl", down=True),
        _qmp_key_event("ctrl", down=False),
        # Alt.
        _qmp_key_event("alt", down=True),
        _qmp_key_event("alt", down=False),
        # Function key.
        _qmp_key_event("f1", down=True),
        _qmp_key_event("f1", down=False),
    ]


def _qmp_deterministic_mouse_events(*, with_wheel: bool = False) -> list[dict[str, object]]:
    # Small relative motion + (optional) scroll + a left click.
    events: list[dict[str, object]] = [
        _qmp_rel_event("x", 10),
        _qmp_rel_event("y", 5),
    ]
    if with_wheel:
        # QMP InputAxis enum is expected to include `wheel`/`vscroll` (vertical scroll) and
        # `hscroll`/`hwheel` (horizontal scroll). Some QEMU builds may use alternate names;
        # the injection helper will retry with fallbacks when needed.
        #
        # The guest selftest validates these via raw HID reports (wheel + AC Pan).
        events += [
            _qmp_rel_event(_QMP_TEST_MOUSE_VSCROLL_AXIS, _QMP_TEST_MOUSE_WHEEL_DELTA),
            _qmp_rel_event(_QMP_TEST_MOUSE_HSCROLL_AXIS, _QMP_TEST_MOUSE_HWHEEL_DELTA),
        ]
    events += [
        _qmp_btn_event("left", down=True),
        _qmp_btn_event("left", down=False),
    ]
    return events


def _qmp_deterministic_tablet_events(*, x: int = 10000, y: int = 20000) -> list[dict[str, object]]:
    """
    Deterministic absolute pointer (tablet) motion + click sequence.

    The sequence includes a reset-to-origin move before the target coordinate so repeated injections
    still generate movement reports even if the previous attempt already moved to (x, y).
    """
    return [
        # Reset move (0,0) to avoid "no-op" repeats.
        _qmp_abs_event("x", 0),
        _qmp_abs_event("y", 0),
        # Target move.
        _qmp_abs_event("x", int(x)),
        _qmp_abs_event("y", int(y)),
        # Left click.
        _qmp_btn_event("left", down=True),
        _qmp_btn_event("left", down=False),
    ]


def _qmp_deterministic_mouse_extra_button_events() -> list[dict[str, object]]:
    """A deterministic mouse button sequence that exercises side/extra buttons."""
    return [
        _qmp_btn_event("side", down=True),
        _qmp_btn_event("side", down=False),
        _qmp_btn_event("extra", down=True),
        _qmp_btn_event("extra", down=False),
    ]


@dataclass(frozen=True)
class _VirtioInputQmpInjectInfo:
    keyboard_device: Optional[str]
    mouse_device: Optional[str]


def _try_qmp_input_inject_virtio_input_events(
    endpoint: _QmpEndpoint, *, with_wheel: bool = False, extended: bool = False
) -> _VirtioInputQmpInjectInfo:
    """
    Inject a small, deterministic keyboard + mouse sequence via QMP.

    Guest-side verification lives in `aero-virtio-selftest.exe` under the marker:
      AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|...
    and optionally:
      AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|...

    When `extended` is true, also inject additional events to exercise:
      - virtio-input-events-modifiers (Shift/Ctrl/Alt/F1)
      - virtio-input-events-buttons (side/extra)
      - virtio-input-events-wheel (wheel)
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

        want_wheel = bool(with_wheel or extended)
        kbd_events = _qmp_deterministic_keyboard_events(qcode="a")
        mouse_events = _qmp_deterministic_mouse_events(with_wheel=want_wheel)

        # Split mouse events into one batch of rel axis events, followed by individual button events.
        rel_end = 0
        while rel_end < len(mouse_events) and mouse_events[rel_end].get("type") == "rel":
            rel_end += 1
        mouse_rel_events = mouse_events[:rel_end]
        mouse_btn_events = mouse_events[rel_end:]

        # Keyboard: 'a' press + release.
        kbd_device = send(s, [kbd_events[0]], device=kbd_device)
        time.sleep(0.05)
        kbd_device = send(s, [kbd_events[1]], device=kbd_device)

        # Mouse: small movement then left click.
        time.sleep(0.05)
        try:
            mouse_device = send(s, mouse_rel_events, device=mouse_device)
        except Exception as e:
            if not want_wheel:
                raise

            def rewrite(events: list[dict[str, object]], axis_map: dict[str, str]) -> list[dict[str, object]]:
                out: list[dict[str, object]] = []
                for ev in events:
                    if ev.get("type") == "rel" and isinstance(ev.get("data"), dict):
                        axis = ev["data"].get("axis")
                        if axis in axis_map:
                            ev2 = {"type": "rel", "data": dict(ev["data"])}
                            ev2["data"]["axis"] = axis_map[axis]
                            out.append(ev2)
                            continue
                    out.append(ev)
                return out

            # Some QEMU versions use alternate axis names for scroll wheels. Try a best-effort matrix
            # of axis pairs before failing.
            errors: dict[str, str] = {"wheel+hscroll": str(e)}
            attempts: list[tuple[str, dict[str, str]]] = [
                ("wheel+hwheel", {_QMP_TEST_MOUSE_HSCROLL_AXIS: _QMP_TEST_MOUSE_HWHEEL_AXIS_FALLBACK}),
                ("vscroll+hscroll", {_QMP_TEST_MOUSE_VSCROLL_AXIS: _QMP_TEST_MOUSE_VSCROLL_AXIS_FALLBACK}),
                (
                    "vscroll+hwheel",
                    {
                        _QMP_TEST_MOUSE_VSCROLL_AXIS: _QMP_TEST_MOUSE_VSCROLL_AXIS_FALLBACK,
                        _QMP_TEST_MOUSE_HSCROLL_AXIS: _QMP_TEST_MOUSE_HWHEEL_AXIS_FALLBACK,
                    },
                ),
            ]

            for label, axis_map in attempts:
                evs = rewrite(mouse_rel_events, axis_map)
                try:
                    mouse_device = send(s, evs, device=mouse_device)
                    break
                except Exception as e2:
                    errors[label] = str(e2)
            else:
                raise RuntimeError(
                    "QMP input-send-event failed while injecting scroll for "
                    "--with-input-wheel/--with-virtio-input-wheel or --with-input-events-extended/--with-input-events-extra. "
                    "Upgrade QEMU or omit those flags. "
                    f"errors={errors}"
                ) from e
        time.sleep(0.05)
        mouse_device = send(s, [mouse_btn_events[0]], device=mouse_device)
        time.sleep(0.05)
        mouse_device = send(s, [mouse_btn_events[1]], device=mouse_device)

        if extended:
            # Keyboard: modifiers + function key.
            time.sleep(0.05)
            for evt in _qmp_deterministic_keyboard_modifier_events():
                kbd_device = send(s, [evt], device=kbd_device)
                time.sleep(0.05)

            # Mouse: side/extra + wheel.
            time.sleep(0.05)
            for evt in _qmp_deterministic_mouse_extra_button_events():
                mouse_device = send(s, [evt], device=mouse_device)
                time.sleep(0.05)

        return _VirtioInputQmpInjectInfo(keyboard_device=kbd_device, mouse_device=mouse_device)


@dataclass(frozen=True)
class _VirtioInputMediaKeysQmpInjectInfo:
    keyboard_device: Optional[str]
    qcode: str


def _try_qmp_input_inject_virtio_input_media_keys(
    endpoint: _QmpEndpoint, *, qcode: str = "volumeup"
) -> _VirtioInputMediaKeysQmpInjectInfo:
    """
    Inject a small, deterministic Consumer Control (media key) sequence via QMP.

    Guest-side verification lives in `aero-virtio-selftest.exe` under the marker:
      AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|...
    """

    def send(sock: socket.socket, events: list[dict[str, object]], *, device: Optional[str]) -> Optional[str]:
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
        ev = _qmp_deterministic_keyboard_events(qcode=qcode)

        # Media key: press + release.
        kbd_device = send(s, [ev[0]], device=kbd_device)
        time.sleep(0.05)
        kbd_device = send(s, [ev[1]], device=kbd_device)

        return _VirtioInputMediaKeysQmpInjectInfo(keyboard_device=kbd_device, qcode=qcode)


@dataclass(frozen=True)
class _VirtioInputTabletQmpInjectInfo:
    tablet_device: Optional[str]


def _try_qmp_input_inject_virtio_input_tablet_events(endpoint: _QmpEndpoint) -> _VirtioInputTabletQmpInjectInfo:
    """
    Inject a deterministic absolute-pointer (tablet) move + click sequence via QMP.

    Guest-side verification lives in `aero-virtio-selftest.exe` under the marker:
      AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|...
    """

    def send(sock: socket.socket, events: list[dict[str, object]], *, device: Optional[str]) -> Optional[str]:
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
        tablet_device: Optional[str] = _VIRTIO_INPUT_QMP_TABLET_ID
        ev = _qmp_deterministic_tablet_events()

        # Reset move (0,0).
        tablet_device = send(s, ev[0:2], device=tablet_device)
        time.sleep(0.05)
        # Target move.
        tablet_device = send(s, ev[2:4], device=tablet_device)
        time.sleep(0.05)
        # Click down.
        tablet_device = send(s, [ev[4]], device=tablet_device)
        time.sleep(0.05)
        # Click up.
        tablet_device = send(s, [ev[5]], device=tablet_device)

        return _VirtioInputTabletQmpInjectInfo(tablet_device=tablet_device)

def _try_qmp_virtio_blk_resize(endpoint: _QmpEndpoint, *, drive_id: str, new_bytes: int) -> str:
    """
    Resize the virtio-blk backing device via QMP.

    Compatibility cascade:
    - Try `blockdev-resize` (node-name based).
    - Fall back to legacy `block_resize` (device/BlockBackend id based).
    """
    with _qmp_connect(endpoint, timeout_seconds=5.0) as s:
        try:
            _qmp_send_command(
                s, _qmp_blockdev_resize_command(node_name=drive_id, size=int(new_bytes))
            )
            return "blockdev-resize"
        except Exception as e_blockdev:
            try:
                _qmp_send_command(
                    s, _qmp_block_resize_command(device=drive_id, size=int(new_bytes))
                )
                print(
                    f"WARNING: QMP blockdev-resize failed; falling back to block_resize: {e_blockdev}",
                    file=sys.stderr,
                )
                return "block_resize"
            except Exception as e_legacy:
                raise RuntimeError(
                    f"QMP resize failed: blockdev-resize error={e_blockdev}; block_resize error={e_legacy}"
                ) from e_legacy


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


def _qemu_virtio_tablet_pci_device_arg(*, disable_legacy: bool, pci_revision: Optional[str]) -> str:
    parts = ["virtio-tablet-pci", f"id={_VIRTIO_INPUT_QMP_TABLET_ID}"]
    if disable_legacy:
        parts.append("disable-legacy=on")
    if pci_revision is not None:
        parts.append(f"x-pci-revision={pci_revision}")
    return ",".join(parts)


def _qemu_device_arg_add_vectors(device_arg: str, vectors: Optional[int]) -> str:
    """
    Optionally append `,vectors=<N>` to a QEMU `-device` argument string.

    This is used by the host harness to request more MSI-X vectors from virtio-pci
    devices (when the running QEMU build exposes the `vectors` property).
    """

    if vectors is None:
        return device_arg
    if int(vectors) <= 0:
        raise ValueError(f"vectors must be a positive integer (got {vectors})")

    # Avoid generating malformed args if callers accidentally include a trailing comma.
    arg = device_arg.rstrip()
    while arg.endswith(","):
        arg = arg[:-1]

    # If the device arg already specifies vectors, do not add a duplicate key.
    if ",vectors=" in ("," + arg):
        return arg

    return f"{arg},vectors={int(vectors)}"


def _qemu_device_arg_maybe_add_vectors(
    qemu_system: str,
    device_name: str,
    device_arg: str,
    vectors: Optional[int],
    *,
    flag_name: str,
) -> str:
    """
    Append `,vectors=<N>` to a `-device` arg only when the QEMU device supports it.

    If unsupported, emit a warning and return the original arg unchanged. This keeps
    the harness compatible with older QEMU builds that do not expose a `vectors`
    property for a given device.
    """

    if vectors is None:
        return device_arg
    if not _qemu_device_supports_property(qemu_system, device_name, "vectors"):
        print(
            f"WARNING: QEMU device '{device_name}' does not advertise a 'vectors' property; ignoring {flag_name}={vectors}",
            file=sys.stderr,
        )
        return device_arg
    try:
        return _qemu_device_arg_add_vectors(device_arg, vectors)
    except Exception as e:
        print(
            f"WARNING: failed to apply {flag_name}={vectors} to QEMU device '{device_name}': {e}",
            file=sys.stderr,
        )
        return device_arg


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


def _virtio_snd_buffer_limits_skip_failure_message(tail: bytes) -> str:
    # Buffer limits stress test marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|PASS/FAIL/SKIP|...
    prefix = b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|SKIP|"

    if b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|SKIP|flag_not_set" in tail:
        return (
            "FAIL: VIRTIO_SND_BUFFER_LIMITS_SKIPPED: virtio-snd-buffer-limits test was skipped (flag_not_set) but "
            "--with-snd-buffer-limits was enabled (provision the guest with --test-snd-buffer-limits)"
        )

    # Fallback: extract the skip reason token from the marker itself.
    idx = tail.rfind(prefix)
    if idx != -1:
        end = tail.find(b"\n", idx)
        if end == -1:
            end = len(tail)
        reason = tail[idx + len(prefix) : end].strip()
        if reason:
            reason_str = reason.decode("utf-8", errors="replace").strip()
            return (
                f"FAIL: VIRTIO_SND_BUFFER_LIMITS_SKIPPED: virtio-snd-buffer-limits test was skipped ({reason_str}) "
                "but --with-snd-buffer-limits was enabled"
            )
    return "FAIL: VIRTIO_SND_BUFFER_LIMITS_SKIPPED: virtio-snd-buffer-limits test was skipped but --with-snd-buffer-limits was enabled"


def _virtio_snd_buffer_limits_required_failure_message(tail: bytes) -> Optional[str]:
    """
    Enforce that virtio-snd-buffer-limits ran and PASSed.

    Returns:
        A "FAIL: ..." message on failure, or None when the marker requirements are satisfied.
    """
    if b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|PASS" in tail:
        return None
    if b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|FAIL" in tail:
        return (
            "FAIL: VIRTIO_SND_BUFFER_LIMITS_FAILED: virtio-snd-buffer-limits test reported FAIL while "
            "--with-snd-buffer-limits was enabled"
        )
    if b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|SKIP" in tail:
        return _virtio_snd_buffer_limits_skip_failure_message(tail)
    return (
        "FAIL: MISSING_VIRTIO_SND_BUFFER_LIMITS: did not observe virtio-snd-buffer-limits PASS marker while "
        "--with-snd-buffer-limits was enabled (provision the guest with --test-snd-buffer-limits)"
    )


@lru_cache(maxsize=None)
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


@lru_cache(maxsize=None)
def _qemu_device_property_names(qemu_system: str, device_name: str) -> frozenset[str]:
    """
    Return the set of property names exposed by `-device <name>,help`.

    QEMU prints per-property help lines like:
      <prop>=<type> ...

    We parse those lines and cache the result so callers can probe feature support
    (e.g. `vectors=`) once per device per harness invocation.
    """

    help_text = _qemu_device_help_text(qemu_system, device_name)
    props: set[str] = set()
    for line in help_text.splitlines():
        m = re.match(r"^\s*([A-Za-z0-9][A-Za-z0-9_.-]*)\s*=", line)
        if m:
            props.add(m.group(1))
    return frozenset(props)


def _qemu_device_supports_property(qemu_system: str, device_name: str, prop: str) -> bool:
    try:
        return prop in _qemu_device_property_names(qemu_system, device_name)
    except RuntimeError:
        return False


@lru_cache(maxsize=None)
def _qemu_device_list_help_text(qemu_system: str) -> str:
    """
    Return the output of `qemu-system-* -device help`.

    This is primarily used for cheap feature probing (e.g. device name aliases).
    The output is cached so we only spawn QEMU once per harness invocation.
    """
    try:
        proc = subprocess.run(
            [qemu_system, "-device", "help"],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            check=False,
        )
    except FileNotFoundError as e:
        raise RuntimeError(f"qemu-system binary not found: {qemu_system}") from e
    except OSError as e:
        raise RuntimeError(f"failed to run '{qemu_system} -device help': {e}") from e
    return proc.stdout or ""


@lru_cache(maxsize=None)
def _qemu_has_device(qemu_system: str, device_name: str) -> bool:
    try:
        _qemu_device_help_text(qemu_system, device_name)
        return True
    except RuntimeError:
        return False


def _assert_qemu_supports_aero_w7_virtio_contract_v1(
    qemu_system: str, *, with_virtio_snd: bool = False, with_virtio_tablet: bool = False
) -> None:
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

    if with_virtio_tablet:
        required.append(("virtio-tablet-pci", True))

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
    help_text = _qemu_device_list_help_text(qemu_system)
    if "virtio-sound-pci" in help_text:
        return "virtio-sound-pci"
    if "virtio-snd-pci" in help_text:
        return "virtio-snd-pci"

    raise RuntimeError(
        "QEMU does not advertise a virtio-snd PCI device (expected 'virtio-sound-pci' or 'virtio-snd-pci'). "
        "Upgrade QEMU or omit --with-virtio-snd/--enable-virtio-snd and pass custom QEMU args."
    )


def _build_arg_parser() -> argparse.ArgumentParser:
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
    parser.add_argument(
        "--udp-port",
        type=int,
        default=18081,
        help="Host UDP echo server port (guest reaches it via slirp at 10.0.2.2:<port>)",
    )
    parser.add_argument(
        "--disable-udp",
        action="store_true",
        help=(
            "Disable the host UDP echo server and do not require the guest virtio-net-udp marker. "
            "Useful when running against older guest selftest binaries that do not yet implement the UDP test."
        ),
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
        "--with-virtio-tablet",
        dest="with_virtio_tablet",
        action="store_true",
        help="Attach a virtio-tablet-pci device (in addition to virtio keyboard/mouse).",
    )
    parser.add_argument(
        "--qemu-preflight-pci",
        "--qmp-preflight-pci",
        dest="qemu_preflight_pci",
        action="store_true",
        help=(
            "After starting QEMU and completing the QMP handshake, run a `query-pci` preflight to validate "
            "QEMU-emitted virtio PCI Vendor/Device/Revision IDs. In the default (contract-v1) mode this enforces "
            "VEN_1AF4 + DEV_1041/DEV_1042/DEV_1052[/DEV_1059] with REV_01. In transitional mode this is permissive "
            "and only asserts that at least one VEN_1AF4 device exists."
        ),
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
        "--with-input-media-keys",
        "--with-virtio-input-media-keys",
        "--require-virtio-input-media-keys",
        "--enable-virtio-input-media-keys",
        dest="with_input_media_keys",
        action="store_true",
        help=(
            "Inject deterministic Consumer Control (media key) events via QMP (input-send-event) and require the guest "
            "virtio-input-media-keys selftest marker to PASS. Also emits a host marker: "
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_MEDIA_KEYS_INJECT|PASS/FAIL|attempt=<n>|kbd_mode=device/broadcast "
            "(may appear multiple times due to retries). "
            "This requires a guest image provisioned with --test-input-media-keys (or env var)."
        ),
    )
    parser.add_argument(
        "--with-input-wheel",
        "--with-virtio-input-wheel",
        "--enable-virtio-input-wheel",
        dest="with_input_wheel",
        action="store_true",
        help=(
            "When injecting virtio-input events, also inject vertical + horizontal scroll wheel events "
            "(QMP rel axis: wheel + hscroll) and require the guest virtio-input-wheel marker to PASS. "
            "Implies --with-input-events."
        ),
    )
    parser.add_argument(
        "--with-input-events-extended",
        "--with-input-events-extra",
        dest="with_input_events_extended",
        action="store_true",
        help=(
            "Also inject and require additional virtio-input end-to-end markers:\n"
            "  - virtio-input-events-modifiers (Shift/Ctrl/Alt + F1)\n"
            "  - virtio-input-events-buttons   (side/extra mouse buttons)\n"
            "  - virtio-input-events-wheel     (mouse wheel)\n"
            "This implies --with-input-events, and requires the guest selftest to be configured with "
            "--test-input-events-extended (or the corresponding env vars)."
        ),
    )
    parser.add_argument(
        "--with-input-tablet-events",
        "--with-tablet-events",
        "--with-virtio-input-tablet-events",
        "--require-virtio-input-tablet-events",
        "--enable-virtio-input-tablet-events",
        dest="with_input_tablet_events",
        action="store_true",
        help=(
            "Attach a virtio-tablet-pci device and inject deterministic absolute-pointer (tablet) events via "
            "QMP (input-send-event). Require the guest virtio-input-tablet-events selftest marker to PASS. "
            "Also emits a host marker: "
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_TABLET_EVENTS_INJECT|PASS/FAIL|attempt=<n>|tablet_mode=device/broadcast "
            "(may appear multiple times due to retries). "
            "This requires a guest image provisioned with --test-input-tablet-events/--test-tablet-events "
            "(or env var: AERO_VIRTIO_SELFTEST_TEST_INPUT_TABLET_EVENTS=1 or AERO_VIRTIO_SELFTEST_TEST_TABLET_EVENTS=1)."
        ),
    )
    parser.add_argument(
        "--with-blk-resize",
        "--with-virtio-blk-resize",
        "--require-virtio-blk-resize",
        dest="with_blk_resize",
        action="store_true",
        help=(
            "Run an end-to-end virtio-blk runtime resize test: wait for the guest "
            "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|READY marker, then grow the backing device via QMP "
            "(blockdev-resize/block_resize) and require the guest virtio-blk-resize marker to PASS. "
            "This requires a guest image provisioned with --test-blk-resize (or env var)."
        ),
    )
    parser.add_argument(
        "--blk-resize-delta-mib",
        type=int,
        default=64,
        help="Delta in MiB to grow the virtio-blk backing device when --with-blk-resize is enabled (default: 64)",
    )
    parser.add_argument(
        "--require-virtio-net-msix",
        action="store_true",
        help="After drivers load, require that the virtio-net PCI function has MSI-X enabled (checked via QMP/QEMU introspection).",
    )
    parser.add_argument(
        "--require-virtio-blk-msix",
        action="store_true",
        help=(
            "Require virtio-blk to run with MSI-X enabled. This performs a host-side MSI-X enable check via QMP "
            "and also requires the guest marker: "
            "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-msix|PASS|mode=msix|..."
        ),
    )
    parser.add_argument(
        "--require-virtio-snd-msix",
        action="store_true",
        help=(
            "Require virtio-snd to run with MSI-X enabled. This performs a host-side MSI-X enable check via QMP "
            "and also requires the guest marker: "
            "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|PASS|mode=msix|... "
            "(this option requires --with-virtio-snd)."
        ),
    )
    parser.add_argument(
        "--require-virtio-input-msix",
        dest="require_virtio_input_msix",
        action="store_true",
        help=(
            "Require the guest virtio-input-msix marker to report mode=msix. "
            "This is optional so older guest selftest binaries (which don't emit the marker) can still run."
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
        "--with-snd-buffer-limits",
        "--with-virtio-snd-buffer-limits",
        "--enable-snd-buffer-limits",
        dest="with_snd_buffer_limits",
        action="store_true",
        help=(
            "Require the guest virtio-snd-buffer-limits stress test marker to PASS. "
            "This requires a guest image provisioned with --test-snd-buffer-limits and also requires "
            "--with-virtio-snd/--enable-virtio-snd so a virtio-snd device is attached."
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
    parser.add_argument(
        "--virtio-msix-vectors",
        type=int,
        default=None,
        help=(
            "If set, request a specific MSI-X vector table size from QEMU by appending ',vectors=N' to "
            "virtio-pci devices created by the harness (virtio-net/blk/input/snd). "
            "Best-effort: requires a QEMU build that supports the 'vectors' property. "
            "Typical values: 2, 4, 8. Windows may still allocate fewer messages; drivers fall back. "
            "Disabled by default."
        ),
    )
    parser.add_argument(
        "--virtio-net-vectors",
        "--virtio-net-msix-vectors",
        type=int,
        default=None,
        metavar="N",
        help="Override virtio-net MSI-X vectors via `-device virtio-net-pci,...,vectors=N` when supported.",
    )
    parser.add_argument(
        "--virtio-blk-vectors",
        "--virtio-blk-msix-vectors",
        type=int,
        default=None,
        metavar="N",
        help="Override virtio-blk MSI-X vectors via `-device virtio-blk-pci,...,vectors=N` when supported.",
    )
    parser.add_argument(
        "--virtio-snd-vectors",
        "--virtio-snd-msix-vectors",
        type=int,
        default=None,
        metavar="N",
        help="Override virtio-snd MSI-X vectors via `-device virtio-snd-pci,...,vectors=N` when supported (requires --with-virtio-snd).",
    )
    parser.add_argument(
        "--virtio-input-vectors",
        "--virtio-input-msix-vectors",
        type=int,
        default=None,
        metavar="N",
        help="Override virtio-input MSI-X vectors via `-device virtio-*-pci,...,vectors=N` when supported.",
    )
    parser.add_argument(
        "--require-intx",
        action="store_true",
        help=(
            "Require INTx interrupt mode for the attached virtio devices (virtio-blk/net/input/snd). "
            "Fails if the guest reports MSI/MSI-X via virtio-*-irq markers."
        ),
    )
    parser.add_argument(
        "--require-msi",
        action="store_true",
        help=(
            "Require MSI/MSI-X interrupt mode for the attached virtio devices (virtio-blk/net/input/snd). "
            "Fails if the guest reports INTx via virtio-*-irq markers."
        ),
    )

    return parser


def main() -> int:
    parser = _build_arg_parser()

    # Any remaining args are passed directly to QEMU.
    args, qemu_extra = parser.parse_known_args()
    need_input_wheel = bool(getattr(args, "with_input_wheel", False))
    need_input_events_extended = bool(getattr(args, "with_input_events_extended", False))
    need_input_events = bool(args.with_input_events) or need_input_wheel or need_input_events_extended
    need_input_media_keys = bool(getattr(args, "with_input_media_keys", False))
    need_input_tablet_events = bool(getattr(args, "with_input_tablet_events", False))
    attach_virtio_tablet = bool(args.with_virtio_tablet or need_input_tablet_events)
    need_blk_resize = bool(getattr(args, "with_blk_resize", False))
    need_msix_check = bool(
        args.require_virtio_net_msix or args.require_virtio_blk_msix or args.require_virtio_snd_msix
    )

    input_events_req_flags: list[str] = []
    if bool(args.with_input_events):
        input_events_req_flags.append("--with-input-events/--with-virtio-input-events")
    if need_input_wheel:
        input_events_req_flags.append("--with-input-wheel/--with-virtio-input-wheel")
    if need_input_events_extended:
        input_events_req_flags.append("--with-input-events-extended/--with-input-events-extra")
    input_events_req_flags_desc = "/".join(input_events_req_flags)

    def resolve_vectors(per_device: Optional[int]) -> Optional[int]:
        return per_device if per_device is not None else args.virtio_msix_vectors

    virtio_net_vectors = resolve_vectors(args.virtio_net_vectors)
    virtio_blk_vectors = resolve_vectors(args.virtio_blk_vectors)
    virtio_snd_vectors = resolve_vectors(args.virtio_snd_vectors)
    virtio_input_vectors = resolve_vectors(args.virtio_input_vectors)
    virtio_net_vectors_flag = "--virtio-net-vectors" if args.virtio_net_vectors is not None else "--virtio-msix-vectors"
    virtio_blk_vectors_flag = "--virtio-blk-vectors" if args.virtio_blk_vectors is not None else "--virtio-msix-vectors"
    virtio_snd_vectors_flag = "--virtio-snd-vectors" if args.virtio_snd_vectors is not None else "--virtio-msix-vectors"
    virtio_input_vectors_flag = "--virtio-input-vectors" if args.virtio_input_vectors is not None else "--virtio-msix-vectors"

    if args.require_intx and args.require_msi:
        parser.error("--require-intx and --require-msi are mutually exclusive")

    if args.virtio_msix_vectors is not None and args.virtio_msix_vectors <= 0:
        parser.error("--virtio-msix-vectors must be a positive integer")
    if args.virtio_net_vectors is not None and args.virtio_net_vectors <= 0:
        parser.error("--virtio-net-vectors must be a positive integer")
    if args.virtio_blk_vectors is not None and args.virtio_blk_vectors <= 0:
        parser.error("--virtio-blk-vectors must be a positive integer")
    if args.virtio_snd_vectors is not None and args.virtio_snd_vectors <= 0:
        parser.error("--virtio-snd-vectors must be a positive integer")
    if args.virtio_input_vectors is not None and args.virtio_input_vectors <= 0:
        parser.error("--virtio-input-vectors must be a positive integer")

    if args.require_virtio_snd_msix and not args.enable_virtio_snd:
        parser.error("--require-virtio-snd-msix requires --with-virtio-snd/--enable-virtio-snd")
    if args.udp_port <= 0 or args.udp_port > 65535:
        parser.error("--udp-port must be in the range 1..65535")

    if not args.enable_virtio_snd:
        if args.with_snd_buffer_limits:
            parser.error("--with-snd-buffer-limits requires --with-virtio-snd/--enable-virtio-snd")
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

    if args.virtio_snd_vectors is not None and not args.enable_virtio_snd:
        parser.error("--virtio-snd-vectors requires --with-virtio-snd/--enable-virtio-snd")
    if need_blk_resize:
        if args.virtio_transitional:
            parser.error(
                "--with-blk-resize is incompatible with --virtio-transitional "
                "(blk resize uses the contract-v1 drive layout with id=drive0)"
            )
        if int(args.blk_resize_delta_mib) <= 0:
            parser.error("--blk-resize-delta-mib must be > 0 when --with-blk-resize is enabled")

    if need_input_events:
        # In default (contract-v1) mode we already validate virtio-keyboard-pci/virtio-mouse-pci via
        # `_assert_qemu_supports_aero_w7_virtio_contract_v1`. In transitional mode virtio-input is
        # optional, but input event injection requires these devices to exist.
        if not _qemu_has_device(args.qemu_system, "virtio-keyboard-pci") or not _qemu_has_device(
            args.qemu_system, "virtio-mouse-pci"
        ):
            parser.error(
                "--with-input-events/--with-virtio-input-events"
                "/--with-input-wheel/--with-virtio-input-wheel"
                "/--with-input-events-extended/--with-input-events-extra requires "
                "QEMU virtio-keyboard-pci and virtio-mouse-pci support. Upgrade QEMU or omit input event injection."
            )

    if need_input_media_keys and not _qemu_has_device(args.qemu_system, "virtio-keyboard-pci"):
        parser.error(
            "--with-input-media-keys requires QEMU virtio-keyboard-pci support. Upgrade QEMU or omit media key injection."
        )

    if attach_virtio_tablet:
        try:
            help_text = _qemu_device_list_help_text(args.qemu_system)
        except RuntimeError as e:
            parser.error(str(e))
        if "virtio-tablet-pci" not in help_text:
            parser.error(
                "--with-virtio-tablet/--with-input-tablet-events/--with-tablet-events/--with-virtio-input-tablet-events requires "
                "QEMU virtio-tablet-pci support. Upgrade QEMU or omit tablet support."
            )

    if not args.virtio_transitional:
        try:
            _assert_qemu_supports_aero_w7_virtio_contract_v1(
                args.qemu_system,
                with_virtio_snd=args.enable_virtio_snd,
                with_virtio_tablet=attach_virtio_tablet,
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
    # - optionally introspect PCI state to verify MSI-X enablement (`--require-virtio-*-msix`)
    #
    # Historically we enabled QMP only when we needed a graceful exit for `-audiodev wav` output, so we
    # wouldn't introduce extra host port/socket dependencies in non-audio harness runs. Input injection
    # also requires QMP, but remains opt-in via:
    # - --with-input-events / --with-virtio-input-events
    # - --with-input-media-keys / --with-virtio-input-media-keys
    # - --with-input-wheel
    # - --with-input-events-extended / --with-input-events-extra
    # - --with-input-tablet-events / --with-tablet-events
    # - --with-blk-resize
    # - --require-virtio-*-msix
    # - --qemu-preflight-pci / --qmp-preflight-pci
    use_qmp = (
        (args.enable_virtio_snd and args.virtio_snd_audio_backend == "wav")
        or need_input_events
        or need_input_media_keys
        or need_input_tablet_events
        or need_blk_resize
        or need_msix_check
        or bool(args.qemu_preflight_pci)
    )
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
                if (
                    need_input_events
                    or need_input_media_keys
                    or need_input_tablet_events
                    or need_blk_resize
                    or need_msix_check
                    or bool(args.qemu_preflight_pci)
                ):
                    req_flags: list[str] = []
                    if bool(args.with_input_events):
                        req_flags.append("--with-input-events/--with-virtio-input-events")
                    if need_input_media_keys:
                        req_flags.append("--with-input-media-keys/--with-virtio-input-media-keys")
                    if need_input_wheel:
                        req_flags.append("--with-input-wheel/--with-virtio-input-wheel")
                    if need_input_events_extended:
                        req_flags.append("--with-input-events-extended/--with-input-events-extra")
                    if need_input_tablet_events:
                        req_flags.append("--with-input-tablet-events/--with-tablet-events")
                    if need_blk_resize:
                        req_flags.append("--with-blk-resize")
                    if need_msix_check:
                        req_flags.append("--require-virtio-*-msix")
                    if bool(args.qemu_preflight_pci):
                        req_flags.append("--qemu-preflight-pci/--qmp-preflight-pci")
                    print(
                        f"ERROR: {'/'.join(req_flags)} requires QMP, but a free TCP port could not be allocated",
                        file=sys.stderr,
                    )
                    return 2

                print(
                    "WARNING: disabling QMP shutdown because a free TCP port could not be allocated",
                    file=sys.stderr,
                )
            else:
                qmp_endpoint = _QmpEndpoint(tcp_host="127.0.0.1", tcp_port=port)
    if (
        need_input_events
        or need_input_media_keys
        or need_input_tablet_events
        or need_blk_resize
        or need_msix_check
        or bool(args.qemu_preflight_pci)
    ) and qmp_endpoint is None:
        req_flags: list[str] = []
        if bool(args.with_input_events):
            req_flags.append("--with-input-events/--with-virtio-input-events")
        if need_input_media_keys:
            req_flags.append("--with-input-media-keys/--with-virtio-input-media-keys")
        if need_input_wheel:
            req_flags.append("--with-input-wheel/--with-virtio-input-wheel")
        if need_input_events_extended:
            req_flags.append("--with-input-events-extended/--with-input-events-extra")
        if need_input_tablet_events:
            req_flags.append("--with-input-tablet-events/--with-tablet-events")
        if need_blk_resize:
            req_flags.append("--with-blk-resize")
        if need_msix_check:
            req_flags.append("--require-virtio-*-msix")
        if bool(args.qemu_preflight_pci):
            req_flags.append("--qemu-preflight-pci/--qmp-preflight-pci")
        print(
            f"ERROR: {'/'.join(req_flags)} requires QMP, but a QMP endpoint could not be allocated",
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

        udp_server: Optional[_UdpEchoServer] = None
        if args.disable_udp:
            print("UDP echo server disabled (--disable-udp)")
        else:
            try:
                udp_server = _UdpEchoServer("127.0.0.1", args.udp_port)
                udp_server.start()
            except OSError as e:
                print(
                    f"ERROR: failed to bind UDP echo server on 127.0.0.1:{args.udp_port} (port in use?): {e}",
                    file=sys.stderr,
                )
                httpd.shutdown()
                return 2

            print(
                f"Starting UDP echo server on 127.0.0.1:{udp_server.port} "
                f"(guest: 10.0.2.2:{udp_server.port})"
            )

        wav_path: Optional[Path] = None
        if args.virtio_transitional:
            # Transitional mode: by default use `-drive if=virtio` so we stay close to QEMU defaults.
            # When vectors overrides are requested, switch to an explicit virtio-blk-pci device so we
            # can pass `vectors=<N>` (when supported by this QEMU build).
            virtio_blk_args: list[str] = []
            if virtio_blk_vectors is None:
                drive = f"file={_qemu_quote_keyval_value(str(disk_image))},if=virtio,cache=writeback"
                if args.snapshot:
                    drive += ",snapshot=on"
                virtio_blk_args = ["-drive", drive]
            else:
                drive_id = "drive0"
                drive = f"file={_qemu_quote_keyval_value(str(disk_image))},if=none,id={drive_id},cache=writeback"
                if args.snapshot:
                    drive += ",snapshot=on"
                virtio_blk = _qemu_device_arg_maybe_add_vectors(
                    args.qemu_system,
                    "virtio-blk-pci",
                    f"virtio-blk-pci,drive={drive_id}",
                    virtio_blk_vectors,
                    flag_name=virtio_blk_vectors_flag,
                )
                virtio_blk_args = ["-drive", drive, "-device", virtio_blk]

            virtio_input_args: list[str] = []
            have_kbd = _qemu_has_device(args.qemu_system, "virtio-keyboard-pci")
            have_mouse = _qemu_has_device(args.qemu_system, "virtio-mouse-pci")
            if have_kbd:
                kbd = _qemu_device_arg_maybe_add_vectors(
                    args.qemu_system,
                    "virtio-keyboard-pci",
                    f"virtio-keyboard-pci,id={_VIRTIO_INPUT_QMP_KEYBOARD_ID}",
                    virtio_input_vectors,
                    flag_name=virtio_input_vectors_flag,
                )
                virtio_input_args += ["-device", kbd]
            if have_mouse:
                mouse = _qemu_device_arg_maybe_add_vectors(
                    args.qemu_system,
                    "virtio-mouse-pci",
                    f"virtio-mouse-pci,id={_VIRTIO_INPUT_QMP_MOUSE_ID}",
                    virtio_input_vectors,
                    flag_name=virtio_input_vectors_flag,
                )
                virtio_input_args += ["-device", mouse]
            if not (have_kbd and have_mouse):
                print(
                    "WARNING: QEMU does not advertise virtio-keyboard-pci/virtio-mouse-pci. "
                    "The guest virtio-input selftest will likely FAIL. Upgrade QEMU or adjust the guest image/selftest expectations.",
                    file=sys.stderr,
                )
            if attach_virtio_tablet:
                tablet = _qemu_device_arg_maybe_add_vectors(
                    args.qemu_system,
                    "virtio-tablet-pci",
                    _qemu_virtio_tablet_pci_device_arg(disable_legacy=False, pci_revision=None),
                    virtio_input_vectors,
                    flag_name=virtio_input_vectors_flag,
                )
                virtio_input_args += ["-device", tablet]

            attached_virtio_input = bool(virtio_input_args)

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
                _qemu_device_arg_maybe_add_vectors(
                    args.qemu_system,
                    "virtio-net-pci",
                    "virtio-net-pci,netdev=net0",
                    virtio_net_vectors,
                    flag_name=virtio_net_vectors_flag,
                ),
            ] + virtio_input_args + virtio_blk_args + virtio_snd_args + qemu_extra
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

            virtio_net = _qemu_device_arg_maybe_add_vectors(
                args.qemu_system,
                "virtio-net-pci",
                f"virtio-net-pci,netdev=net0,disable-legacy=on,x-pci-revision={aero_pci_rev}",
                virtio_net_vectors,
                flag_name=virtio_net_vectors_flag,
            )
            virtio_blk = _qemu_device_arg_maybe_add_vectors(
                args.qemu_system,
                "virtio-blk-pci",
                f"virtio-blk-pci,drive={drive_id},disable-legacy=on,x-pci-revision={aero_pci_rev}",
                virtio_blk_vectors,
                flag_name=virtio_blk_vectors_flag,
            )
            virtio_kbd = _qemu_device_arg_maybe_add_vectors(
                args.qemu_system,
                "virtio-keyboard-pci",
                f"virtio-keyboard-pci,id={_VIRTIO_INPUT_QMP_KEYBOARD_ID},disable-legacy=on,x-pci-revision={aero_pci_rev}",
                virtio_input_vectors,
                flag_name=virtio_input_vectors_flag,
            )
            virtio_mouse = _qemu_device_arg_maybe_add_vectors(
                args.qemu_system,
                "virtio-mouse-pci",
                f"virtio-mouse-pci,id={_VIRTIO_INPUT_QMP_MOUSE_ID},disable-legacy=on,x-pci-revision={aero_pci_rev}",
                virtio_input_vectors,
                flag_name=virtio_input_vectors_flag,
            )
            attached_virtio_input = True
            virtio_tablet = None
            if attach_virtio_tablet:
                virtio_tablet = _qemu_device_arg_maybe_add_vectors(
                    args.qemu_system,
                    "virtio-tablet-pci",
                    _qemu_virtio_tablet_pci_device_arg(
                        disable_legacy=True,
                        pci_revision=aero_pci_rev,
                    ),
                    virtio_input_vectors,
                    flag_name=virtio_input_vectors_flag,
                )

            virtio_snd_args: list[str] = []
            if args.enable_virtio_snd:
                try:
                    device_arg = _get_qemu_virtio_sound_device_arg(
                        args.qemu_system, disable_legacy=True, pci_revision=aero_pci_rev
                    )
                    snd_device = _detect_virtio_snd_device(args.qemu_system)
                    device_arg = _qemu_device_arg_maybe_add_vectors(
                        args.qemu_system,
                        snd_device,
                        device_arg,
                        virtio_snd_vectors,
                        flag_name=virtio_snd_vectors_flag,
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
            ]
            if virtio_tablet is not None:
                qemu_args += ["-device", virtio_tablet]
            qemu_args += [
                "-drive",
                drive,
                "-device",
                virtio_blk,
            ] + virtio_snd_args + qemu_extra

        irq_mode_devices = ["virtio-blk", "virtio-net"]
        if attached_virtio_input:
            irq_mode_devices.append("virtio-input")
        if args.enable_virtio_snd:
            irq_mode_devices.append("virtio-snd")

        print("Launching QEMU:")
        print("  " + " ".join(shlex.quote(str(a)) for a in qemu_args))
        stderr_f = qemu_stderr_log.open("wb")
        proc = subprocess.Popen(qemu_args, stderr=stderr_f)
        result_code: Optional[int] = None
        try:
            if args.qemu_preflight_pci:
                if qmp_endpoint is None:
                    raise AssertionError("--qemu-preflight-pci requested but QMP endpoint is not configured")
                try:
                    _qmp_pci_preflight(
                        qmp_endpoint,
                        virtio_transitional=bool(args.virtio_transitional),
                        with_virtio_snd=bool(args.enable_virtio_snd),
                        with_virtio_tablet=bool(attach_virtio_tablet),
                    )
                except Exception as e:
                    print(f"FAIL: QEMU_PCI_PREFLIGHT_FAILED: {e}", file=sys.stderr)
                    _print_qemu_stderr_tail(qemu_stderr_log)
                    return 2

            pos = 0
            tail = b""
            irq_diag_markers: dict[str, dict[str, str]] = {}
            irq_diag_carry = b""
            virtio_blk_marker_line: Optional[str] = None
            virtio_blk_marker_carry = b""
            virtio_input_msix_marker: Optional[_VirtioInputMsixMarker] = None
            saw_virtio_blk_pass = False
            saw_virtio_blk_fail = False
            virtio_blk_marker_time: Optional[float] = None
            saw_virtio_blk_resize_ready = False
            saw_virtio_blk_resize_pass = False
            saw_virtio_blk_resize_fail = False
            saw_virtio_blk_resize_skip = False
            blk_resize_old_bytes: Optional[int] = None
            blk_resize_new_bytes: Optional[int] = None
            blk_resize_requested = False
            saw_virtio_input_pass = False
            saw_virtio_input_fail = False
            virtio_input_marker_time: Optional[float] = None
            saw_virtio_input_events_ready = False
            saw_virtio_input_events_pass = False
            saw_virtio_input_events_fail = False
            saw_virtio_input_events_skip = False
            saw_virtio_input_media_keys_ready = False
            saw_virtio_input_media_keys_pass = False
            saw_virtio_input_media_keys_fail = False
            saw_virtio_input_media_keys_skip = False
            input_media_keys_inject_attempts = 0
            next_input_media_keys_inject = 0.0
            saw_virtio_input_wheel_pass = False
            saw_virtio_input_wheel_fail = False
            saw_virtio_input_wheel_skip = False
            saw_virtio_input_events_modifiers_pass = False
            saw_virtio_input_events_modifiers_fail = False
            saw_virtio_input_events_modifiers_skip = False
            saw_virtio_input_events_buttons_pass = False
            saw_virtio_input_events_buttons_fail = False
            saw_virtio_input_events_buttons_skip = False
            saw_virtio_input_events_wheel_pass = False
            saw_virtio_input_events_wheel_fail = False
            saw_virtio_input_events_wheel_skip = False
            input_events_inject_attempts = 0
            max_input_events_inject_attempts = 30 if need_input_events_extended else 20
            next_input_events_inject = 0.0
            saw_virtio_input_tablet_events_ready = False
            saw_virtio_input_tablet_events_pass = False
            saw_virtio_input_tablet_events_fail = False
            saw_virtio_input_tablet_events_skip = False
            input_tablet_events_inject_attempts = 0
            next_input_tablet_events_inject = 0.0
            saw_virtio_snd_pass = False
            saw_virtio_snd_skip = False
            saw_virtio_snd_fail = False
            saw_virtio_snd_capture_pass = False
            saw_virtio_snd_capture_skip = False
            saw_virtio_snd_capture_fail = False
            saw_virtio_snd_duplex_pass = False
            saw_virtio_snd_duplex_skip = False
            saw_virtio_snd_duplex_fail = False
            saw_virtio_snd_buffer_limits_pass = False
            saw_virtio_snd_buffer_limits_skip = False
            saw_virtio_snd_buffer_limits_fail = False
            saw_virtio_net_pass = False
            saw_virtio_net_fail = False
            msix_checked = False
            saw_virtio_net_udp_pass = False
            saw_virtio_net_udp_fail = False
            saw_virtio_net_udp_skip = False
            require_per_test_markers = not args.virtio_transitional
            deadline = time.monotonic() + args.timeout_seconds

            while time.monotonic() < deadline:
                chunk, pos = _read_new_bytes(serial_log, pos)
                if chunk:
                    if args.follow_serial:
                        sys.stdout.write(chunk.decode("utf-8", errors="replace"))
                        sys.stdout.flush()

                    # Capture standalone guest IRQ diagnostics markers (`virtio-<dev>-irq|INFO/WARN|...`)
                    # incrementally so they are not lost if the rolling tail buffer is truncated.
                    irq_diag_carry = _update_virtio_irq_markers_from_chunk(
                        irq_diag_markers, chunk, carry=irq_diag_carry
                    )
                    virtio_blk_marker_line, virtio_blk_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_blk_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|",
                        carry=virtio_blk_marker_carry,
                    )
                    tail += chunk
                    if len(tail) > 131072:
                        tail = tail[-131072:]
                    if virtio_input_msix_marker is None or b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|" in tail:
                        marker = _parse_virtio_input_msix_marker(tail)
                        if marker is not None:
                            virtio_input_msix_marker = marker

                    if not saw_virtio_blk_pass and b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS" in tail:
                        saw_virtio_blk_pass = True
                        if virtio_blk_marker_time is None:
                            virtio_blk_marker_time = time.monotonic()
                    if not saw_virtio_blk_fail and b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|FAIL" in tail:
                        saw_virtio_blk_fail = True
                        if virtio_blk_marker_time is None:
                            virtio_blk_marker_time = time.monotonic()

                    if not saw_virtio_blk_resize_ready:
                        ready = _try_extract_virtio_blk_resize_ready(tail)
                        if ready is not None:
                            saw_virtio_blk_resize_ready = True
                            blk_resize_old_bytes = int(ready.old_bytes)
                            if need_blk_resize:
                                delta_bytes = int(args.blk_resize_delta_mib) * 1024 * 1024
                                blk_resize_new_bytes = _virtio_blk_resize_compute_new_bytes(
                                    blk_resize_old_bytes, delta_bytes
                                )
                    if (
                        not saw_virtio_blk_resize_pass
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|PASS" in tail
                    ):
                        saw_virtio_blk_resize_pass = True
                    if (
                        not saw_virtio_blk_resize_fail
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|FAIL" in tail
                    ):
                        saw_virtio_blk_resize_fail = True
                    if (
                        not saw_virtio_blk_resize_skip
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|SKIP" in tail
                    ):
                        saw_virtio_blk_resize_skip = True
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
                    if (
                        not saw_virtio_input_media_keys_ready
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|READY" in tail
                    ):
                        saw_virtio_input_media_keys_ready = True
                    if (
                        not saw_virtio_input_media_keys_pass
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|PASS" in tail
                    ):
                        saw_virtio_input_media_keys_pass = True
                    if (
                        not saw_virtio_input_media_keys_fail
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|FAIL" in tail
                    ):
                        saw_virtio_input_media_keys_fail = True
                    if (
                        not saw_virtio_input_media_keys_skip
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|SKIP" in tail
                    ):
                        saw_virtio_input_media_keys_skip = True
                    if (
                        not saw_virtio_input_wheel_pass
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|PASS" in tail
                    ):
                        saw_virtio_input_wheel_pass = True
                    if (
                        not saw_virtio_input_wheel_fail
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|FAIL" in tail
                    ):
                        saw_virtio_input_wheel_fail = True
                    if (
                        not saw_virtio_input_wheel_skip
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|SKIP" in tail
                    ):
                        saw_virtio_input_wheel_skip = True

                    if (
                        not saw_virtio_input_events_modifiers_pass
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-modifiers|PASS" in tail
                    ):
                        saw_virtio_input_events_modifiers_pass = True
                    if (
                        not saw_virtio_input_events_modifiers_fail
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-modifiers|FAIL" in tail
                    ):
                        saw_virtio_input_events_modifiers_fail = True
                    if (
                        not saw_virtio_input_events_modifiers_skip
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-modifiers|SKIP" in tail
                    ):
                        saw_virtio_input_events_modifiers_skip = True
                    if (
                        not saw_virtio_input_events_buttons_pass
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-buttons|PASS" in tail
                    ):
                        saw_virtio_input_events_buttons_pass = True
                    if (
                        not saw_virtio_input_events_buttons_fail
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-buttons|FAIL" in tail
                    ):
                        saw_virtio_input_events_buttons_fail = True
                    if (
                        not saw_virtio_input_events_buttons_skip
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-buttons|SKIP" in tail
                    ):
                        saw_virtio_input_events_buttons_skip = True
                    if (
                        not saw_virtio_input_events_wheel_pass
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-wheel|PASS" in tail
                    ):
                        saw_virtio_input_events_wheel_pass = True
                    if (
                        not saw_virtio_input_events_wheel_fail
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-wheel|FAIL" in tail
                    ):
                        saw_virtio_input_events_wheel_fail = True
                    if (
                        not saw_virtio_input_events_wheel_skip
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-wheel|SKIP" in tail
                    ):
                        saw_virtio_input_events_wheel_skip = True

                    if (
                        not saw_virtio_input_tablet_events_ready
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|READY" in tail
                    ):
                        saw_virtio_input_tablet_events_ready = True
                    if (
                        not saw_virtio_input_tablet_events_pass
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|PASS" in tail
                    ):
                        saw_virtio_input_tablet_events_pass = True
                    if (
                        not saw_virtio_input_tablet_events_fail
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|FAIL" in tail
                    ):
                        saw_virtio_input_tablet_events_fail = True
                    if (
                        not saw_virtio_input_tablet_events_skip
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|SKIP" in tail
                    ):
                        saw_virtio_input_tablet_events_skip = True

                    # If input events are required, fail fast when the guest reports SKIP/FAIL for
                    # virtio-input-events. This saves CI time when the guest image was provisioned
                    # without `--test-input-events`, or when the end-to-end input path is broken.
                    if need_input_events:
                        if saw_virtio_input_events_skip:
                            print(
                                f"FAIL: VIRTIO_INPUT_EVENTS_SKIPPED: virtio-input-events test was skipped (flag_not_set) but "
                                f"{input_events_req_flags_desc} was enabled (provision the guest with --test-input-events)",
                                file=sys.stderr,
                            )
                            _print_tail(serial_log)
                            result_code = 1
                            break
                        if saw_virtio_input_events_fail:
                            print(
                                "FAIL: VIRTIO_INPUT_EVENTS_FAILED: virtio-input-events test reported FAIL while "
                                f"{input_events_req_flags_desc} was enabled",
                                file=sys.stderr,
                            )
                            _print_tail(serial_log)
                            result_code = 1
                            break

                    if need_input_media_keys:
                        if saw_virtio_input_media_keys_skip:
                            print(
                                "FAIL: VIRTIO_INPUT_MEDIA_KEYS_SKIPPED: virtio-input-media-keys test was skipped (flag_not_set) but "
                                "--with-input-media-keys was enabled (provision the guest with --test-input-media-keys)",
                                file=sys.stderr,
                            )
                            _print_tail(serial_log)
                            result_code = 1
                            break
                        if saw_virtio_input_media_keys_fail:
                            print(
                                "FAIL: VIRTIO_INPUT_MEDIA_KEYS_FAILED: virtio-input-media-keys test reported FAIL while --with-input-media-keys was enabled",
                                file=sys.stderr,
                            )
                            _print_tail(serial_log)
                            result_code = 1
                            break

                    if need_input_events_extended:
                        if (
                            saw_virtio_input_events_modifiers_skip
                            or saw_virtio_input_events_buttons_skip
                            or saw_virtio_input_events_wheel_skip
                        ):
                            print(
                                "FAIL: VIRTIO_INPUT_EVENTS_EXTENDED_SKIPPED: virtio-input-events-extended markers were skipped but "
                                "--with-input-events-extended was enabled (provision the guest with --test-input-events-extended)",
                                file=sys.stderr,
                            )
                            _print_tail(serial_log)
                            result_code = 1
                            break
                        if (
                            saw_virtio_input_events_modifiers_fail
                            or saw_virtio_input_events_buttons_fail
                            or saw_virtio_input_events_wheel_fail
                        ):
                            print(
                                "FAIL: VIRTIO_INPUT_EVENTS_EXTENDED_FAILED: one or more virtio-input-events-* markers reported FAIL "
                                "while --with-input-events-extended was enabled",
                                file=sys.stderr,
                            )
                            _print_tail(serial_log)
                            result_code = 1
                            break

                    if need_input_wheel:
                        if saw_virtio_input_wheel_skip:
                            print(
                                "FAIL: VIRTIO_INPUT_WHEEL_SKIPPED: virtio-input-wheel test was skipped but "
                                "--with-input-wheel/--with-virtio-input-wheel was enabled",
                                file=sys.stderr,
                            )
                            _print_tail(serial_log)
                            result_code = 1
                            break
                        if saw_virtio_input_wheel_fail:
                            print(
                                "FAIL: VIRTIO_INPUT_WHEEL_FAILED: virtio-input-wheel test reported FAIL while "
                                "--with-input-wheel/--with-virtio-input-wheel was enabled",
                                file=sys.stderr,
                            )
                            _print_tail(serial_log)
                            result_code = 1
                            break

                    if need_input_tablet_events:
                        if saw_virtio_input_tablet_events_skip:
                            print(
                                "FAIL: VIRTIO_INPUT_TABLET_EVENTS_SKIPPED: virtio-input-tablet-events test was skipped (flag_not_set) but "
                                "--with-input-tablet-events/--with-tablet-events was enabled (provision the guest with --test-input-tablet-events/--test-tablet-events)",
                                file=sys.stderr,
                            )
                            _print_tail(serial_log)
                            result_code = 1
                            break
                        if saw_virtio_input_tablet_events_fail:
                            print(
                                "FAIL: VIRTIO_INPUT_TABLET_EVENTS_FAILED: virtio-input-tablet-events test reported FAIL while --with-input-tablet-events/--with-tablet-events was enabled",
                                file=sys.stderr,
                            )
                            _print_tail(serial_log)
                            result_code = 1
                            break

                    if need_blk_resize:
                        if saw_virtio_blk_resize_skip:
                            print(
                                "FAIL: VIRTIO_BLK_RESIZE_SKIPPED: virtio-blk-resize test was skipped (flag_not_set) but "
                                "--with-blk-resize was enabled (provision the guest with --test-blk-resize)",
                                file=sys.stderr,
                            )
                            _print_tail(serial_log)
                            result_code = 1
                            break
                        if saw_virtio_blk_resize_fail:
                            print(
                                "FAIL: VIRTIO_BLK_RESIZE_FAILED: virtio-blk-resize test reported FAIL while --with-blk-resize was enabled",
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
                    if (
                        not saw_virtio_snd_buffer_limits_pass
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|PASS" in tail
                    ):
                        saw_virtio_snd_buffer_limits_pass = True
                    if (
                        not saw_virtio_snd_buffer_limits_skip
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|SKIP" in tail
                    ):
                        saw_virtio_snd_buffer_limits_skip = True
                    if (
                        not saw_virtio_snd_buffer_limits_fail
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|FAIL" in tail
                    ):
                        saw_virtio_snd_buffer_limits_fail = True
                    if not saw_virtio_net_pass and b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS" in tail:
                        saw_virtio_net_pass = True
                    if not saw_virtio_net_fail and b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|FAIL" in tail:
                        saw_virtio_net_fail = True
                    if (
                        not saw_virtio_net_udp_pass
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|PASS" in tail
                    ):
                        saw_virtio_net_udp_pass = True
                    if (
                        not saw_virtio_net_udp_fail
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|FAIL" in tail
                    ):
                        saw_virtio_net_udp_fail = True
                    if (
                        not saw_virtio_net_udp_skip
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|SKIP" in tail
                    ):
                        saw_virtio_net_udp_skip = True

                    # If requested, verify MSI-X enablement once we're confident the relevant drivers
                    # have loaded (i.e. after the corresponding guest test marker has appeared).
                    #
                    # This is intentionally earlier than waiting for RESULT|PASS so we can still run the
                    # QMP check even if the guest shuts down immediately after reporting results.
                    if need_msix_check and not msix_checked:
                        msix_ready = True
                        if args.require_virtio_blk_msix and not (saw_virtio_blk_pass or saw_virtio_blk_fail):
                            msix_ready = False
                        if args.require_virtio_net_msix and not (saw_virtio_net_pass or saw_virtio_net_fail):
                            msix_ready = False
                        if args.require_virtio_snd_msix and not (
                            saw_virtio_snd_pass or saw_virtio_snd_fail or saw_virtio_snd_skip
                        ):
                            msix_ready = False

                        if msix_ready:
                            assert qmp_endpoint is not None
                            msg = _require_virtio_msix_check_failure_message(
                                qmp_endpoint,
                                require_virtio_net_msix=bool(args.require_virtio_net_msix),
                                require_virtio_blk_msix=bool(args.require_virtio_blk_msix),
                                require_virtio_snd_msix=bool(args.require_virtio_snd_msix),
                            )
                            msix_checked = True
                            if msg is not None:
                                print(msg, file=sys.stderr)
                                _print_tail(serial_log)
                                result_code = 1
                                break

                    if b"AERO_VIRTIO_SELFTEST|RESULT|PASS" in tail:
                        if require_per_test_markers:
                            # Require per-test markers so older selftest binaries cannot
                            # accidentally pass the host harness.
                            if saw_virtio_blk_fail:
                                print(
                                    "FAIL: VIRTIO_BLK_FAILED: selftest RESULT=PASS but virtio-blk test reported FAIL",
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
                            if need_blk_resize:
                                if saw_virtio_blk_resize_fail:
                                    print(
                                        "FAIL: VIRTIO_BLK_RESIZE_FAILED: selftest RESULT=PASS but virtio-blk-resize test reported FAIL",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_blk_resize_pass:
                                    if saw_virtio_blk_resize_skip:
                                        print(
                                            "FAIL: VIRTIO_BLK_RESIZE_SKIPPED: virtio-blk-resize test was skipped (flag_not_set) but "
                                            "--with-blk-resize was enabled (provision the guest with --test-blk-resize)",
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(
                                            "FAIL: MISSING_VIRTIO_BLK_RESIZE: selftest RESULT=PASS but did not emit virtio-blk-resize test marker "
                                            "while --with-blk-resize was enabled",
                                            file=sys.stderr,
                                        )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if saw_virtio_input_fail:
                                print(
                                    "FAIL: VIRTIO_INPUT_FAILED: selftest RESULT=PASS but virtio-input test reported FAIL",
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
                                        f"while {input_events_req_flags_desc} was enabled",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_input_events_pass:
                                    if saw_virtio_input_events_skip:
                                        print(
                                            "FAIL: VIRTIO_INPUT_EVENTS_SKIPPED: virtio-input-events test was skipped (flag_not_set) but "
                                            f"{input_events_req_flags_desc} was enabled (provision the guest with --test-input-events)",
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(
                                            "FAIL: MISSING_VIRTIO_INPUT_EVENTS: selftest RESULT=PASS but did not emit virtio-input-events test marker "
                                            f"while {input_events_req_flags_desc} was enabled",
                                            file=sys.stderr,
                                        )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if need_input_media_keys:
                                if saw_virtio_input_media_keys_fail:
                                    print(
                                        "FAIL: VIRTIO_INPUT_MEDIA_KEYS_FAILED: selftest RESULT=PASS but virtio-input-media-keys test reported FAIL "
                                        "while --with-input-media-keys was enabled",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_input_media_keys_pass:
                                    if saw_virtio_input_media_keys_skip:
                                        print(
                                            "FAIL: VIRTIO_INPUT_MEDIA_KEYS_SKIPPED: virtio-input-media-keys test was skipped (flag_not_set) but "
                                            "--with-input-media-keys was enabled (provision the guest with --test-input-media-keys)",
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(
                                            "FAIL: MISSING_VIRTIO_INPUT_MEDIA_KEYS: selftest RESULT=PASS but did not emit virtio-input-media-keys test marker "
                                            "while --with-input-media-keys was enabled",
                                            file=sys.stderr,
                                        )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if need_input_events_extended:
                                if (
                                    saw_virtio_input_events_modifiers_fail
                                    or saw_virtio_input_events_buttons_fail
                                    or saw_virtio_input_events_wheel_fail
                                ):
                                    print(
                                        "FAIL: VIRTIO_INPUT_EVENTS_EXTENDED_FAILED: selftest RESULT=PASS but a virtio-input-events-* marker reported FAIL "
                                        "while --with-input-events-extended was enabled",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break

                                # Each sub-marker must PASS (not SKIP/missing).
                                for name, saw_pass, saw_skip in (
                                    (
                                        "virtio-input-events-modifiers",
                                        saw_virtio_input_events_modifiers_pass,
                                        saw_virtio_input_events_modifiers_skip,
                                    ),
                                    (
                                        "virtio-input-events-buttons",
                                        saw_virtio_input_events_buttons_pass,
                                        saw_virtio_input_events_buttons_skip,
                                    ),
                                    (
                                        "virtio-input-events-wheel",
                                        saw_virtio_input_events_wheel_pass,
                                        saw_virtio_input_events_wheel_skip,
                                    ),
                                ):
                                    if saw_pass:
                                        continue
                                    if saw_skip:
                                        print(
                                            f"FAIL: VIRTIO_INPUT_EVENTS_EXTENDED_SKIPPED: {name} was skipped (flag_not_set) but "
                                            "--with-input-events-extended was enabled (provision the guest with --test-input-events-extended)",
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(
                                            f"FAIL: MISSING_VIRTIO_INPUT_EVENTS_EXTENDED: did not observe {name} PASS marker while "
                                            "--with-input-events-extended was enabled",
                                            file=sys.stderr,
                                        )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if result_code is not None:
                                    break

                            if need_input_wheel:
                                if saw_virtio_input_wheel_fail:
                                    print(
                                        "FAIL: VIRTIO_INPUT_WHEEL_FAILED: selftest RESULT=PASS but virtio-input-wheel test reported FAIL "
                                        "while --with-input-wheel/--with-virtio-input-wheel was enabled",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_input_wheel_pass:
                                    if saw_virtio_input_wheel_skip:
                                        print(
                                            "FAIL: VIRTIO_INPUT_WHEEL_SKIPPED: virtio-input-wheel test was skipped but "
                                            "--with-input-wheel/--with-virtio-input-wheel was enabled",
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(
                                            "FAIL: MISSING_VIRTIO_INPUT_WHEEL: selftest RESULT=PASS but did not emit virtio-input-wheel test marker "
                                            "while --with-input-wheel/--with-virtio-input-wheel was enabled",
                                            file=sys.stderr,
                                        )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break

                            if need_input_tablet_events:
                                if saw_virtio_input_tablet_events_fail:
                                    print(
                                        "FAIL: VIRTIO_INPUT_TABLET_EVENTS_FAILED: selftest RESULT=PASS but virtio-input-tablet-events test reported FAIL "
                                        "while --with-input-tablet-events/--with-tablet-events was enabled",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_input_tablet_events_pass:
                                    if saw_virtio_input_tablet_events_skip:
                                        print(
                                            "FAIL: VIRTIO_INPUT_TABLET_EVENTS_SKIPPED: virtio-input-tablet-events test was skipped (flag_not_set) but "
                                            "--with-input-tablet-events/--with-tablet-events was enabled (provision the guest with --test-input-tablet-events/--test-tablet-events)",
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(
                                            "FAIL: MISSING_VIRTIO_INPUT_TABLET_EVENTS: selftest RESULT=PASS but did not emit virtio-input-tablet-events test marker "
                                            "while --with-input-tablet-events/--with-tablet-events was enabled",
                                            file=sys.stderr,
                                        )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if saw_virtio_snd_fail:
                                print(
                                    "FAIL: VIRTIO_SND_FAILED: selftest RESULT=PASS but virtio-snd test reported FAIL",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break

                            if args.enable_virtio_snd:
                                # When we explicitly attach virtio-snd, the guest test must actually run and PASS
                                # (it must not be skipped via --disable-snd).
                                if not saw_virtio_snd_pass:
                                    msg = "FAIL: MISSING_VIRTIO_SND: virtio-snd test did not PASS while --with-virtio-snd was enabled"
                                    if saw_virtio_snd_skip:
                                        msg = _virtio_snd_skip_failure_message(tail)
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if saw_virtio_snd_capture_fail:
                                    print(
                                        "FAIL: VIRTIO_SND_CAPTURE_FAILED: selftest RESULT=PASS but virtio-snd-capture test reported FAIL",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_snd_capture_pass:
                                    msg = "FAIL: MISSING_VIRTIO_SND_CAPTURE: virtio-snd capture test did not PASS while --with-virtio-snd was enabled"
                                    if saw_virtio_snd_capture_skip:
                                        msg = _virtio_snd_capture_skip_failure_message(tail)
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if saw_virtio_snd_duplex_fail:
                                    print(
                                        "FAIL: VIRTIO_SND_DUPLEX_FAILED: selftest RESULT=PASS but virtio-snd-duplex test reported FAIL",
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

                                if args.with_snd_buffer_limits:
                                    msg = _virtio_snd_buffer_limits_required_failure_message(tail)
                                    if msg is not None:
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
                                        "FAIL: VIRTIO_SND_CAPTURE_FAILED: selftest RESULT=PASS but virtio-snd-capture test reported FAIL",
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
                                        "FAIL: VIRTIO_SND_DUPLEX_FAILED: selftest RESULT=PASS but virtio-snd-duplex test reported FAIL",
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
                                    "FAIL: VIRTIO_NET_FAILED: selftest RESULT=PASS but virtio-net test reported FAIL",
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
                            if not args.disable_udp:
                                if saw_virtio_net_udp_fail:
                                    print(
                                        "FAIL: VIRTIO_NET_UDP_FAILED: selftest RESULT=PASS but virtio-net-udp test reported FAIL",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_net_udp_pass:
                                    if saw_virtio_net_udp_skip:
                                        print(
                                            "FAIL: VIRTIO_NET_UDP_SKIPPED: virtio-net-udp test was skipped but UDP testing is enabled (update/provision the guest selftest)",
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(
                                            "FAIL: MISSING_VIRTIO_NET_UDP: selftest RESULT=PASS but did not emit virtio-net-udp test marker",
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
                                    "FAIL: VIRTIO_SND_FAILED: selftest RESULT=PASS but virtio-snd test reported FAIL",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if not saw_virtio_snd_pass:
                                msg = "FAIL: MISSING_VIRTIO_SND: virtio-snd test did not PASS while --with-virtio-snd was enabled"
                                if saw_virtio_snd_skip:
                                    msg = _virtio_snd_skip_failure_message(tail)
                                print(msg, file=sys.stderr)
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if saw_virtio_snd_capture_fail:
                                print(
                                    "FAIL: VIRTIO_SND_CAPTURE_FAILED: selftest RESULT=PASS but virtio-snd-capture test reported FAIL",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if not saw_virtio_snd_capture_pass:
                                msg = "FAIL: MISSING_VIRTIO_SND_CAPTURE: virtio-snd capture test did not PASS while --with-virtio-snd was enabled"
                                if saw_virtio_snd_capture_skip:
                                    msg = _virtio_snd_capture_skip_failure_message(tail)
                                print(msg, file=sys.stderr)
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if saw_virtio_snd_duplex_fail:
                                print(
                                    "FAIL: VIRTIO_SND_DUPLEX_FAILED: selftest RESULT=PASS but virtio-snd-duplex test reported FAIL",
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

                            if args.with_snd_buffer_limits:
                                msg = _virtio_snd_buffer_limits_required_failure_message(tail)
                                if msg is not None:
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                        if need_input_events:
                            if saw_virtio_input_events_fail:
                                print(
                                    "FAIL: VIRTIO_INPUT_EVENTS_FAILED: virtio-input-events test reported FAIL while "
                                    f"{input_events_req_flags_desc} was enabled",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if not saw_virtio_input_events_pass:
                                if saw_virtio_input_events_skip:
                                    print(
                                        "FAIL: VIRTIO_INPUT_EVENTS_SKIPPED: virtio-input-events test was skipped (flag_not_set) but "
                                        f"{input_events_req_flags_desc} was enabled (provision the guest with --test-input-events)",
                                        file=sys.stderr,
                                    )
                                else:
                                    print(
                                        "FAIL: MISSING_VIRTIO_INPUT_EVENTS: did not observe virtio-input-events PASS marker while "
                                        f"{input_events_req_flags_desc} was enabled",
                                        file=sys.stderr,
                                    )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                        if need_input_media_keys:
                            if saw_virtio_input_media_keys_fail:
                                print(
                                    "FAIL: VIRTIO_INPUT_MEDIA_KEYS_FAILED: virtio-input-media-keys test reported FAIL while --with-input-media-keys was enabled",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if not saw_virtio_input_media_keys_pass:
                                if saw_virtio_input_media_keys_skip:
                                    print(
                                        "FAIL: VIRTIO_INPUT_MEDIA_KEYS_SKIPPED: virtio-input-media-keys test was skipped (flag_not_set) but "
                                        "--with-input-media-keys was enabled (provision the guest with --test-input-media-keys)",
                                        file=sys.stderr,
                                    )
                                else:
                                    print(
                                        "FAIL: MISSING_VIRTIO_INPUT_MEDIA_KEYS: did not observe virtio-input-media-keys PASS marker while "
                                        "--with-input-media-keys was enabled",
                                        file=sys.stderr,
                                    )
                                _print_tail(serial_log)
                                result_code = 1
                                break

                        if need_blk_resize:
                            if saw_virtio_blk_resize_fail:
                                print(
                                    "FAIL: VIRTIO_BLK_RESIZE_FAILED: virtio-blk-resize test reported FAIL while --with-blk-resize was enabled",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if not saw_virtio_blk_resize_pass:
                                if saw_virtio_blk_resize_skip:
                                    print(
                                        "FAIL: VIRTIO_BLK_RESIZE_SKIPPED: virtio-blk-resize test was skipped (flag_not_set) but "
                                        "--with-blk-resize was enabled (provision the guest with --test-blk-resize)",
                                        file=sys.stderr,
                                    )
                                else:
                                    print(
                                        "FAIL: MISSING_VIRTIO_BLK_RESIZE: did not observe virtio-blk-resize PASS marker while --with-blk-resize was enabled",
                                        file=sys.stderr,
                                    )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                        if need_input_events_extended:
                            if (
                                saw_virtio_input_events_modifiers_fail
                                or saw_virtio_input_events_buttons_fail
                                or saw_virtio_input_events_wheel_fail
                            ):
                                print(
                                    "FAIL: VIRTIO_INPUT_EVENTS_EXTENDED_FAILED: a virtio-input-events-* marker reported FAIL while "
                                    "--with-input-events-extended was enabled",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            # Each sub-marker must PASS (not SKIP/missing).
                            for name, saw_pass, saw_skip in (
                                (
                                    "virtio-input-events-modifiers",
                                    saw_virtio_input_events_modifiers_pass,
                                    saw_virtio_input_events_modifiers_skip,
                                ),
                                (
                                    "virtio-input-events-buttons",
                                    saw_virtio_input_events_buttons_pass,
                                    saw_virtio_input_events_buttons_skip,
                                ),
                                (
                                    "virtio-input-events-wheel",
                                    saw_virtio_input_events_wheel_pass,
                                    saw_virtio_input_events_wheel_skip,
                                ),
                            ):
                                if saw_pass:
                                    continue
                                if saw_skip:
                                    print(
                                        f"FAIL: VIRTIO_INPUT_EVENTS_EXTENDED_SKIPPED: {name} was skipped (flag_not_set) but "
                                        "--with-input-events-extended was enabled (provision the guest with --test-input-events-extended)",
                                        file=sys.stderr,
                                    )
                                else:
                                    print(
                                        f"FAIL: MISSING_VIRTIO_INPUT_EVENTS_EXTENDED: did not observe {name} PASS marker while "
                                        "--with-input-events-extended was enabled",
                                        file=sys.stderr,
                                    )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if result_code is not None:
                                break

                        if need_input_wheel:
                            if saw_virtio_input_wheel_fail:
                                print(
                                    "FAIL: VIRTIO_INPUT_WHEEL_FAILED: virtio-input-wheel test reported FAIL while "
                                    "--with-input-wheel/--with-virtio-input-wheel was enabled",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if not saw_virtio_input_wheel_pass:
                                if saw_virtio_input_wheel_skip:
                                    print(
                                        "FAIL: VIRTIO_INPUT_WHEEL_SKIPPED: virtio-input-wheel test was skipped but "
                                        "--with-input-wheel/--with-virtio-input-wheel was enabled",
                                        file=sys.stderr,
                                    )
                                else:
                                    print(
                                        "FAIL: MISSING_VIRTIO_INPUT_WHEEL: did not observe virtio-input-wheel PASS marker while "
                                        "--with-input-wheel/--with-virtio-input-wheel was enabled",
                                        file=sys.stderr,
                                    )
                                _print_tail(serial_log)
                                result_code = 1
                                break

                        if need_input_tablet_events:
                            if saw_virtio_input_tablet_events_fail:
                                print(
                                    "FAIL: VIRTIO_INPUT_TABLET_EVENTS_FAILED: virtio-input-tablet-events test reported FAIL while --with-input-tablet-events/--with-tablet-events was enabled",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if not saw_virtio_input_tablet_events_pass:
                                if saw_virtio_input_tablet_events_skip:
                                    print(
                                        "FAIL: VIRTIO_INPUT_TABLET_EVENTS_SKIPPED: virtio-input-tablet-events test was skipped (flag_not_set) but "
                                        "--with-input-tablet-events/--with-tablet-events was enabled (provision the guest with --test-input-tablet-events/--test-tablet-events)",
                                        file=sys.stderr,
                                    )
                                else:
                                    print(
                                        "FAIL: MISSING_VIRTIO_INPUT_TABLET_EVENTS: did not observe virtio-input-tablet-events PASS marker while --with-input-tablet-events/--with-tablet-events was enabled",
                                        file=sys.stderr,
                                    )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                        if need_msix_check and not msix_checked:
                            assert qmp_endpoint is not None
                            msg = _require_virtio_msix_check_failure_message(
                                qmp_endpoint,
                                require_virtio_net_msix=bool(args.require_virtio_net_msix),
                                require_virtio_blk_msix=bool(args.require_virtio_blk_msix),
                                require_virtio_snd_msix=bool(args.require_virtio_snd_msix),
                            )
                            msix_checked = True
                            if msg is not None:
                                print(msg, file=sys.stderr)
                                _print_tail(serial_log)
                                result_code = 1
                                break
                        if args.require_virtio_blk_msix:
                            ok, reason = _require_virtio_blk_msix_marker(tail)
                            if not ok:
                                print(
                                    f"FAIL: VIRTIO_BLK_MSIX_REQUIRED: {reason}",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                        if args.require_virtio_snd_msix:
                            ok, reason = _require_virtio_snd_msix_marker(tail)
                            if not ok:
                                print(
                                    f"FAIL: VIRTIO_SND_MSIX_REQUIRED: {reason}",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break

                        if bool(args.require_virtio_input_msix):
                            if virtio_input_msix_marker is None:
                                print(
                                    "FAIL: MISSING_VIRTIO_INPUT_MSIX: did not observe virtio-input-msix marker while "
                                    "--require-virtio-input-msix was enabled (guest selftest too old?)",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            mode = virtio_input_msix_marker.fields.get("mode", "")
                            if mode != "msix":
                                print(
                                    f"FAIL: VIRTIO_INPUT_MSIX_REQUIRED: virtio-input-msix marker did not report mode=msix "
                                    f"while --require-virtio-input-msix was enabled (mode={mode or 'missing'})",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                        irq_fail = _check_virtio_irq_mode_enforcement(
                            tail,
                            irq_diag_markers=irq_diag_markers,
                            require_intx=args.require_intx,
                            require_msi=args.require_msi,
                            devices=irq_mode_devices,
                        )
                        if irq_fail is not None:
                            print(irq_fail, file=sys.stderr)
                            _print_tail(serial_log)
                            result_code = 1
                            break
                        print("PASS: AERO_VIRTIO_SELFTEST|RESULT|PASS")
                        result_code = 0
                        break
                    if b"AERO_VIRTIO_SELFTEST|RESULT|FAIL" in tail:
                        print("FAIL: SELFTEST_FAILED: AERO_VIRTIO_SELFTEST|RESULT|FAIL")
                        _print_tail(serial_log)
                        result_code = 1
                        break

                # When requested, inject keyboard/mouse events after the guest has armed the user-mode HID
                # report read loop (virtio-input-events|READY). Inject multiple times on a short interval to
                # reduce flakiness from timing windows (reports may be dropped when no read is pending).
                #
                # When requested, resize the virtio-blk backing device after the guest has armed its polling
                # loop (virtio-blk-resize|READY).
                if (
                    need_blk_resize
                    and virtio_blk_marker_time is not None
                    and not saw_virtio_blk_resize_ready
                    and not saw_virtio_blk_resize_pass
                    and not saw_virtio_blk_resize_fail
                    and not saw_virtio_blk_resize_skip
                    and time.monotonic() - virtio_blk_marker_time > 20.0
                ):
                    print(
                        "FAIL: MISSING_VIRTIO_BLK_RESIZE: did not observe virtio-blk-resize marker after virtio-blk completed while "
                        "--with-blk-resize was enabled (guest selftest too old or missing --test-blk-resize)",
                        file=sys.stderr,
                    )
                    _print_tail(serial_log)
                    result_code = 1
                    break

                if (
                    need_blk_resize
                    and saw_virtio_blk_resize_ready
                    and not saw_virtio_blk_resize_pass
                    and not saw_virtio_blk_resize_fail
                    and not saw_virtio_blk_resize_skip
                    and not blk_resize_requested
                    and qmp_endpoint is not None
                    and blk_resize_old_bytes is not None
                    and blk_resize_new_bytes is not None
                ):
                    blk_resize_requested = True
                    try:
                        qmp_cmd = _try_qmp_virtio_blk_resize(
                            qmp_endpoint, drive_id="drive0", new_bytes=int(blk_resize_new_bytes)
                        )
                        print(
                            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESIZE|REQUEST|"
                            f"old_bytes={int(blk_resize_old_bytes)}|new_bytes={int(blk_resize_new_bytes)}|qmp_cmd={qmp_cmd}"
                        )
                    except Exception as e:
                        reason = _sanitize_marker_value(str(e) or type(e).__name__)
                        print(
                            f"AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESIZE|FAIL|reason={reason}",
                            file=sys.stderr,
                        )
                        print(
                            f"FAIL: QMP_BLK_RESIZE_FAILED: failed to resize virtio-blk device via QMP: {e}",
                            file=sys.stderr,
                        )
                        _print_tail(serial_log)
                        result_code = 1
                        break

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
                        f"{input_events_req_flags_desc} was enabled (guest selftest too old or missing --test-input-events)",
                        file=sys.stderr,
                    )
                    _print_tail(serial_log)
                    result_code = 1
                    break

                if (
                    need_input_media_keys
                    and virtio_input_marker_time is not None
                    and not saw_virtio_input_media_keys_ready
                    and not saw_virtio_input_media_keys_pass
                    and not saw_virtio_input_media_keys_fail
                    and not saw_virtio_input_media_keys_skip
                    and time.monotonic() - virtio_input_marker_time > 20.0
                ):
                    print(
                        "FAIL: MISSING_VIRTIO_INPUT_MEDIA_KEYS: did not observe virtio-input-media-keys marker after virtio-input completed while "
                        "--with-input-media-keys was enabled (guest selftest too old or missing --test-input-media-keys)",
                        file=sys.stderr,
                    )
                    _print_tail(serial_log)
                    result_code = 1
                    break

                if (
                    need_input_tablet_events
                    and virtio_input_marker_time is not None
                    and not saw_virtio_input_tablet_events_ready
                    and not saw_virtio_input_tablet_events_pass
                    and not saw_virtio_input_tablet_events_fail
                    and not saw_virtio_input_tablet_events_skip
                    and time.monotonic() - virtio_input_marker_time > 20.0
                ):
                    print(
                        "FAIL: MISSING_VIRTIO_INPUT_TABLET_EVENTS: did not observe virtio-input-tablet-events marker after virtio-input completed while "
                        "--with-input-tablet-events/--with-tablet-events was enabled (guest selftest too old or missing --test-input-tablet-events/--test-tablet-events)",
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
                    and input_events_inject_attempts < max_input_events_inject_attempts
                    and time.monotonic() >= next_input_events_inject
                ):
                    input_events_inject_attempts += 1
                    next_input_events_inject = time.monotonic() + 0.5
                    try:
                        info = _try_qmp_input_inject_virtio_input_events(
                            qmp_endpoint,
                            with_wheel=need_input_wheel or need_input_events_extended,
                            extended=need_input_events_extended,
                        )
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

                if (
                    need_input_media_keys
                    and saw_virtio_input_media_keys_ready
                    and not saw_virtio_input_media_keys_pass
                    and not saw_virtio_input_media_keys_fail
                    and not saw_virtio_input_media_keys_skip
                    and qmp_endpoint is not None
                    and input_media_keys_inject_attempts < 20
                    and time.monotonic() >= next_input_media_keys_inject
                ):
                    input_media_keys_inject_attempts += 1
                    next_input_media_keys_inject = time.monotonic() + 0.5
                    try:
                        info = _try_qmp_input_inject_virtio_input_media_keys(qmp_endpoint, qcode="volumeup")
                        kbd_mode = "broadcast" if info.keyboard_device is None else "device"
                        print(
                            f"AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_MEDIA_KEYS_INJECT|PASS|attempt={input_media_keys_inject_attempts}|"
                            f"kbd_mode={kbd_mode}"
                        )
                    except Exception as e:
                        reason = _sanitize_marker_value(str(e) or type(e).__name__)
                        print(
                            f"AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_MEDIA_KEYS_INJECT|FAIL|attempt={input_media_keys_inject_attempts}|reason={reason}",
                            file=sys.stderr,
                        )
                        print(
                            f"FAIL: QMP_MEDIA_KEYS_UNSUPPORTED: failed to inject virtio-input media key events via QMP: {e}",
                            file=sys.stderr,
                        )
                        _print_tail(serial_log)
                        result_code = 1
                        break

                if (
                    need_input_tablet_events
                    and saw_virtio_input_tablet_events_ready
                    and not saw_virtio_input_tablet_events_pass
                    and not saw_virtio_input_tablet_events_fail
                    and not saw_virtio_input_tablet_events_skip
                    and qmp_endpoint is not None
                    and input_tablet_events_inject_attempts < 20
                    and time.monotonic() >= next_input_tablet_events_inject
                ):
                    input_tablet_events_inject_attempts += 1
                    next_input_tablet_events_inject = time.monotonic() + 0.5
                    try:
                        info = _try_qmp_input_inject_virtio_input_tablet_events(qmp_endpoint)
                        tablet_mode = "broadcast" if info.tablet_device is None else "device"
                        print(
                            f"AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_TABLET_EVENTS_INJECT|PASS|attempt={input_tablet_events_inject_attempts}|"
                            f"tablet_mode={tablet_mode}"
                        )
                    except Exception as e:
                        reason = _sanitize_marker_value(str(e) or type(e).__name__)
                        print(
                            f"AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_TABLET_EVENTS_INJECT|FAIL|attempt={input_tablet_events_inject_attempts}|reason={reason}",
                            file=sys.stderr,
                        )
                        print(
                            f"FAIL: QMP_INPUT_TABLET_INJECT_FAILED: failed to inject virtio-input tablet events via QMP: {e}",
                            file=sys.stderr,
                        )
                        _print_tail(serial_log)
                        result_code = 1
                        break

                if proc.poll() is not None:
                    # One last read after exit in case QEMU shut down immediately after writing the marker.
                    chunk2, pos = _read_new_bytes(serial_log, pos)
                    if chunk2:
                        irq_diag_carry = _update_virtio_irq_markers_from_chunk(
                            irq_diag_markers, chunk2, carry=irq_diag_carry
                        )
                        virtio_blk_marker_line, virtio_blk_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_blk_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|",
                            carry=virtio_blk_marker_carry,
                        )
                        tail += chunk2
                        if len(tail) > 131072:
                            tail = tail[-131072:]
                        if virtio_input_msix_marker is None or b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|" in tail:
                            marker = _parse_virtio_input_msix_marker(tail)
                            if marker is not None:
                                virtio_input_msix_marker = marker
                        if not saw_virtio_blk_pass and b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS" in tail:
                            saw_virtio_blk_pass = True
                            if virtio_blk_marker_time is None:
                                virtio_blk_marker_time = time.monotonic()
                        if not saw_virtio_blk_fail and b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|FAIL" in tail:
                            saw_virtio_blk_fail = True
                            if virtio_blk_marker_time is None:
                                virtio_blk_marker_time = time.monotonic()

                        if not saw_virtio_blk_resize_ready:
                            ready = _try_extract_virtio_blk_resize_ready(tail)
                            if ready is not None:
                                saw_virtio_blk_resize_ready = True
                                blk_resize_old_bytes = int(ready.old_bytes)
                                if need_blk_resize:
                                    delta_bytes = int(args.blk_resize_delta_mib) * 1024 * 1024
                                    blk_resize_new_bytes = _virtio_blk_resize_compute_new_bytes(
                                        blk_resize_old_bytes, delta_bytes
                                    )
                        if (
                            not saw_virtio_blk_resize_pass
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|PASS" in tail
                        ):
                            saw_virtio_blk_resize_pass = True
                        if (
                            not saw_virtio_blk_resize_fail
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|FAIL" in tail
                        ):
                            saw_virtio_blk_resize_fail = True
                        if (
                            not saw_virtio_blk_resize_skip
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|SKIP" in tail
                        ):
                            saw_virtio_blk_resize_skip = True
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
                        if (
                            not saw_virtio_input_media_keys_ready
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|READY" in tail
                        ):
                            saw_virtio_input_media_keys_ready = True
                        if (
                            not saw_virtio_input_media_keys_pass
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|PASS" in tail
                        ):
                            saw_virtio_input_media_keys_pass = True
                        if (
                            not saw_virtio_input_media_keys_fail
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|FAIL" in tail
                        ):
                            saw_virtio_input_media_keys_fail = True
                        if (
                            not saw_virtio_input_media_keys_skip
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|SKIP" in tail
                        ):
                            saw_virtio_input_media_keys_skip = True
                        if (
                            not saw_virtio_input_wheel_pass
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|PASS" in tail
                        ):
                            saw_virtio_input_wheel_pass = True
                        if (
                            not saw_virtio_input_wheel_fail
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|FAIL" in tail
                        ):
                            saw_virtio_input_wheel_fail = True
                        if (
                            not saw_virtio_input_wheel_skip
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|SKIP" in tail
                        ):
                            saw_virtio_input_wheel_skip = True

                        if (
                            not saw_virtio_input_events_modifiers_pass
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-modifiers|PASS" in tail
                        ):
                            saw_virtio_input_events_modifiers_pass = True
                        if (
                            not saw_virtio_input_events_modifiers_fail
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-modifiers|FAIL" in tail
                        ):
                            saw_virtio_input_events_modifiers_fail = True
                        if (
                            not saw_virtio_input_events_modifiers_skip
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-modifiers|SKIP" in tail
                        ):
                            saw_virtio_input_events_modifiers_skip = True
                        if (
                            not saw_virtio_input_events_buttons_pass
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-buttons|PASS" in tail
                        ):
                            saw_virtio_input_events_buttons_pass = True
                        if (
                            not saw_virtio_input_events_buttons_fail
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-buttons|FAIL" in tail
                        ):
                            saw_virtio_input_events_buttons_fail = True
                        if (
                            not saw_virtio_input_events_buttons_skip
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-buttons|SKIP" in tail
                        ):
                            saw_virtio_input_events_buttons_skip = True
                        if (
                            not saw_virtio_input_events_wheel_pass
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-wheel|PASS" in tail
                        ):
                            saw_virtio_input_events_wheel_pass = True
                        if (
                            not saw_virtio_input_events_wheel_fail
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-wheel|FAIL" in tail
                        ):
                            saw_virtio_input_events_wheel_fail = True
                        if (
                            not saw_virtio_input_events_wheel_skip
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-wheel|SKIP" in tail
                        ):
                            saw_virtio_input_events_wheel_skip = True

                        if (
                            not saw_virtio_input_tablet_events_ready
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|READY" in tail
                        ):
                            saw_virtio_input_tablet_events_ready = True
                        if (
                            not saw_virtio_input_tablet_events_pass
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|PASS" in tail
                        ):
                            saw_virtio_input_tablet_events_pass = True
                        if (
                            not saw_virtio_input_tablet_events_fail
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|FAIL" in tail
                        ):
                            saw_virtio_input_tablet_events_fail = True
                        if (
                            not saw_virtio_input_tablet_events_skip
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|SKIP" in tail
                        ):
                            saw_virtio_input_tablet_events_skip = True
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
                        if (
                            not saw_virtio_snd_buffer_limits_pass
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|PASS" in tail
                        ):
                            saw_virtio_snd_buffer_limits_pass = True
                        if (
                            not saw_virtio_snd_buffer_limits_skip
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|SKIP" in tail
                        ):
                            saw_virtio_snd_buffer_limits_skip = True
                        if (
                            not saw_virtio_snd_buffer_limits_fail
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|FAIL" in tail
                        ):
                            saw_virtio_snd_buffer_limits_fail = True
                        if not saw_virtio_net_pass and b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS" in tail:
                            saw_virtio_net_pass = True
                        if not saw_virtio_net_fail and b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|FAIL" in tail:
                            saw_virtio_net_fail = True
                        if (
                            not saw_virtio_net_udp_pass
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|PASS" in tail
                        ):
                            saw_virtio_net_udp_pass = True
                        if (
                            not saw_virtio_net_udp_fail
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|FAIL" in tail
                        ):
                            saw_virtio_net_udp_fail = True
                        if (
                            not saw_virtio_net_udp_skip
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|SKIP" in tail
                        ):
                            saw_virtio_net_udp_skip = True
                        if b"AERO_VIRTIO_SELFTEST|RESULT|PASS" in tail:
                            if require_per_test_markers:
                                if saw_virtio_blk_fail:
                                    print(
                                        "FAIL: VIRTIO_BLK_FAILED: selftest RESULT=PASS but virtio-blk test reported FAIL",
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
                                if need_blk_resize:
                                    if saw_virtio_blk_resize_fail:
                                        print(
                                            "FAIL: VIRTIO_BLK_RESIZE_FAILED: selftest RESULT=PASS but virtio-blk-resize test reported FAIL",
                                            file=sys.stderr,
                                        )
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                                    if not saw_virtio_blk_resize_pass:
                                        if saw_virtio_blk_resize_skip:
                                            print(
                                                "FAIL: VIRTIO_BLK_RESIZE_SKIPPED: virtio-blk-resize test was skipped (flag_not_set) but "
                                                "--with-blk-resize was enabled (provision the guest with --test-blk-resize)",
                                                file=sys.stderr,
                                            )
                                        else:
                                            print(
                                                "FAIL: MISSING_VIRTIO_BLK_RESIZE: selftest RESULT=PASS but did not emit virtio-blk-resize test marker "
                                                "while --with-blk-resize was enabled",
                                                file=sys.stderr,
                                            )
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                                if saw_virtio_input_fail:
                                    print(
                                        "FAIL: VIRTIO_INPUT_FAILED: selftest RESULT=PASS but virtio-input test reported FAIL",
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
                                        "FAIL: VIRTIO_SND_FAILED: selftest RESULT=PASS but virtio-snd test reported FAIL",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break

                                if args.enable_virtio_snd:
                                    if not saw_virtio_snd_pass:
                                        msg = "FAIL: MISSING_VIRTIO_SND: virtio-snd test did not PASS while --with-virtio-snd was enabled"
                                        if saw_virtio_snd_skip:
                                            msg = _virtio_snd_skip_failure_message(tail)
                                        print(msg, file=sys.stderr)
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                                    if saw_virtio_snd_capture_fail:
                                        print(
                                            "FAIL: VIRTIO_SND_CAPTURE_FAILED: selftest RESULT=PASS but virtio-snd-capture test reported FAIL",
                                            file=sys.stderr,
                                        )
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                                    if not saw_virtio_snd_capture_pass:
                                        msg = (
                                            "FAIL: MISSING_VIRTIO_SND_CAPTURE: virtio-snd capture test did not PASS while --with-virtio-snd was enabled"
                                        )
                                        if saw_virtio_snd_capture_skip:
                                            msg = _virtio_snd_capture_skip_failure_message(tail)
                                        print(msg, file=sys.stderr)
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                                    if saw_virtio_snd_duplex_fail:
                                        print(
                                            "FAIL: VIRTIO_SND_DUPLEX_FAILED: selftest RESULT=PASS but virtio-snd-duplex test reported FAIL",
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

                                    if args.with_snd_buffer_limits:
                                        msg = _virtio_snd_buffer_limits_required_failure_message(tail)
                                        if msg is not None:
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
                                            "FAIL: VIRTIO_SND_CAPTURE_FAILED: selftest RESULT=PASS but virtio-snd-capture test reported FAIL",
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
                                            "FAIL: VIRTIO_SND_DUPLEX_FAILED: selftest RESULT=PASS but virtio-snd-duplex test reported FAIL",
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
                                        "FAIL: VIRTIO_NET_FAILED: selftest RESULT=PASS but virtio-net test reported FAIL",
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
                                if not args.disable_udp:
                                    if saw_virtio_net_udp_fail:
                                        print(
                                            "FAIL: VIRTIO_NET_UDP_FAILED: selftest RESULT=PASS but virtio-net-udp test reported FAIL",
                                            file=sys.stderr,
                                        )
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                                    if not saw_virtio_net_udp_pass:
                                        if saw_virtio_net_udp_skip:
                                            print(
                                                "FAIL: VIRTIO_NET_UDP_SKIPPED: virtio-net-udp test was skipped but UDP testing is enabled (update/provision the guest selftest)",
                                                file=sys.stderr,
                                            )
                                        else:
                                            print(
                                                "FAIL: MISSING_VIRTIO_NET_UDP: selftest RESULT=PASS but did not emit virtio-net-udp test marker",
                                                file=sys.stderr,
                                            )
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                            elif args.enable_virtio_snd:
                                if saw_virtio_snd_fail:
                                    print(
                                        "FAIL: VIRTIO_SND_FAILED: selftest RESULT=PASS but virtio-snd test reported FAIL",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break

                                if not saw_virtio_snd_pass:
                                    msg = "FAIL: MISSING_VIRTIO_SND: virtio-snd test did not PASS while --with-virtio-snd was enabled"
                                    if saw_virtio_snd_skip:
                                        msg = _virtio_snd_skip_failure_message(tail)
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if saw_virtio_snd_capture_fail:
                                    print(
                                        "FAIL: VIRTIO_SND_CAPTURE_FAILED: selftest RESULT=PASS but virtio-snd-capture test reported FAIL",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_snd_capture_pass:
                                    msg = "FAIL: MISSING_VIRTIO_SND_CAPTURE: virtio-snd capture test did not PASS while --with-virtio-snd was enabled"
                                    if saw_virtio_snd_capture_skip:
                                        msg = _virtio_snd_capture_skip_failure_message(tail)
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if saw_virtio_snd_duplex_fail:
                                    print(
                                        "FAIL: VIRTIO_SND_DUPLEX_FAILED: selftest RESULT=PASS but virtio-snd-duplex test reported FAIL",
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

                                if args.with_snd_buffer_limits:
                                    msg = _virtio_snd_buffer_limits_required_failure_message(tail)
                                    if msg is not None:
                                        print(msg, file=sys.stderr)
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                            if need_input_events:
                                if saw_virtio_input_events_fail:
                                    print(
                                        "FAIL: VIRTIO_INPUT_EVENTS_FAILED: virtio-input-events test reported FAIL while "
                                        f"{input_events_req_flags_desc} was enabled",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_input_events_pass:
                                    if saw_virtio_input_events_skip:
                                        print(
                                            "FAIL: VIRTIO_INPUT_EVENTS_SKIPPED: virtio-input-events test was skipped (flag_not_set) but "
                                            f"{input_events_req_flags_desc} was enabled (provision the guest with --test-input-events)",
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(
                                            "FAIL: MISSING_VIRTIO_INPUT_EVENTS: did not observe virtio-input-events PASS marker while "
                                            f"{input_events_req_flags_desc} was enabled",
                                            file=sys.stderr,
                                        )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if need_input_media_keys:
                                if saw_virtio_input_media_keys_fail:
                                    print(
                                        "FAIL: VIRTIO_INPUT_MEDIA_KEYS_FAILED: virtio-input-media-keys test reported FAIL while --with-input-media-keys was enabled",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_input_media_keys_pass:
                                    if saw_virtio_input_media_keys_skip:
                                        print(
                                            "FAIL: VIRTIO_INPUT_MEDIA_KEYS_SKIPPED: virtio-input-media-keys test was skipped (flag_not_set) but "
                                            "--with-input-media-keys was enabled (provision the guest with --test-input-media-keys)",
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(
                                            "FAIL: MISSING_VIRTIO_INPUT_MEDIA_KEYS: did not observe virtio-input-media-keys PASS marker while "
                                            "--with-input-media-keys was enabled",
                                            file=sys.stderr,
                                        )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break

                            if need_blk_resize:
                                if saw_virtio_blk_resize_fail:
                                    print(
                                        "FAIL: VIRTIO_BLK_RESIZE_FAILED: virtio-blk-resize test reported FAIL while --with-blk-resize was enabled",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_blk_resize_pass:
                                    if saw_virtio_blk_resize_skip:
                                        print(
                                            "FAIL: VIRTIO_BLK_RESIZE_SKIPPED: virtio-blk-resize test was skipped (flag_not_set) but "
                                            "--with-blk-resize was enabled (provision the guest with --test-blk-resize)",
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(
                                            "FAIL: MISSING_VIRTIO_BLK_RESIZE: did not observe virtio-blk-resize PASS marker while --with-blk-resize was enabled",
                                            file=sys.stderr,
                                        )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if need_input_events_extended:
                                if (
                                    saw_virtio_input_events_modifiers_fail
                                    or saw_virtio_input_events_buttons_fail
                                    or saw_virtio_input_events_wheel_fail
                                ):
                                    print(
                                        "FAIL: VIRTIO_INPUT_EVENTS_EXTENDED_FAILED: a virtio-input-events-* marker reported FAIL while "
                                        "--with-input-events-extended was enabled",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                for name, saw_pass, saw_skip in (
                                    (
                                        "virtio-input-events-modifiers",
                                        saw_virtio_input_events_modifiers_pass,
                                        saw_virtio_input_events_modifiers_skip,
                                    ),
                                    (
                                        "virtio-input-events-buttons",
                                        saw_virtio_input_events_buttons_pass,
                                        saw_virtio_input_events_buttons_skip,
                                    ),
                                    (
                                        "virtio-input-events-wheel",
                                        saw_virtio_input_events_wheel_pass,
                                        saw_virtio_input_events_wheel_skip,
                                    ),
                                ):
                                    if saw_pass:
                                        continue
                                    if saw_skip:
                                        print(
                                            f"FAIL: VIRTIO_INPUT_EVENTS_EXTENDED_SKIPPED: {name} was skipped (flag_not_set) but "
                                            "--with-input-events-extended was enabled (provision the guest with --test-input-events-extended)",
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(
                                            f"FAIL: MISSING_VIRTIO_INPUT_EVENTS_EXTENDED: did not observe {name} PASS marker while "
                                            "--with-input-events-extended was enabled",
                                            file=sys.stderr,
                                        )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if result_code is not None:
                                    break

                            if need_input_tablet_events:
                                if saw_virtio_input_tablet_events_fail:
                                    print(
                                        "FAIL: VIRTIO_INPUT_TABLET_EVENTS_FAILED: virtio-input-tablet-events test reported FAIL while --with-input-tablet-events/--with-tablet-events was enabled",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_input_tablet_events_pass:
                                    if saw_virtio_input_tablet_events_skip:
                                        print(
                                            "FAIL: VIRTIO_INPUT_TABLET_EVENTS_SKIPPED: virtio-input-tablet-events test was skipped (flag_not_set) but "
                                            "--with-input-tablet-events/--with-tablet-events was enabled (provision the guest with --test-input-tablet-events/--test-tablet-events)",
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(
                                            "FAIL: MISSING_VIRTIO_INPUT_TABLET_EVENTS: did not observe virtio-input-tablet-events PASS marker while "
                                            "--with-input-tablet-events/--with-tablet-events was enabled",
                                            file=sys.stderr,
                                        )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if need_input_wheel:
                                if saw_virtio_input_wheel_fail:
                                    print(
                                        "FAIL: VIRTIO_INPUT_WHEEL_FAILED: virtio-input-wheel test reported FAIL while "
                                        "--with-input-wheel/--with-virtio-input-wheel was enabled",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_input_wheel_pass:
                                    if saw_virtio_input_wheel_skip:
                                        print(
                                            "FAIL: VIRTIO_INPUT_WHEEL_SKIPPED: virtio-input-wheel test was skipped but "
                                            "--with-input-wheel/--with-virtio-input-wheel was enabled",
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(
                                            "FAIL: MISSING_VIRTIO_INPUT_WHEEL: did not observe virtio-input-wheel PASS marker while "
                                            "--with-input-wheel/--with-virtio-input-wheel was enabled",
                                            file=sys.stderr,
                                        )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if need_msix_check and not msix_checked:
                                assert qmp_endpoint is not None
                                msg = _require_virtio_msix_check_failure_message(
                                    qmp_endpoint,
                                    require_virtio_net_msix=bool(args.require_virtio_net_msix),
                                    require_virtio_blk_msix=bool(args.require_virtio_blk_msix),
                                    require_virtio_snd_msix=bool(args.require_virtio_snd_msix),
                                )
                                msix_checked = True
                                if msg is not None:
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if args.require_virtio_blk_msix:
                                ok, reason = _require_virtio_blk_msix_marker(tail)
                                if not ok:
                                    print(
                                        f"FAIL: VIRTIO_BLK_MSIX_REQUIRED: {reason}",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if args.require_virtio_snd_msix:
                                ok, reason = _require_virtio_snd_msix_marker(tail)
                                if not ok:
                                    print(
                                        f"FAIL: VIRTIO_SND_MSIX_REQUIRED: {reason}",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break

                            if bool(args.require_virtio_input_msix):
                                if virtio_input_msix_marker is None:
                                    print(
                                        "FAIL: MISSING_VIRTIO_INPUT_MSIX: did not observe virtio-input-msix marker while "
                                        "--require-virtio-input-msix was enabled (guest selftest too old?)",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                mode = virtio_input_msix_marker.fields.get("mode", "")
                                if mode != "msix":
                                    print(
                                        f"FAIL: VIRTIO_INPUT_MSIX_REQUIRED: virtio-input-msix marker did not report mode=msix "
                                        f"while --require-virtio-input-msix was enabled (mode={mode or 'missing'})",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            irq_fail = _check_virtio_irq_mode_enforcement(
                                tail,
                                irq_diag_markers=irq_diag_markers,
                                require_intx=args.require_intx,
                                require_msi=args.require_msi,
                                devices=irq_mode_devices,
                            )
                            if irq_fail is not None:
                                print(irq_fail, file=sys.stderr)
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            print("PASS: AERO_VIRTIO_SELFTEST|RESULT|PASS")
                            result_code = 0
                            break
                        if b"AERO_VIRTIO_SELFTEST|RESULT|FAIL" in tail:
                            print("FAIL: SELFTEST_FAILED: AERO_VIRTIO_SELFTEST|RESULT|FAIL")
                            _print_tail(serial_log)
                            result_code = 1
                            break

                    print(f"FAIL: QEMU_EXITED: QEMU exited before selftest result marker (exit code: {proc.returncode})")
                    _print_tail(serial_log)
                    _print_qemu_stderr_tail(qemu_stderr_log)
                    result_code = 3
                    break

                time.sleep(0.25)

            if result_code is None:
                print("FAIL: TIMEOUT: timed out waiting for AERO_VIRTIO_SELFTEST result marker")
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
            if udp_server is not None:
                udp_server.close()
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

        # Flush any pending non-newline-terminated IRQ diagnostic line so it can still be surfaced.
        if irq_diag_carry:
            for dev, fields in _parse_virtio_irq_markers(irq_diag_carry).items():
                irq_diag_markers[dev] = fields
        # Flush any pending non-newline-terminated virtio-blk TEST marker (rare, but keep behavior
        # deterministic if the serial log ends without a trailing newline).
        if virtio_blk_marker_carry:
            raw = virtio_blk_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|"):
                try:
                    virtio_blk_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass

        _emit_virtio_blk_irq_host_marker(tail, blk_test_line=virtio_blk_marker_line, irq_diag_markers=irq_diag_markers)
        _emit_virtio_blk_io_host_marker(tail, blk_test_line=virtio_blk_marker_line)
        _emit_virtio_net_large_host_marker(tail)
        _emit_virtio_net_diag_host_marker(tail)
        _emit_virtio_net_irq_host_marker(tail)
        _emit_virtio_snd_irq_host_marker(tail)
        _emit_virtio_input_irq_host_marker(tail)
        _emit_virtio_irq_host_markers(tail, markers=irq_diag_markers)
        _emit_virtio_snd_playback_host_marker(tail)
        _emit_virtio_snd_capture_host_marker(tail)
        _emit_virtio_snd_format_host_marker(tail)
        _emit_virtio_snd_duplex_host_marker(tail)
        _emit_virtio_snd_buffer_limits_host_marker(tail)
        _emit_virtio_snd_eventq_host_marker(tail)

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
                if bits_per_sample == 32:
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
                elif bits_per_sample == 64:
                    for i in range(0, len(mv), 8):
                        (raw_d,) = struct.unpack_from("<d", mv, i)
                        if not math.isfinite(raw_d):
                            continue
                        v = float(raw_d) * 32767.0
                        av = -v if v < 0.0 else v
                        if av > peak_f:
                            peak_f = av
                        sum_sq += v * v
                        count += 1
                else:
                    raise ValueError(f"unsupported float bits_per_sample {bits_per_sample}")
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


@dataclass(frozen=True)
class _VirtioInputMsixMarker:
    status: str
    fields: dict[str, str]
    line: str


def _parse_virtio_input_msix_marker(tail: bytes) -> Optional[_VirtioInputMsixMarker]:
    """
    Parse the guest marker emitted by the selftest for virtio-input interrupt diagnostics.

    Example:
      AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|PASS|mode=msix|messages=3|...
    """
    marker_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|")
    if marker_line is None:
        return None

    parts = marker_line.split("|")
    status = parts[3] if len(parts) >= 4 else ""
    fields = _parse_marker_kv_fields(marker_line)
    return _VirtioInputMsixMarker(status=status, fields=fields, line=marker_line)


_VIRTIO_IRQ_MARKER_RE = re.compile(r"^virtio-(?P<dev>.+)-irq\|(?P<level>INFO|WARN)(?:\|(?P<rest>.*))?$")
_VIRTIO_NET_DIAG_MARKER_RE = re.compile(r"^virtio-net-diag\|(?P<level>INFO|WARN)(?:\|(?P<rest>.*))?$")


def _try_parse_virtio_irq_marker_line(line: str) -> Optional[tuple[str, dict[str, str]]]:
    """
    Parse a single `virtio-<dev>-irq|INFO/WARN|...` line.

    Returns `(device_key, fields)` on success (device_key includes the `virtio-` prefix), otherwise `None`.
    """
    m = _VIRTIO_IRQ_MARKER_RE.match(line.strip())
    if not m:
        return None
    dev = m.group("dev")
    level = m.group("level")
    rest = m.group("rest") or ""

    fields: dict[str, str] = {"level": level}
    extra_parts: list[str] = []
    for tok in rest.split("|") if rest else []:
        tok = tok.strip()
        if not tok:
            continue
        if "=" not in tok:
            extra_parts.append(tok)
            continue
        k, v = tok.split("=", 1)
        k = k.strip()
        v = v.strip()
        if not k:
            continue
        fields[k] = v
    if extra_parts:
        # Preserve non key/value tokens in a best-effort field so callers can still surface
        # diagnostics even if the guest marker format changes slightly.
        fields["msg"] = "|".join(extra_parts)

    # Backward compatible: virtio-blk miniport IRQ diagnostics historically used `message_count=<n>`
    # instead of the `messages=<n>` key used by other virtio devices. Normalize so downstream host
    # markers are stable.
    if "message_count" in fields:
        fields.setdefault("messages", fields["message_count"])
        del fields["message_count"]

    return f"virtio-{dev}", fields


def _parse_virtio_irq_markers(tail: bytes) -> dict[str, dict[str, str]]:
    """
    Parse guest IRQ diagnostics markers.

    The guest selftest may emit interrupt mode diagnostics for each virtio device:

      virtio-<dev>-irq|INFO|mode=msix|vectors=...|...
      virtio-<dev>-irq|WARN|mode=intx|reason=...|...

    Returns a mapping from device name (e.g. "virtio-net") to a dict of parsed fields.
    The dict always includes a "level" key ("INFO" or "WARN") and may include additional
    key/value fields (e.g. "mode", "vectors", ...).

    These markers are informational by default and do not affect overall harness PASS/FAIL.
    """
    out: dict[str, dict[str, str]] = {}
    for raw in tail.splitlines():
        raw2 = raw.lstrip()
        if not raw2.startswith(b"virtio-") or b"-irq|" not in raw2:
            continue
        try:
            line = raw2.decode("utf-8", errors="replace").strip()
        except Exception:
            continue
        parsed = _try_parse_virtio_irq_marker_line(line)
        if parsed is None:
            continue
        dev, fields = parsed
        out[dev] = fields
    return out


def _update_virtio_irq_markers_from_chunk(
    markers: dict[str, dict[str, str]], chunk: bytes, *, carry: bytes = b""
) -> bytes:
    """
    Incrementally parse `virtio-<dev>-irq|...` markers from a newly read serial chunk.

    Returns the updated `carry` bytes that represent a potentially incomplete last line
    (i.e. the text after the last `\\n`).
    """
    if not chunk and not carry:
        return b""
    data = carry + chunk
    # Use splitlines(keepends=True) so we correctly handle any of: LF, CRLF, or CR.
    parts = data.splitlines(keepends=True)
    new_carry = b""
    if parts and not parts[-1].endswith((b"\n", b"\r")):
        new_carry = parts.pop()
    # Bound the carry buffer to avoid unbounded growth if the guest prints extremely long
    # lines without newlines. The harness tail buffer is capped at 128 KiB; use the same cap
    # here since we can only reliably parse marker lines within that window anyway.
    if len(new_carry) > 131072:
        new_carry = new_carry[-131072:]

    for raw in parts:
        # Drop the line ending so we can match against the raw marker text.
        raw = raw.rstrip(b"\r\n")
        raw2 = raw.lstrip()
        if not raw2 or not raw2.startswith(b"virtio-") or b"-irq|" not in raw2:
            continue
        try:
            line = raw2.decode("utf-8", errors="replace").strip()
        except Exception:
            continue
        parsed = _try_parse_virtio_irq_marker_line(line)
        if parsed is None:
            continue
        dev, fields = parsed
        markers[dev] = fields
    return new_carry


def _update_last_marker_line_from_chunk(
    last: Optional[str], chunk: bytes, *, prefix: bytes, carry: bytes = b""
) -> tuple[Optional[str], bytes]:
    """
    Incrementally track the last full line that starts with `prefix`.

    Returns `(last_line, carry)` where `carry` is any potentially incomplete last line
    (i.e. the bytes after the last `\\n`).
    """
    if not chunk and not carry:
        return last, b""

    data = carry + chunk
    parts = data.split(b"\n")
    new_carry = parts.pop() if parts else b""

    for raw in parts:
        if raw.endswith(b"\r"):
            raw = raw[:-1]
        raw2 = raw.lstrip()
        if not raw2.startswith(prefix):
            continue
        try:
            last = raw2.decode("utf-8", errors="replace").strip()
        except Exception:
            continue

    return last, new_carry


def _normalize_irq_mode(mode: str) -> str:
    """
    Normalize an IRQ mode string from guest markers.

    The guest selftest typically reports one of:
      - intx
      - msi
      - msix
    """
    m = (mode or "").strip().lower()
    m = m.replace("_", "-")
    if m in ("msi-x", "msix"):
        return "msix"
    if m == "msi":
        return "msi"
    if m == "intx":
        return "intx"
    return m


def _irq_mode_family(mode: str) -> Optional[str]:
    """
    Return "intx" or "msi" for known modes, else None.

    MSI-X is treated as part of the MSI family for the purpose of --require-msi.
    """
    m = _normalize_irq_mode(mode)
    if m == "intx":
        return "intx"
    if m in ("msi", "msix"):
        return "msi"
    return None


def _try_extract_irq_mode_from_aero_marker_line(marker_line: str) -> Optional[str]:
    fields = _parse_marker_kv_fields(marker_line)
    if not fields:
        return None
    # Prefer the explicit irq_mode key, but accept mode= when it clearly names an IRQ mode.
    if "irq_mode" in fields:
        return fields["irq_mode"]
    if "mode" in fields:
        mode = fields["mode"]
        if _irq_mode_family(mode) is not None:
            return mode
    if "interrupt_mode" in fields:
        mode = fields["interrupt_mode"]
        if _irq_mode_family(mode) is not None:
            return mode
    return None


def _extract_virtio_irq_mode(
    tail: bytes, *, irq_diag_markers: dict[str, dict[str, str]], device: str
) -> Optional[str]:
    """
    Extract the guest-reported IRQ mode for a virtio device.

    Preference order:
    - standalone guest IRQ diagnostics (`virtio-<dev>-irq|...|mode=...`) captured in `irq_diag_markers`
    - virtio-blk: dedicated `virtio-blk-irq` AERO marker lines when present
    - fall back to `AERO_VIRTIO_SELFTEST|TEST|<device>|...|irq_mode=...`
    """
    if device in irq_diag_markers:
        fields = irq_diag_markers[device]
        if "mode" in fields and fields["mode"]:
            return fields["mode"]

    if device == "virtio-blk":
        for prefix in (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-irq|",
            b"AERO_VIRTIO_SELFTEST|MARKER|virtio-blk-irq|",
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|IRQ|",
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|INFO|",
        ):
            line = _try_extract_last_marker_line(tail, prefix)
            if line is None:
                continue
            mode = _try_extract_irq_mode_from_aero_marker_line(line)
            if mode is not None:
                return mode

    line = _try_extract_last_marker_line(tail, f"AERO_VIRTIO_SELFTEST|TEST|{device}|".encode("utf-8"))
    if line is None:
        return None
    return _try_extract_irq_mode_from_aero_marker_line(line)


def _check_virtio_irq_mode_enforcement(
    tail: bytes,
    *,
    irq_diag_markers: Optional[dict[str, dict[str, str]]] = None,
    require_intx: bool = False,
    require_msi: bool = False,
    devices: Optional[list[str]] = None,
) -> Optional[str]:
    """
    Enforce virtio IRQ mode requirements.

    Returns a deterministic failure message (starting with `FAIL:`) on mismatch, otherwise None.
    """
    if not require_intx and not require_msi:
        return None
    if require_intx and require_msi:
        raise AssertionError("require_intx and require_msi are mutually exclusive")

    expected = "intx" if require_intx else "msi"
    if devices is None:
        devices = ["virtio-blk", "virtio-net", "virtio-input", "virtio-snd"]

    diag = irq_diag_markers if irq_diag_markers is not None else _parse_virtio_irq_markers(tail)

    for dev in devices:
        got_raw = _extract_virtio_irq_mode(tail, irq_diag_markers=diag, device=dev)
        if got_raw is None or not str(got_raw).strip():
            return f"FAIL: IRQ_MODE_MISMATCH: {dev} expected={expected} got=unknown"
        got = _normalize_irq_mode(str(got_raw))
        got_family = _irq_mode_family(got)
        if expected == "intx":
            if got_family != "intx":
                return f"FAIL: IRQ_MODE_MISMATCH: {dev} expected=intx got={got}"
        else:
            if got_family != "msi":
                return f"FAIL: IRQ_MODE_MISMATCH: {dev} expected=msi got={got}"
    return None


def _emit_virtio_irq_host_markers(
    tail: bytes, *, markers: Optional[dict[str, dict[str, str]]] = None
) -> None:
    """
    Best-effort: emit host-side markers mirroring the guest `virtio-<dev>-irq|...` diagnostics.

    This does not affect harness PASS/FAIL; it's only for log scraping/diagnostics.
    """
    if markers is None:
        markers = _parse_virtio_irq_markers(tail)
    if not markers:
        return

    for dev, fields in sorted(markers.items()):
        level = fields.get("level", "INFO")
        dev_short = dev
        if dev_short.startswith("virtio-"):
            dev_short = dev_short[len("virtio-") :]
        # Avoid colliding with the stable PASS/FAIL/INFO `VIRTIO_*_IRQ` host markers that mirror
        # `irq_*` fields from `AERO_VIRTIO_SELFTEST|TEST|...` lines.
        marker_name = "VIRTIO_" + dev_short.upper().replace("-", "_") + "_IRQ_DIAG"

        parts = [f"AERO_VIRTIO_WIN7_HOST|{marker_name}|{level}"]
        for k in sorted(fields.keys()):
            if k == "level":
                continue
            parts.append(f"{k}={_sanitize_marker_value(fields[k])}")
        print("|".join(parts))


def _try_extract_marker_status(marker_line: str) -> Optional[str]:
    """
    Return the marker status token (PASS/FAIL/SKIP) if present.

    Marker lines are `|` separated and typically include an explicit token:
      AERO_VIRTIO_SELFTEST|TEST|virtio-snd|PASS|...
    """
    toks = marker_line.split("|")
    if "FAIL" in toks:
        return "FAIL"
    if "PASS" in toks:
        return "PASS"
    if "SKIP" in toks:
        return "SKIP"
    return None

@dataclass(frozen=True)
class _VirtioBlkResizeReadyInfo:
    disk: Optional[int]
    old_bytes: int


def _try_extract_virtio_blk_resize_ready(tail: bytes) -> Optional[_VirtioBlkResizeReadyInfo]:
    """
    Extract the guest virtio-blk-resize READY marker from the serial tail.

    Marker format:
      AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|READY|disk=<N>|old_bytes=<u64>
    """
    marker_line = _try_extract_last_marker_line(
        tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|READY"
    )
    if marker_line is None:
        return None
    fields = _parse_marker_kv_fields(marker_line)
    if "old_bytes" not in fields:
        return None
    try:
        old_bytes = int(fields["old_bytes"], 0)
    except Exception:
        return None

    disk: Optional[int] = None
    if "disk" in fields:
        try:
            disk = int(fields["disk"], 0)
        except Exception:
            disk = None

    return _VirtioBlkResizeReadyInfo(disk=disk, old_bytes=old_bytes)


def _virtio_blk_resize_compute_new_bytes(old_bytes: int, delta_bytes: int) -> int:
    """
    Compute the new (grown) disk size for the virtio-blk runtime resize test.

    Ensures:
    - grow-only (new_bytes > old_bytes)
    - 512-byte alignment (QEMU typically requires sector alignment)
    """
    old = int(old_bytes)
    delta = int(delta_bytes)
    if old < 0:
        raise ValueError("old_bytes must be >= 0")
    if delta <= 0:
        raise ValueError("delta_bytes must be > 0")

    new = old + delta
    # Align up to 512 bytes.
    align = 512
    if new % align != 0:
        new = ((new + align - 1) // align) * align
    if new <= old:
        new = old + align
    return new


def _emit_virtio_net_large_host_marker(tail: bytes) -> None:
    """
    Best-effort: emit a host-side marker describing the guest's virtio-net large transfer metrics.

    This does not affect harness PASS/FAIL; it's only for log scraping/diagnostics.
    """
    marker_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|")
    if marker_line is None:
        return

    fields = _parse_marker_kv_fields(marker_line)
    if not any(
        k in fields
        for k in (
            "large_ok",
            "large_bytes",
            "large_mbps",
            "large_fnv1a64",
            "upload_ok",
            "upload_bytes",
            "upload_mbps",
            "msi",
            "msi_messages",
        )
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
    for k in (
        "large_ok",
        "large_bytes",
        "large_fnv1a64",
        "large_mbps",
        "upload_ok",
        "upload_bytes",
        "upload_mbps",
        "msi",
        "msi_messages",
    ):
        if k in fields:
            parts.append(f"{k}={_sanitize_marker_value(fields[k])}")
    print("|".join(parts))


def _emit_virtio_net_diag_host_marker(tail: bytes) -> None:
    """
    Best-effort: emit a host-side marker mirroring the guest's `virtio-net-diag|...` diagnostics.

    This does not affect harness PASS/FAIL; it's only for log scraping/diagnostics.
    """
    marker_line = _try_extract_last_marker_line(tail, b"virtio-net-diag|")
    if marker_line is None:
        return

    m = _VIRTIO_NET_DIAG_MARKER_RE.match(marker_line)
    if not m:
        return

    level = m.group("level")
    rest = m.group("rest") or ""

    fields: dict[str, str] = {}
    extra_parts: list[str] = []
    for tok in rest.split("|") if rest else []:
        tok = tok.strip()
        if not tok:
            continue
        if "=" not in tok:
            extra_parts.append(tok)
            continue
        k, v = tok.split("=", 1)
        k = k.strip()
        v = v.strip()
        if not k:
            continue
        fields[k] = v
    if extra_parts:
        fields["msg"] = "|".join(extra_parts)

    parts = [f"AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_DIAG|{level}"]

    ordered = (
        "reason",
        "host_features",
        "guest_features",
        "irq_mode",
        "irq_message_count",
        "msix_config_vector",
        "msix_rx_vector",
        "msix_tx_vector",
        "rx_queue_size",
        "tx_queue_size",
        "rx_avail_idx",
        "rx_used_idx",
        "tx_avail_idx",
        "tx_used_idx",
        "rx_vq_error_flags",
        "tx_vq_error_flags",
        "tx_csum_v4",
        "tx_csum_v6",
        "tx_tso_v4",
        "tx_tso_v6",
        "stat_tx_err",
        "stat_rx_err",
        "stat_rx_no_buf",
        "msg",
    )
    for k in ordered:
        if k in fields:
            parts.append(f"{k}={_sanitize_marker_value(fields[k])}")

    extra = sorted(k for k in fields if k not in ordered)
    for k in extra:
        parts.append(f"{k}={_sanitize_marker_value(fields[k])}")

    print("|".join(parts))


def _emit_virtio_blk_irq_host_marker(
    tail: bytes,
    *,
    blk_test_line: Optional[str] = None,
    irq_diag_markers: Optional[dict[str, dict[str, str]]] = None,
) -> None:
    """
    Best-effort: emit a host-side marker describing the guest's virtio-blk interrupt mode/vectors.

    This does not affect harness PASS/FAIL; it's only for log scraping/diagnostics.
    """
    # Collect IRQ fields from (a) the virtio-blk per-test marker (IOCTL-derived fields) and/or
    # (b) standalone diagnostics:
    #     - `virtio-blk-miniport-irq|...` (miniport IOCTL-derived mode/message_count/MSI-X vectors)
    #     - `virtio-blk-irq|...` (cfgmgr32 resource enumeration / Windows-assigned IRQ resources)
    #
    # Prefer the per-test marker when present, but fill in missing fields from the standalone
    # diagnostics so the host marker is still produced for older selftest binaries.
    if blk_test_line is None:
        blk_test_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|")
    blk_test_fields = _parse_marker_kv_fields(blk_test_line) if blk_test_line is not None else {}

    if irq_diag_markers is None:
        irq_diag_markers = _parse_virtio_irq_markers(tail)
    # The guest virtio-blk selftest historically used `virtio-blk-irq|...` for miniport
    # diagnostics (IOCTL-derived IRQ mode + MSI/MSI-X vectors). It was later renamed to
    # `virtio-blk-miniport-irq|...` so `virtio-blk-irq|...` can be reserved for
    # cfgmgr32/Windows-assigned IRQ resource enumeration. Accept both to keep the stable
    # `AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ|...` marker robust across guest versions.
    irq_diag = irq_diag_markers.get("virtio-blk")
    irq_diag_miniport = irq_diag_markers.get("virtio-blk-miniport")

    out_fields: dict[str, str] = {}

    def _set_if_missing(key: str, value: Optional[str]) -> None:
        if value is None:
            return
        if key not in out_fields:
            out_fields[key] = value

    # From the per-test marker (IOCTL query adds irq_mode + msix vectors).
    _set_if_missing("irq_mode", blk_test_fields.get("irq_mode") or blk_test_fields.get("mode") or blk_test_fields.get("interrupt_mode"))
    _set_if_missing("irq_message_count", blk_test_fields.get("irq_message_count"))
    _set_if_missing("irq_vectors", blk_test_fields.get("irq_vectors") or blk_test_fields.get("vectors"))
    _set_if_missing("msi_vector", blk_test_fields.get("msi_vector") or blk_test_fields.get("vector"))
    _set_if_missing("msix_config_vector", blk_test_fields.get("msix_config_vector"))
    _set_if_missing("msix_queue_vector", blk_test_fields.get("msix_queue_vector") or blk_test_fields.get("msix_queue0_vector"))

    if "irq_message_count" not in out_fields:
        _set_if_missing(
            "irq_message_count",
            blk_test_fields.get("messages") or blk_test_fields.get("irq_messages") or blk_test_fields.get("msi_messages"),
        )

    def _apply_irq_diag(diag: dict[str, str]) -> None:
        _set_if_missing("irq_mode", diag.get("irq_mode") or diag.get("mode") or diag.get("interrupt_mode"))
        if "irq_message_count" not in out_fields:
            _set_if_missing(
                "irq_message_count",
                diag.get("irq_message_count")
                or diag.get("messages")
                or diag.get("message_count")
                or diag.get("irq_messages")
                or diag.get("msi_messages"),
            )
        _set_if_missing("irq_vectors", diag.get("irq_vectors") or diag.get("vectors"))
        _set_if_missing("msi_vector", diag.get("msi_vector") or diag.get("vector"))
        _set_if_missing("msix_config_vector", diag.get("msix_config_vector"))
        _set_if_missing(
            "msix_queue_vector",
            diag.get("msix_queue_vector") or diag.get("msix_queue0_vector"),
        )

        # Preserve any additional interrupt-related fields so the marker stays useful for debugging.
        for k, v in diag.items():
            if k in ("level", "mode", "messages", "message_count", "vectors", "vector", "msix_queue0_vector"):
                continue
            if k.startswith(("irq_", "msi_", "msix_")):
                _set_if_missing(k, v)

    # From standalone IRQ diagnostics.
    #
    # Prefer the renamed miniport prefix (`virtio-blk-miniport-irq|...`) when present since
    # `virtio-blk-irq|...` may refer to cfgmgr32 resource enumeration on newer guests.
    if irq_diag_miniport is not None:
        _apply_irq_diag(irq_diag_miniport)
    if irq_diag is not None:
        _apply_irq_diag(irq_diag)

    # Backward compatible: emit nothing unless we saw at least one interrupt-related key.
    ordered_keys = (
        "irq_mode",
        "irq_message_count",
        "irq_vectors",
        "msi_vector",
        "msix_config_vector",
        "msix_queue_vector",
    )
    if not any(k in out_fields for k in ordered_keys) and not any(k.startswith("irq_") for k in out_fields):
        return

    status = "INFO"
    if blk_test_line is not None:
        if "FAIL" in blk_test_line.split("|"):
            status = "FAIL"
        elif "PASS" in blk_test_line.split("|"):
            status = "PASS"

    parts = [f"AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ|{status}"]
    for k in ordered_keys:
        if k in out_fields:
            parts.append(f"{k}={_sanitize_marker_value(out_fields[k])}")

    extra_irq = sorted(k for k in out_fields if k.startswith("irq_") and k not in ordered_keys)
    for k in extra_irq:
        parts.append(f"{k}={_sanitize_marker_value(out_fields[k])}")

    extra_msi = sorted(
        k for k in out_fields if (k.startswith("msi_") or k.startswith("msix_")) and k not in ordered_keys
    )
    for k in extra_msi:
        parts.append(f"{k}={_sanitize_marker_value(out_fields[k])}")
    print("|".join(parts))


def _require_virtio_blk_msix_marker(tail: bytes) -> tuple[bool, str]:
    """
    Return (ok, reason). `ok` is True iff the guest reported virtio-blk running in MSI-X mode
    via the marker: AERO_VIRTIO_SELFTEST|TEST|virtio-blk-msix|PASS|mode=msix|...
    """
    marker_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-msix|")
    if marker_line is None:
        return False, "missing virtio-blk-msix marker (guest selftest too old?)"

    parts = marker_line.split("|")
    if "FAIL" in parts:
        return False, "virtio-blk-msix marker reported FAIL"
    if "SKIP" in parts:
        return False, "virtio-blk-msix marker reported SKIP"

    fields = _parse_marker_kv_fields(marker_line)
    mode = fields.get("mode")
    if mode is None:
        return False, "virtio-blk-msix marker missing mode=... field"
    if mode != "msix":
        msgs = fields.get("messages", "?")
        return False, f"mode={mode} (expected msix) messages={msgs}"
    return True, "ok"


def _require_virtio_snd_msix_marker(tail: bytes) -> tuple[bool, str]:
    """
    Return (ok, reason). `ok` is True iff the guest reported virtio-snd running in MSI-X mode
    via the marker: AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|PASS|mode=msix|...
    """
    marker_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|")
    if marker_line is None:
        return False, "missing virtio-snd-msix marker (guest selftest too old?)"

    parts = marker_line.split("|")
    if "FAIL" in parts:
        return False, "virtio-snd-msix marker reported FAIL"
    if "SKIP" in parts:
        return False, "virtio-snd-msix marker reported SKIP"

    fields = _parse_marker_kv_fields(marker_line)
    mode = fields.get("mode")
    if mode is None:
        return False, "virtio-snd-msix marker missing mode=... field"
    if mode != "msix":
        msgs = fields.get("messages", "?")
        return False, f"mode={mode} (expected msix) messages={msgs}"
    return True, "ok"

def _emit_virtio_irq_host_marker(tail: bytes, *, device: str, host_marker: str) -> None:
    """
    Best-effort: emit a host-side marker describing the guest's IRQ mode/message count diagnostics.

    The guest-side selftest may include fields like `irq_mode=msi/msix/intx` and `irq_message_count=<n>`
    on its per-device TEST marker. This helper mirrors those fields into a stable host-side marker for
    log scraping/diagnostics.
    """
    marker_line = _try_extract_last_marker_line(tail, f"AERO_VIRTIO_SELFTEST|TEST|{device}|".encode("utf-8"))
    if marker_line is None:
        return

    fields = _parse_marker_kv_fields(marker_line)
    if not any(k.startswith("irq_") for k in fields):
        return

    status = "INFO"
    if "FAIL" in marker_line.split("|"):
        status = "FAIL"
    elif "PASS" in marker_line.split("|"):
        status = "PASS"

    parts = [f"AERO_VIRTIO_WIN7_HOST|{host_marker}|{status}"]

    # Keep ordering stable for log scraping.
    ordered = ["irq_mode", "irq_message_count"]
    for k in ordered:
        if k in fields:
            parts.append(f"{k}={_sanitize_marker_value(fields[k])}")

    extra = sorted(k for k in fields if k.startswith("irq_") and k not in ordered)
    for k in extra:
        parts.append(f"{k}={_sanitize_marker_value(fields[k])}")

    print("|".join(parts))


def _emit_virtio_net_irq_host_marker(tail: bytes) -> None:
    _emit_virtio_irq_host_marker(tail, device="virtio-net", host_marker="VIRTIO_NET_IRQ")


def _emit_virtio_snd_irq_host_marker(tail: bytes) -> None:
    _emit_virtio_irq_host_marker(tail, device="virtio-snd", host_marker="VIRTIO_SND_IRQ")


def _emit_virtio_input_irq_host_marker(tail: bytes) -> None:
    _emit_virtio_irq_host_marker(tail, device="virtio-input", host_marker="VIRTIO_INPUT_IRQ")


def _emit_virtio_snd_playback_host_marker(tail: bytes) -> None:
    """
    Best-effort: emit a host-side marker summarizing the guest's virtio-snd playback selftest.

    This does not affect harness PASS/FAIL; it's only for log scraping/diagnostics.
    """
    marker_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd|")
    if marker_line is None:
        return

    status = _try_extract_marker_status(marker_line)
    if status is None:
        return

    fields = _parse_marker_kv_fields(marker_line)
    parts = [f"AERO_VIRTIO_WIN7_HOST|VIRTIO_SND|{status}"]
    for k, v in fields.items():
        parts.append(f"{k}={_sanitize_marker_value(v)}")
    print("|".join(parts))


def _emit_virtio_snd_capture_host_marker(tail: bytes) -> None:
    """
    Best-effort: emit a host-side marker summarizing the guest's virtio-snd capture selftest.

    This does not affect harness PASS/FAIL; it's only for log scraping/diagnostics.
    """
    marker_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|")
    if marker_line is None:
        return

    status = _try_extract_marker_status(marker_line)
    if status is None:
        return

    fields = _parse_marker_kv_fields(marker_line)
    parts = [f"AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_CAPTURE|{status}"]
    for k in ("method", "frames", "non_silence", "silence_only", "reason"):
        if k in fields:
            parts.append(f"{k}={_sanitize_marker_value(fields[k])}")
    print("|".join(parts))


def _emit_virtio_snd_duplex_host_marker(tail: bytes) -> None:
    """
    Best-effort: emit a host-side marker summarizing the guest's virtio-snd duplex selftest.

    This does not affect harness PASS/FAIL; it's only for log scraping/diagnostics.
    """
    marker_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|")
    if marker_line is None:
        return

    status = _try_extract_marker_status(marker_line)
    if status is None:
        return

    fields = _parse_marker_kv_fields(marker_line)
    parts = [f"AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_DUPLEX|{status}"]
    for k in ("frames", "non_silence", "reason", "hr"):
        if k in fields:
            parts.append(f"{k}={_sanitize_marker_value(fields[k])}")
    print("|".join(parts))


def _emit_virtio_snd_buffer_limits_host_marker(tail: bytes) -> None:
    """
    Best-effort: emit a host-side marker summarizing the guest's virtio-snd buffer-limits selftest.

    This does not affect harness PASS/FAIL; it's only for log scraping/diagnostics.
    """
    marker_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|")
    if marker_line is None:
        return

    status = _try_extract_marker_status(marker_line)
    if status is None:
        return

    fields = _parse_marker_kv_fields(marker_line)
    parts = [f"AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_BUFFER_LIMITS|{status}"]
    for k in ("mode", "expected_failure", "buffer_bytes", "init_hr", "hr", "reason"):
        if k in fields:
            parts.append(f"{k}={_sanitize_marker_value(fields[k])}")
    print("|".join(parts))


def _emit_virtio_snd_format_host_marker(tail: bytes) -> None:
    """
    Best-effort: emit a host-side marker surfacing the negotiated virtio-snd endpoint mix formats.

    The guest selftest emits:
      AERO_VIRTIO_SELFTEST|TEST|virtio-snd-format|INFO|render=<...>|capture=<...>

    Mirror it into:
      AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_FORMAT|INFO|render=<...>|capture=<...>

    This does not affect harness PASS/FAIL; it's only for log scraping/diagnostics.
    """
    marker_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-format|")
    if marker_line is None:
        return

    toks = marker_line.split("|")
    status = toks[3] if len(toks) >= 4 else "INFO"
    if status not in ("PASS", "FAIL", "SKIP", "INFO"):
        status = "INFO"

    fields = _parse_marker_kv_fields(marker_line)
    parts = [f"AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_FORMAT|{status}"]
    for k in ("render", "capture"):
        if k in fields:
            parts.append(f"{k}={_sanitize_marker_value(fields[k])}")
    print("|".join(parts))


def _emit_virtio_snd_eventq_host_marker(tail: bytes) -> None:
    """
    Best-effort: emit a host-side marker summarizing the guest's virtio-snd eventq counters.

    This does not affect harness PASS/FAIL; it's only for log scraping/diagnostics.
    """
    marker_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-eventq|")
    if marker_line is None:
        return

    toks = marker_line.split("|")

    status = "INFO"
    if "FAIL" in toks:
        status = "FAIL"
    elif "PASS" in toks:
        status = "PASS"
    elif "SKIP" in toks:
        status = "SKIP"
    elif "INFO" in toks:
        status = "INFO"

    fields = _parse_marker_kv_fields(marker_line)
    parts = [f"AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_EVENTQ|{status}"]

    # The guest SKIP marker uses a plain token (e.g. `...|SKIP|device_missing`) rather than
    # a `reason=...` field. Mirror it as `reason=` so log scraping can treat it uniformly.
    if status == "SKIP" and "reason" not in fields:
        try:
            idx = toks.index("SKIP")
            if idx + 1 < len(toks):
                reason_tok = toks[idx + 1].strip()
                if reason_tok and "=" not in reason_tok:
                    parts.append(f"reason={_sanitize_marker_value(reason_tok)}")
        except Exception:
            pass

    # Keep ordering stable for log scraping.
    ordered = [
        "completions",
        "parsed",
        "short",
        "unknown",
        "jack_connected",
        "jack_disconnected",
        "pcm_period",
        "xrun",
        "ctl_notify",
    ]
    for k in ordered:
        if k in fields:
            parts.append(f"{k}={_sanitize_marker_value(fields[k])}")

    extra = sorted(k for k in fields if k not in ordered and k != "reason")
    for k in extra:
        parts.append(f"{k}={_sanitize_marker_value(fields[k])}")

    print("|".join(parts))


def _emit_virtio_blk_io_host_marker(tail: bytes, *, blk_test_line: Optional[str] = None) -> None:
    """
    Best-effort: emit a host-side marker describing the guest's virtio-blk file I/O throughput.

    This does not affect harness PASS/FAIL; it's only for log scraping/diagnostics.
    """
    if blk_test_line is None:
        blk_test_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|")
    if blk_test_line is None:
        return

    fields = _parse_marker_kv_fields(blk_test_line)
    # Backward compatible: older guest selftests will not include the I/O perf fields. Emit nothing
    # unless we see at least one throughput/byte count key.
    if (
        "write_bytes" not in fields
        and "write_mbps" not in fields
        and "read_bytes" not in fields
        and "read_mbps" not in fields
    ):
        return

    status = _try_extract_marker_status(blk_test_line) or "INFO"
    # Fall back to per-phase ok flags when the marker does not include a PASS/FAIL token.
    if status not in ("PASS", "FAIL"):
        if fields.get("write_ok") == "0" or fields.get("flush_ok") == "0" or fields.get("read_ok") == "0":
            status = "FAIL"
        elif fields.get("write_ok") == "1" and fields.get("flush_ok") == "1" and fields.get("read_ok") == "1":
            status = "PASS"

    parts = [f"AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IO|{status}"]
    for k in ("write_ok", "write_bytes", "write_mbps", "flush_ok", "read_ok", "read_bytes", "read_mbps"):
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
        if kind == "float" and fmt.bits_per_sample not in (32, 64):
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
