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

When `--with-virtio-snd/--require-virtio-snd/--enable-virtio-snd` is enabled, the harness also configures virtio-snd as a modern-only
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
  - verify QEMU-emitted virtio PCI Vendor/Device/Revision IDs via `query-pci` (when `--qemu-preflight-pci/--qmp-preflight-pci` is enabled)
  - trigger a virtio-blk runtime resize via `blockdev-resize` / legacy `block_resize` (when `--with-blk-resize/--with-virtio-blk-resize/--require-virtio-blk-resize/--enable-virtio-blk-resize` is enabled)
  - inject deterministic virtio-input events:
    - keyboard + relative mouse: `--with-input-events/--with-virtio-input-events/--require-virtio-input-events/--enable-virtio-input-events`
      - prefers QMP `input-send-event`, with backcompat fallbacks when unavailable
    - mouse wheel: `--with-input-wheel` (aliases: `--with-virtio-input-wheel`, `--require-virtio-input-wheel`, `--enable-virtio-input-wheel`)
      (requires QMP `input-send-event`)
    - Consumer Control (media keys): `--with-input-media-keys/--with-virtio-input-media-keys/--require-virtio-input-media-keys/--enable-virtio-input-media-keys`
      - prefers QMP `input-send-event`, with backcompat fallbacks when unavailable
    - tablet / absolute pointer: `--with-input-tablet-events/--with-tablet-events/--with-virtio-input-tablet-events/--require-virtio-input-tablet-events/--enable-virtio-input-tablet-events` (requires QMP `input-send-event`)
  - verify host-side MSI-X enablement on virtio PCI functions via QMP/QEMU introspection (when `--require-virtio-*-msix` is enabled)
  (unix socket on POSIX; TCP loopback fallback on Windows)
- tails the serial log until it sees AERO_VIRTIO_SELFTEST|RESULT|PASS/FAIL
  - in default (non-transitional) mode, a PASS result also requires per-test markers for virtio-blk, virtio-input,
     virtio-input-bind, virtio-snd (PASS or SKIP), virtio-snd-capture (PASS or SKIP), virtio-snd-duplex (PASS or SKIP),
     virtio-net, and virtio-net-udp so older selftest binaries cannot accidentally pass
  - when --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd is enabled, virtio-snd, virtio-snd-capture, and virtio-snd-duplex must PASS (not SKIP)
  - when --with-snd-buffer-limits/--with-virtio-snd-buffer-limits/--require-virtio-snd-buffer-limits/--enable-snd-buffer-limits/--enable-virtio-snd-buffer-limits is enabled, virtio-snd-buffer-limits must PASS (not FAIL/SKIP/missing)
  - when --with-input-events/--with-virtio-input-events/--require-virtio-input-events/--enable-virtio-input-events is enabled, virtio-input-events must PASS (not FAIL/missing)
  - when --with-input-leds/--with-virtio-input-leds/--require-virtio-input-leds/--enable-virtio-input-leds is enabled, virtio-input-leds must PASS (not SKIP/FAIL/missing) (provision the guest with
    --test-input-leds; newer guest selftests also accept --test-input-led and emit the legacy marker)
  - when --with-input-media-keys/--with-virtio-input-media-keys/--require-virtio-input-media-keys/--enable-virtio-input-media-keys is enabled, virtio-input-media-keys must PASS (not FAIL/missing)
  - when --with-input-led/--with-virtio-input-led/--require-virtio-input-led/--enable-virtio-input-led is enabled, virtio-input-led must PASS (not FAIL/SKIP/missing)
  - when --with-input-tablet-events/--with-tablet-events/--with-virtio-input-tablet-events/--require-virtio-input-tablet-events/--enable-virtio-input-tablet-events is enabled, virtio-input-tablet-events must PASS (not FAIL/missing)
  - when --with-blk-resize/--with-virtio-blk-resize/--require-virtio-blk-resize/--enable-virtio-blk-resize is enabled, virtio-blk-resize must PASS (not SKIP/FAIL/missing)
  - when --with-blk-reset/--with-virtio-blk-reset/--require-virtio-blk-reset/--enable-virtio-blk-reset is enabled, virtio-blk-reset must PASS (not SKIP/FAIL/missing)

For convenience when scraping CI logs, the harness may also emit stable host-side summary markers (best-effort;
do not affect PASS/FAIL):

- `AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IO|PASS/FAIL/INFO|write_ok=...|write_bytes=...|write_mbps=...|flush_ok=...|read_ok=...|read_bytes=...|read_mbps=...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESIZE|REQUEST|old_bytes=...|new_bytes=...|qmp_cmd=...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESIZE|FAIL|reason=...|old_bytes=...|new_bytes=...|drive_id=...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESIZE|PASS/FAIL/SKIP/READY|...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESET|PASS/FAIL/SKIP|performed=...|counter_before=...|counter_after=...|err=...|reason=...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_LARGE|PASS/FAIL/INFO|large_ok=...|large_bytes=...|large_fnv1a64=...|large_mbps=...|upload_ok=...|upload_bytes=...|upload_mbps=...|msi=...|msi_messages=...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_UDP|PASS/FAIL/SKIP|bytes=...|small_bytes=...|mtu_bytes=...|reason=...|wsa=...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_UDP_DNS|PASS/FAIL/SKIP|server=...|query=...|sent=...|recv=...|rcode=...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_OFFLOAD_CSUM|PASS/FAIL/INFO|tx_csum=...|rx_csum=...|fallback=...|...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_DIAG|INFO/WARN|reason=...|host_features=...|guest_features=...|...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_BIND|PASS/FAIL|devices=...|wrong_service=...|missing_service=...|problem=...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_BINDING|PASS/FAIL|service=...|pnp_id=...|reason=...|expected=...|actual=...|...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_SND|PASS/FAIL/SKIP|...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_CAPTURE|PASS/FAIL/SKIP|...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_DUPLEX|PASS/FAIL/SKIP|...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_BUFFER_LIMITS|PASS/FAIL/SKIP|...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_EVENTQ|INFO/SKIP|completions=...|pcm_period=...|xrun=...|...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_FORMAT|INFO|render=...|capture=...`

- It may also mirror the guest's UDP DNS smoke-test marker when present:
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_UDP_DNS|PASS/FAIL/SKIP|server=...|query=...|sent=...|recv=...|rcode=...|reason=...`
  - informational only; does not affect overall PASS/FAIL.

It may also mirror the guest's checksum offload marker (`virtio-net-offload-csum`) when present:
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_OFFLOAD_CSUM|PASS/FAIL/INFO|tx_csum=...|rx_csum=...|fallback=...|...`
  - informational only; does not affect overall PASS/FAIL unless `--require-net-csum-offload` is enabled.

It may also optionally flap the virtio-net link state via QMP `set_link` when `--with-net-link-flap` is enabled,
coordinated by a guest-side READY marker:

- `AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|READY` (guest)
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_LINK_FLAP|PASS/FAIL|name=...|down_delay_sec=...|reason=...` (host)

It may also mirror guest-side IRQ diagnostics (when present) into per-device host markers:

- `AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ|PASS/FAIL/INFO|irq_mode=...|irq_message_count=...|msix_config_vector=...|msix_queue_vector=...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RECOVERY|INFO|abort_srb=...|reset_device_srb=...|reset_bus_srb=...|pnp_srb=...|ioctl_reset=...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_COUNTERS|INFO/SKIP|abort=...|reset_device=...|reset_bus=...|pnp=...|ioctl_reset=...|capacity_change_events=<n|not_supported>`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESET_RECOVERY|INFO/SKIP|reset_detected=...|hw_reset_bus=...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_IRQ|PASS/FAIL/INFO|irq_mode=...|irq_message_count=...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_IRQ|PASS/FAIL/INFO|irq_mode=...|irq_message_count=...`
- `AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_IRQ|PASS/FAIL/INFO|irq_mode=...|irq_message_count=...`
- `AERO_VIRTIO_WIN7_HOST|QEMU_PCI_PREFLIGHT|PASS|mode=contract-v1/transitional|vendor=1af4|devices=...`

When `--require-no-blk-recovery` is enabled, the harness fails if any of the virtio-blk recovery counters are non-zero,
emitting a deterministic failure token:

- `FAIL: VIRTIO_BLK_RECOVERY_NONZERO: ...`

When `--fail-on-blk-recovery` is enabled, the harness fails if the guest reports non-zero virtio-blk abort/reset activity
via either:

- the dedicated `virtio-blk-counters` marker (preferred), or
- legacy `abort_srb`/`reset_*_srb` fields appended to the guest `virtio-blk` marker (older guest binaries).

If the dedicated marker is present but reports `SKIP`, the harness does **not** fall back (treats counters as
unavailable). On failure, emits:

- `FAIL: VIRTIO_BLK_RECOVERY_DETECTED: ...`

When `--require-no-blk-reset-recovery` is enabled, the harness fails if the guest reports non-zero virtio-blk timeout/error
recovery activity counters (`reset_detected` / `hw_reset_bus`) via either:

- the dedicated `virtio-blk-reset-recovery` marker (preferred), or
- the legacy miniport diagnostic line `virtio-blk-miniport-reset-recovery|INFO|...` (older guest binaries).

Missing/SKIP/WARN diagnostics are treated as unavailable and do not fail the run. On failure, emits:

- `FAIL: VIRTIO_BLK_RESET_RECOVERY_NONZERO: ...`

When `--fail-on-blk-reset-recovery` is enabled, the harness fails if the guest reports non-zero virtio-blk hardware reset
invocations (`hw_reset_bus`) via either:

- the dedicated `virtio-blk-reset-recovery` marker (preferred), or
- the legacy miniport diagnostic line `virtio-blk-miniport-reset-recovery|INFO|...` (older guest binaries).

Missing/SKIP/WARN diagnostics are treated as unavailable and do not fail the run. On failure, emits:

- `FAIL: VIRTIO_BLK_RESET_RECOVERY_DETECTED: ...`

When the guest virtio-snd selftest fails due to the `ForceNullBackend` bring-up toggle (which disables the virtio
transport and makes host-side wav verification silent), the harness emits:

- `FAIL: VIRTIO_SND_FORCE_NULL_BACKEND: ...`

Note: virtio-blk miniport IRQ diagnostics may report `mode=msi` even when MSI-X vectors are assigned; the harness infers
MSI-X (`irq_mode=msix`) when any `msix_*_vector` field is non-`0xFFFF`.

It also mirrors the standalone guest IRQ diagnostic lines (when present):

- `AERO_VIRTIO_WIN7_HOST|VIRTIO_*_IRQ_DIAG|INFO/WARN|...`
"""

from __future__ import annotations

import argparse
from collections import deque
import http.server
import hashlib
import json
import math
import re
import warnings
import os
import shutil
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


def _iter_qmp_query_pci_buses(query_pci_result: object) -> list[tuple[Optional[int], dict[str, object]]]:
    """
    Flatten the QMP `query-pci` bus tree into a list of bus objects.

    QEMU represents subordinate PCI buses behind bridge devices via a nested structure:
      - bus.devices[*].pci_bridge.bus.devices[*]...

    Older versions may also return a flat list. We support both by:
      - starting from the top-level list (when present), and
      - recursively traversing any nested `pci_bridge.bus` objects.

    We deduplicate by bus number when available to avoid double-counting if a given bus appears both
    in the top-level list and under a bridge (best-effort; bus numbers are assumed unique for the
    harness's usage).
    """
    buses: list[tuple[Optional[int], dict[str, object]]] = []
    if not isinstance(query_pci_result, list):
        return buses

    # Worklist of bus objects to visit.
    stack: list[dict[str, object]] = [b for b in query_pci_result if isinstance(b, dict)]
    seen_bus_nums: set[int] = set()

    while stack:
        bus_obj = stack.pop()
        bus_num = _qmp_maybe_int(bus_obj.get("bus"))
        if bus_num is None:
            # Nested buses under `pci_bridge` use `number` rather than `bus` in the QAPI schema.
            bus_num = _qmp_maybe_int(bus_obj.get("number"))

        if bus_num is not None:
            if bus_num in seen_bus_nums:
                continue
            seen_bus_nums.add(bus_num)

        buses.append((bus_num, bus_obj))

        devs = bus_obj.get("devices")
        if not isinstance(devs, list):
            continue
        for dev_obj in devs:
            if not isinstance(dev_obj, dict):
                continue
            bridge_obj = dev_obj.get("pci_bridge")
            if not isinstance(bridge_obj, dict):
                continue
            child_bus = bridge_obj.get("bus")
            if isinstance(child_bus, dict):
                stack.append(child_bus)

    return buses


def _iter_qmp_query_pci_devices(query_pci_result: object) -> list[_PciId]:
    """
    Attempt to extract vendor/device/subsystem/revision from QMP `query-pci` output.

    QEMU's `query-pci` QMP schema is stable but can vary slightly between versions; we treat
    unknown/missing fields as optional and ignore devices we can't parse.
    """
    devices: list[_PciId] = []

    for _, bus in _iter_qmp_query_pci_buses(query_pci_result):
        bus_devices = bus.get("devices")
        if not isinstance(bus_devices, list):
            continue
        for dev in bus_devices:
            if not isinstance(dev, dict):
                continue

            id_obj = dev.get("id")
            id_dict = id_obj if isinstance(id_obj, dict) else None

            vendor, device = _qmp_device_vendor_device_id(dev)  # type: ignore[arg-type]
            if vendor is None or device is None:
                continue

            subsys_vendor = _qmp_maybe_int(dev.get("subsystem_vendor_id"))
            if subsys_vendor is None and id_dict is not None:
                sv_obj = id_dict.get("subsystem_vendor_id")
                if sv_obj is None:
                    sv_obj = id_dict.get("subsystem_vendor")
                subsys_vendor = _qmp_maybe_int(sv_obj)
            subsys = _qmp_maybe_int(dev.get("subsystem_id"))
            if subsys is None and id_dict is not None:
                s_obj = id_dict.get("subsystem_id")
                if s_obj is None:
                    s_obj = id_dict.get("subsystem")
                subsys = _qmp_maybe_int(s_obj)

            rev = _qmp_maybe_int(dev.get("revision"))
            if rev is None and id_dict is not None:
                r_obj = id_dict.get("revision")
                if r_obj is None:
                    r_obj = id_dict.get("rev")
                rev = _qmp_maybe_int(r_obj)
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


_SERIAL_TAIL_CAP_BYTES = 131072


def _append_serial_tail(tail: bytes, chunk: bytes) -> bytes:
    """
    Append a newly read serial chunk to the rolling `tail` buffer.

    This keeps the rolling tail size bounded to `_SERIAL_TAIL_CAP_BYTES` without constructing an
    intermediate `tail + chunk` buffer larger than the cap (trim the existing tail *before*
    appending).
    If the newly read `chunk` itself exceeds the cap, only its last cap bytes are retained.
    """
    if not chunk:
        return tail

    cap = _SERIAL_TAIL_CAP_BYTES
    if len(chunk) >= cap:
        return chunk[-cap:]

    max_old = cap - len(chunk)
    if len(tail) > max_old:
        tail = tail[-max_old:]
    return tail + chunk


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
# Stable QOM `id=` value for the virtio-net device so QMP `set_link` can target it deterministically.
_VIRTIO_NET_QMP_ID = "aero_virtio_net0"


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


class _QmpCommandError(RuntimeError):
    """
    Structured error for a QMP command response containing `{"error": ...}`.

    Keeping the parsed `error.class`/`error.desc` fields makes it possible for callers to
    implement robust fallbacks (e.g. `input-send-event` → `send-key` / HMP) based on the
    actual QMP error type, rather than string matching.
    """

    def __init__(self, *, execute: str, resp: dict[str, object]):
        self.execute = execute
        self.resp = resp

        err = resp.get("error")
        err_dict = err if isinstance(err, dict) else {}
        self.error_class = str(err_dict.get("class") or "")
        self.error_desc = str(err_dict.get("desc") or "")

        # Keep the message stable-ish for log scraping / tests.
        msg = f"QMP command '{execute}' failed"
        if self.error_class or self.error_desc:
            msg += f": {self.error_class}: {self.error_desc}".rstrip()
        else:
            msg += f": {resp}"
        super().__init__(msg)


def _qmp_error_is_command_not_found(e: BaseException, *, command: str) -> bool:
    """
    Best-effort detection of QMP "unknown command" responses across QEMU versions.

    Newer QEMU builds typically respond with:
      {"error":{"class":"CommandNotFound","desc":"..."}}

    Some environments may only surface a stringified error. Keep the matching conservative
    and scoped to the requested `command`.
    """

    cmd = command.lower()
    # Match QEMU-style phrasing:
    #   "The command input-send-event has not been found"
    # and variants with quotes around the command name.
    cmd_pat = re.compile(
        rf"\bcommand\s+['\"]?{re.escape(cmd)}['\"]?\s+has\s+not\s+been\s+found\b"
    )
    if isinstance(e, _QmpCommandError):
        if (e.execute or "").lower() != cmd:
            # Structured QMP error refers to a different command; do not treat it as
            # `command` being missing.
            return False

        # Most accurate case: we have the QMP error response.
        cls = (e.error_class or "").lower()
        desc = (e.error_desc or "").lower()
        if cls == "commandnotfound":
            return True

        # Some QEMU builds may not use CommandNotFound but still describe an unknown command.
        # Keep this conservative so we don't accidentally treat "device not found" (etc) as a
        # missing QMP command.
        if cmd in desc and ("unknown command" in desc or "command not found" in desc):
            return True
        if cmd_pat.search(desc):
            return True
        return False

    # Best-effort fallback when callers surface only a stringified error.
    msg = str(e).lower()
    if cmd in msg and ("commandnotfound" in msg or "unknown command" in msg or "command not found" in msg):
        return True
    if cmd_pat.search(msg):
        return True
    return False


def _qmp_send_command(sock: socket.socket, cmd: dict[str, object]) -> dict[str, object]:
    resp = _qmp_send_command_raw(sock, cmd)
    if "error" in resp:
        execute = str(cmd.get("execute") or "")
        raise _QmpCommandError(execute=execute, resp=resp)
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
            vendor_obj = id_obj.get("vendor_id")
            if vendor_obj is None:
                vendor_obj = id_obj.get("vendor")
            vendor = _qmp_maybe_int(vendor_obj)
        if device is None:
            device_obj = id_obj.get("device_id")
            if device_obj is None:
                device_obj = id_obj.get("device")
            device = _qmp_maybe_int(device_obj)
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
    for bus_num, bus_obj in _iter_qmp_query_pci_buses(query_pci_return):
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

    req_flags: list[str] = []
    if require_virtio_net_msix:
        req_flags.append("--require-virtio-net-msix/--require-net-msix")
    if require_virtio_blk_msix:
        req_flags.append("--require-virtio-blk-msix/--require-blk-msix")
    if require_virtio_snd_msix:
        req_flags.append("--require-virtio-snd-msix/--require-snd-msix")
    ctx = f" (while {', '.join(req_flags)} was enabled)" if req_flags else ""

    try:
        query_infos, info_infos, query_supported, info_supported = _qmp_collect_pci_msix_info(endpoint)
    except Exception as e:
        return f"FAIL: QMP_MSIX_CHECK_FAILED: failed to query PCI state via QMP: {e}{ctx}"

    if not query_supported and not info_supported:
        return (
            "FAIL: QMP_MSIX_CHECK_UNSUPPORTED: QEMU QMP does not support query-pci or human-monitor-command "
            f"(required for MSI-X verification){ctx}"
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
            return (
                f"FAIL: {token}: did not find {device_name} PCI function(s) ({ids_str}) in QEMU PCI introspection output{ctx}"
            )

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
                f"{device_name} PCI function(s) ({ids_str}) (bdf={bdfs}{extra}){ctx}"
            )

        not_enabled = [i for i in matches if not i.msix_enabled]
        if not_enabled:
            i = not_enabled[0]
            return (
                f"FAIL: {token}: {device_name} PCI function {i.pci_id()} at {i.bdf()} "
                f"reported MSI-X disabled (source={i.source}){ctx}"
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


def _qmp_set_link_command(*, name: str, up: bool) -> dict[str, object]:
    """
    Build a QMP `set_link` command.

    This helper exists primarily so host-harness unit tests can sanity-check command structure.
    """
    return {"execute": "set_link", "arguments": {"name": str(name), "up": bool(up)}}


def _qmp_blockdev_resize_command(*, node_name: str, size: int) -> dict[str, object]:
    """
    Build a QMP `blockdev-resize` command (node-name based).

    This helper exists primarily so host-harness unit tests can sanity-check command structure.
    """
    return {"execute": "blockdev-resize", "arguments": {"node-name": node_name, "size": int(size)}}


def _qmp_block_resize_command(*, device: str, size: int) -> dict[str, object]:
    """
    Build a legacy QMP `block_resize` command (drive / block-backend id based).

    This helper exists primarily so host-harness unit tests can sanity-check command structure.
    """
    return {"execute": "block_resize", "arguments": {"device": device, "size": int(size)}}


def _qmp_send_key_command(*, qcodes: list[str], hold_time_ms: Optional[int] = None) -> dict[str, object]:
    """
    Build a QMP `send-key` command (legacy fallback for keyboard injection).

    Some older QEMU builds lack `input-send-event` but still expose `send-key`.
    """

    args: dict[str, object] = {
        "keys": [{"type": "qcode", "data": q} for q in qcodes],
    }
    if hold_time_ms is not None:
        # QMP uses `hold-time` in milliseconds.
        args["hold-time"] = int(hold_time_ms)
    return {"execute": "send-key", "arguments": args}


def _qmp_human_monitor_command(*, command_line: str) -> dict[str, object]:
    """
    Build a QMP `human-monitor-command` command.

    This is used as a fallback for older injection mechanisms (HMP `sendkey`, `mouse_move`,
    `mouse_button`, ...).
    """

    return {"execute": "human-monitor-command", "arguments": {"command-line": command_line}}


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
    backend: str


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
            # If the command itself doesn't exist, don't bother retrying without `device=`.
            if _qmp_error_is_command_not_found(e_with_device, command="input-send-event"):
                raise
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

        try:
            # Preferred path: QMP `input-send-event` (supports virtio device routing on newer QEMU).
            #
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
                        "--with-input-wheel/--with-virtio-input-wheel/--require-virtio-input-wheel/--enable-virtio-input-wheel or "
                        "--with-input-events-extended/--with-input-events-extra. "
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

            return _VirtioInputQmpInjectInfo(
                keyboard_device=kbd_device,
                mouse_device=mouse_device,
                backend="qmp_input_send_event",
            )
        except _QmpCommandError as e:
            # Backcompat: older QEMU builds lack `input-send-event`.
            if not _qmp_error_is_command_not_found(e, command="input-send-event"):
                raise

            if want_wheel:
                raise RuntimeError(
                    "QMP command 'input-send-event' is required for "
                    "--with-input-wheel/--with-virtio-input-wheel/--require-virtio-input-wheel/--enable-virtio-input-wheel or "
                    "--with-input-events-extended/--with-input-events-extra "
                    "(scroll/extra input injection), but this QEMU build does not support it. "
                    "Upgrade QEMU or omit those flags."
                ) from e

            # Fallback keyboard injection:
            # - Prefer QMP `send-key` (qcodes).
            # - Fall back further to HMP `sendkey` via QMP `human-monitor-command`.
            try:
                _qmp_send_command(s, _qmp_send_key_command(qcodes=["a"], hold_time_ms=50))
            except _QmpCommandError as e_send_key:
                if not _qmp_error_is_command_not_found(e_send_key, command="send-key"):
                    raise
                _qmp_send_command(s, _qmp_human_monitor_command(command_line="sendkey a"))

            # Fallback mouse injection: HMP `mouse_move` + `mouse_button`.
            _qmp_send_command(s, _qmp_human_monitor_command(command_line="mouse_move 10 5"))
            _qmp_send_command(s, _qmp_human_monitor_command(command_line="mouse_button 1"))
            _qmp_send_command(s, _qmp_human_monitor_command(command_line="mouse_button 0"))

            # Fallback paths are broadcast-only.
            return _VirtioInputQmpInjectInfo(
                keyboard_device=None,
                mouse_device=None,
                backend="hmp_fallback",
            )


@dataclass(frozen=True)
class _VirtioInputMediaKeysQmpInjectInfo:
    keyboard_device: Optional[str]
    qcode: str
    backend: str


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
            if _qmp_error_is_command_not_found(e_with_device, command="input-send-event"):
                raise
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
        backend = "qmp_input_send_event"
        kbd_device: Optional[str] = _VIRTIO_INPUT_QMP_KEYBOARD_ID
        ev = _qmp_deterministic_keyboard_events(qcode=qcode)

        try:
            # Preferred path: QMP `input-send-event`.
            # Media key: press + release.
            kbd_device = send(s, [ev[0]], device=kbd_device)
            time.sleep(0.05)
            kbd_device = send(s, [ev[1]], device=kbd_device)

            return _VirtioInputMediaKeysQmpInjectInfo(
                keyboard_device=kbd_device,
                qcode=qcode,
                backend=backend,
            )
        except _QmpCommandError as e:
            # Backcompat: older QEMU builds lack `input-send-event`.
            if not _qmp_error_is_command_not_found(e, command="input-send-event"):
                raise

            backend = "hmp_fallback"

            # Keyboard fallback:
            # - Prefer QMP `send-key` (qcodes).
            # - Fall back further to HMP `sendkey` via QMP `human-monitor-command`.
            try:
                _qmp_send_command(s, _qmp_send_key_command(qcodes=[qcode], hold_time_ms=50))
            except _QmpCommandError as e_send_key:
                if not _qmp_error_is_command_not_found(e_send_key, command="send-key"):
                    raise
                _qmp_send_command(s, _qmp_human_monitor_command(command_line=f"sendkey {qcode}"))

            # Fallback paths are broadcast-only.
            return _VirtioInputMediaKeysQmpInjectInfo(
                keyboard_device=None,
                qcode=qcode,
                backend=backend,
            )


@dataclass(frozen=True)
class _VirtioInputTabletQmpInjectInfo:
    tablet_device: Optional[str]
    backend: str


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
            if _qmp_error_is_command_not_found(e_with_device, command="input-send-event"):
                raise
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

        try:
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

            return _VirtioInputTabletQmpInjectInfo(
                tablet_device=tablet_device,
                backend="qmp_input_send_event",
            )
        except _QmpCommandError as e:
            if not _qmp_error_is_command_not_found(e, command="input-send-event"):
                raise
            raise RuntimeError(
                "QMP command 'input-send-event' is required for --with-input-tablet-events/--with-tablet-events/--with-virtio-input-tablet-events/--require-virtio-input-tablet-events/--enable-virtio-input-tablet-events "
                "(absolute pointer injection), but this QEMU build does not support it. "
                "Upgrade QEMU or omit --with-input-tablet-events/--with-tablet-events/--with-virtio-input-tablet-events/--require-virtio-input-tablet-events/--enable-virtio-input-tablet-events."
            ) from e


def _try_qmp_set_link(endpoint: _QmpEndpoint, *, name: str, up: bool) -> None:
    """
    Toggle a QEMU NIC link state via QMP `set_link`.

    Raises:
        RuntimeError: on QMP failure (including when `set_link` is unsupported).
    """
    with _qmp_connect(endpoint, timeout_seconds=5.0) as s:
        resp = _qmp_send_command_raw(s, _qmp_set_link_command(name=name, up=bool(up)))
        if "return" in resp:
            return

        err = resp.get("error")
        if isinstance(err, dict):
            desc = err.get("desc")
            # Detect "unknown command" responses across QEMU versions (some use GenericError with a
            # descriptive `desc` rather than the structured CommandNotFound class).
            if _qmp_error_is_command_not_found(
                _QmpCommandError(execute="set_link", resp=resp),
                command="set_link",
            ):
                raise RuntimeError(
                    "unsupported QEMU: QMP does not support set_link (required for --with-net-link-flap/--with-virtio-net-link-flap/--require-virtio-net-link-flap/--enable-virtio-net-link-flap). "
                    "Upgrade QEMU or omit --with-net-link-flap/--with-virtio-net-link-flap/--require-virtio-net-link-flap/--enable-virtio-net-link-flap."
                )
            if isinstance(desc, str) and desc:
                raise RuntimeError(f"QMP set_link failed: {desc}")

        raise RuntimeError(f"QMP set_link failed: {resp}")


def _try_qmp_set_link_any(endpoint: _QmpEndpoint, *, names: list[str], up: bool) -> str:
    """
    Best-effort `set_link` targeting helper.

    QEMU's `set_link` historically accepts a string "name" that can map to different identifiers
    depending on QEMU build/device configuration. For deterministic harness operation we primarily
    target the virtio-net QOM `id=` (e.g. "aero_virtio_net0"), but for compatibility we also allow
    falling back to a netdev id (e.g. "net0") when the first identifier is rejected.

    Returns:
        The name that succeeded.

    Raises:
        RuntimeError: when all names fail, or when `set_link` itself is unsupported.
    """
    if not names:
        raise ValueError("names must be non-empty")

    last_err: Optional[BaseException] = None
    for name in names:
        try:
            _try_qmp_set_link(endpoint, name=name, up=up)
            return name
        except RuntimeError as e:
            # Preserve the explicit unsupported-QEMU error path.
            if "unsupported QEMU" in str(e) and "set_link" in str(e):
                raise
            last_err = e
            continue

    if last_err is not None:
        raise RuntimeError(f"QMP set_link failed for names={names}: {last_err}") from last_err
    raise RuntimeError(f"QMP set_link failed for names={names}")


def _try_qmp_virtio_blk_resize(endpoint: _QmpEndpoint, *, drive_id: str, new_bytes: int) -> str:
    """
    Resize the virtio-blk backing device via QMP.

    Compatibility cascade:
    - Try `blockdev-resize` (node-name based).
    - Fall back to legacy `block_resize` (device / block-backend id based).
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


def _try_qmp_net_link_flap(
    endpoint: _QmpEndpoint, *, names: list[str], down_delay_seconds: float = 3.0
) -> str:
    """
    Flap a virtio-net link down/up via QMP `set_link`.

    Returns:
        The QMP `name` that succeeded (for logging).
    """
    name_used = _try_qmp_set_link_any(endpoint, names=names, up=False)
    time.sleep(float(down_delay_seconds))
    try:
        # Prefer reusing the same identifier that succeeded for the DOWN phase, but allow a
        # best-effort fallback to the other candidate names when bringing the link back UP.
        names_up = [name_used] + [n for n in names if n != name_used]
        name_used = _try_qmp_set_link_any(endpoint, names=names_up, up=True)
    except Exception as e:
        # Preserve the name that was accepted for the DOWN phase so callers can report it in
        # host-side markers even if the UP phase fails.
        err = RuntimeError(f"QMP set_link failed while bringing link UP (name={name_used}): {e}")
        setattr(err, "name_used", name_used)
        raise err from e
    return name_used


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

    Note: `vectors=0` is used to force INTx-only operation (disable MSI-X), but that
    is handled by `_qemu_device_arg_disable_msix` (not this helper).
    """

    if vectors is None:
        return device_arg
    vectors_i = int(vectors)
    if vectors_i <= 0:
        raise ValueError(f"vectors must be a positive integer (got {vectors})")

    # Avoid generating malformed args if callers accidentally include a trailing comma.
    arg = device_arg.rstrip()
    while arg.endswith(","):
        arg = arg[:-1]

    # If the device arg already specifies vectors, do not add a duplicate key.
    if ",vectors=" in ("," + arg):
        return arg

    return f"{arg},vectors={vectors_i}"


def _qemu_device_arg_apply_vectors(device_arg: str, vectors: Optional[int]) -> str:
    """
    Optionally append `,vectors=<N>` to a QEMU `-device` argument string.

    Unlike `_qemu_device_arg_add_vectors`, this helper allows `vectors=0`, which is a common QEMU
    mechanism to disable MSI-X for virtio-pci devices (forcing legacy INTx).
    """

    if vectors is None:
        return device_arg
    if int(vectors) < 0:
        raise ValueError(f"vectors must be a non-negative integer (got {vectors})")

    # Avoid generating malformed args if callers accidentally include a trailing comma.
    arg = device_arg.rstrip()
    while arg.endswith(","):
        arg = arg[:-1]

    # If the device arg already specifies vectors, do not add a duplicate key.
    if ",vectors=" in ("," + arg):
        return arg

    return f"{arg},vectors={int(vectors)}"


def _qemu_device_arg_disable_msix(device_arg: str, disable: bool) -> str:
    """
    Optionally append `,vectors=0` to a QEMU `-device` argument string.

    This is used by the host harness INTx-only test mode to force virtio-pci devices to expose no
    MSI-X capability (Windows 7 must use legacy INTx + ISR paths).
    """

    if not disable:
        return device_arg

    return _qemu_device_arg_apply_vectors(device_arg, 0)


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

    If unsupported, raise a clear error so users don't get an opaque QEMU startup
    failure (or silently run without the requested vectors/INTx configuration).
    """

    if vectors is None:
        return device_arg
    if not _qemu_device_supports_property(qemu_system, device_name, "vectors"):
        raise RuntimeError(
            f"QEMU device '{device_name}' does not expose the 'vectors' property, "
            f"but the harness was configured to set virtio vectors via {flag_name}={vectors}. "
            "Disable the flag or upgrade QEMU."
        )
    return _qemu_device_arg_add_vectors(device_arg, vectors)


def _virtio_snd_skip_failure_message(
    tail: bytes,
    *,
    marker_line: Optional[str] = None,
    skip_reason: Optional[str] = None,
) -> str:
    # The guest selftest's virtio-snd marker is intentionally strict and machine-friendly:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-snd|PASS/FAIL/SKIP|irq_mode=...|irq_message_count=...
    #
    # Any reason for SKIP is logged as human-readable text, so the host harness must infer
    # a useful error message from the tail log.

    # But still surface any stable IRQ details that the guest appended to the marker line, since
    # those survive tail truncation when we capture the marker incrementally.
    details = ""
    marker = marker_line
    if marker is not None:
        if (
            not marker.startswith("AERO_VIRTIO_SELFTEST|TEST|virtio-snd|")
            or _try_extract_marker_status(marker) != "SKIP"
        ):
            marker = None
    if marker is None:
        marker = _try_extract_last_marker_line(
            tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP"
        )
    if marker is not None:
        fields = _parse_marker_kv_fields(marker)
        parts: list[str] = []
        for k in ("irq_mode", "irq_message_count", "irq_reason"):
            v = (fields.get(k) or "").strip()
            if v:
                parts.append(f"{k}={_sanitize_marker_value(v)}")
        if parts:
            details = " (" + " ".join(parts) + ")"

    reason = (skip_reason or "").strip()
    if reason == "guest_not_configured_with_--test-snd":
        return (
            "FAIL: VIRTIO_SND_SKIPPED: virtio-snd test was skipped (guest not configured with --test-snd) "
            "but --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"
            + details
        )
    if reason == "--disable-snd":
        return (
            "FAIL: VIRTIO_SND_SKIPPED: virtio-snd test was skipped (--disable-snd) but --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"
            + details
        )
    if reason == "device_missing":
        return (
            "FAIL: VIRTIO_SND_SKIPPED: virtio-snd test was skipped (device missing) but --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"
            + details
        )

    if b"virtio-snd: skipped (enable with --test-snd)" in tail:
        return (
            "FAIL: VIRTIO_SND_SKIPPED: virtio-snd test was skipped (guest not configured with --test-snd) "
            "but --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"
            + details
        )
    if b"virtio-snd: disabled by --disable-snd" in tail:
        return (
            "FAIL: VIRTIO_SND_SKIPPED: virtio-snd test was skipped (--disable-snd) but --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"
            + details
        )
    if b"virtio-snd:" in tail and b"device not detected" in tail:
        return (
            "FAIL: VIRTIO_SND_SKIPPED: virtio-snd test was skipped (device missing) but --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"
            + details
        )
    return (
        "FAIL: VIRTIO_SND_SKIPPED: virtio-snd test was skipped but --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"
        + details
    )


def _try_extract_plain_marker_token(marker_line: str, status: str) -> Optional[str]:
    """
    Extract a plain (non key=value) token immediately following a marker status.

    Many guest markers use a compact format:
      AERO_VIRTIO_SELFTEST|TEST|<name>|FAIL|<reason>|key=value|...

    Returns:
      The token (e.g. "wrong_service") or None when unavailable.
    """
    try:
        toks = marker_line.split("|")
        if status not in toks:
            return None
        idx = toks.index(status)
        if idx + 1 >= len(toks):
            return None
        tok = toks[idx + 1].strip()
        if not tok or "=" in tok:
            return None
        return tok
    except Exception:
        return None


def _virtio_snd_fail_failure_message(tail: bytes, *, marker_line: Optional[str] = None) -> str:
    # virtio-snd playback marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL|irq_mode=...|irq_message_count=...
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL|wrong_service|irq_mode=...|...
    marker = marker_line
    if marker is not None:
        if not marker.startswith("AERO_VIRTIO_SELFTEST|TEST|virtio-snd|") or _try_extract_marker_status(marker) != "FAIL":
            marker = None
    if marker is None:
        marker = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL")

    details = ""
    if marker is not None:
        fields = _parse_marker_kv_fields(marker)
        reason = fields.get("reason", "").strip()
        if not reason:
            reason = _try_extract_plain_marker_token(marker, "FAIL") or ""
        parts: list[str] = []
        if reason:
            parts.append(f"reason={reason}")
        # Include a small, stable subset of IRQ diagnostics to aid debugging.
        for k in ("irq_mode", "irq_message_count", "irq_reason"):
            v = fields.get(k, "").strip()
            if v:
                parts.append(f"{k}={v}")
        if parts:
            details = " (" + " ".join(parts) + ")"

    return (
        "FAIL: VIRTIO_SND_FAILED: selftest RESULT=PASS but virtio-snd test reported FAIL"
        + details
    )


def _virtio_snd_capture_fail_failure_message(tail: bytes, *, marker_line: Optional[str] = None) -> str:
    # virtio-snd capture marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|FAIL|endpoint_missing
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|FAIL|force_null_backend
    marker = marker_line
    if marker is not None:
        if (
            not marker.startswith("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|")
            or _try_extract_marker_status(marker) != "FAIL"
        ):
            marker = None
    if marker is None:
        marker = _try_extract_last_marker_line(
            tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|FAIL"
        )

    details = ""
    if marker is not None:
        fields = _parse_marker_kv_fields(marker)
        reason = fields.get("reason", "").strip()
        if not reason:
            reason = _try_extract_plain_marker_token(marker, "FAIL") or ""
        hr = fields.get("hr", "").strip()
        parts: list[str] = []
        if reason:
            parts.append(f"reason={reason}")
        if hr:
            parts.append(f"hr={hr}")
        if parts:
            details = " (" + " ".join(parts) + ")"

    return (
        "FAIL: VIRTIO_SND_CAPTURE_FAILED: selftest RESULT=PASS but virtio-snd-capture test reported FAIL"
        + details
    )


def _virtio_snd_duplex_fail_failure_message(tail: bytes, *, marker_line: Optional[str] = None) -> str:
    # virtio-snd duplex marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|FAIL|reason=<...>|hr=0x...
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|FAIL|force_null_backend
    marker = marker_line
    if marker is not None:
        if (
            not marker.startswith("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|")
            or _try_extract_marker_status(marker) != "FAIL"
        ):
            marker = None
    if marker is None:
        marker = _try_extract_last_marker_line(
            tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|FAIL"
        )

    details = ""
    if marker is not None:
        fields = _parse_marker_kv_fields(marker)
        reason = fields.get("reason", "").strip()
        if not reason:
            reason = _try_extract_plain_marker_token(marker, "FAIL") or ""
        hr = fields.get("hr", "").strip()
        parts: list[str] = []
        if reason:
            parts.append(f"reason={reason}")
        if hr:
            parts.append(f"hr={hr}")
        if parts:
            details = " (" + " ".join(parts) + ")"

    return (
        "FAIL: VIRTIO_SND_DUPLEX_FAILED: selftest RESULT=PASS but virtio-snd-duplex test reported FAIL"
        + details
    )


def _virtio_snd_buffer_limits_fail_failure_message(tail: bytes, *, marker_line: Optional[str] = None) -> str:
    # virtio-snd-buffer-limits marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|FAIL|reason=<...>|hr=0x...
    marker = marker_line
    if marker is not None:
        if (
            not marker.startswith("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|")
            or _try_extract_marker_status(marker) != "FAIL"
        ):
            marker = None
    if marker is None:
        marker = _try_extract_last_marker_line(
            tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|FAIL"
        )

    details = ""
    if marker is not None:
        fields = _parse_marker_kv_fields(marker)
        reason = fields.get("reason", "").strip()
        if not reason:
            reason = _try_extract_plain_marker_token(marker, "FAIL") or ""
        hr = fields.get("hr", "").strip()
        parts: list[str] = []
        if reason:
            parts.append(f"reason={reason}")
        if hr:
            parts.append(f"hr={hr}")
        if parts:
            details = " (" + " ".join(parts) + ")"

    return (
        "FAIL: VIRTIO_SND_BUFFER_LIMITS_FAILED: virtio-snd-buffer-limits test reported FAIL while "
        "--with-snd-buffer-limits/--with-virtio-snd-buffer-limits/--require-virtio-snd-buffer-limits/--enable-snd-buffer-limits/--enable-virtio-snd-buffer-limits was enabled"
        + details
    )


_VIRTIO_SND_FORCE_NULL_BACKEND_REG_PATH = (
    r"HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters\ForceNullBackend"
)


def _try_virtio_snd_force_null_backend_failure_message(tail: bytes) -> Optional[str]:
    """
    If the guest virtio-snd tests failed due to ForceNullBackend, return a specific failure token/message.

    The guest selftest emits machine-friendly markers:
      AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL|force_null_backend|...
      AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|FAIL|force_null_backend
      AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|FAIL|force_null_backend
    """

    markers = (
        b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL|force_null_backend",
        b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|FAIL|force_null_backend",
        b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|FAIL|force_null_backend",
    )
    if not any(m in tail for m in markers):
        return None

    pnp_id: Optional[str] = None
    source: Optional[str] = None
    try:
        text = tail.decode("utf-8", errors="replace")
        m = re.search(r"ForceNullBackend=1 set \(pnp_id=([^\s]+) source=([^)]+)\)", text)
        if m:
            pnp_id = m.group(1)
            source = m.group(2)
        else:
            m2 = re.search(r"ForceNullBackend=1 set \(source=([^)]+)\)", text)
            if m2:
                source = m2.group(1)
    except Exception:
        # Best-effort: failure tokens must never raise.
        pnp_id = None
        source = None

    diag: list[str] = []
    if pnp_id:
        diag.append(f"pnp_id={pnp_id}")
    if source:
        diag.append(f"source={source}")
    diag_str = f" ({' '.join(diag)})" if diag else ""

    return (
        "FAIL: VIRTIO_SND_FORCE_NULL_BACKEND: virtio-snd selftest reported force_null_backend"
        f"{diag_str}; ForceNullBackend=1 disables the virtio-snd transport (host wav capture will be silent). "
        f"Clear the registry toggle to enable virtio-snd: {_VIRTIO_SND_FORCE_NULL_BACKEND_REG_PATH} (DWORD 0)."
    )


def _virtio_snd_capture_skip_failure_message(tail: bytes, *, marker_line: Optional[str] = None) -> str:
    # The capture marker is separate from the playback marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|PASS/FAIL/SKIP|...
    marker = marker_line
    if marker is not None:
        if (
            not marker.startswith("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|")
            or _try_extract_marker_status(marker) != "SKIP"
        ):
            marker = None
    if marker is None:
        marker = _try_extract_last_marker_line(
            tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP"
        )

    reason = ""
    if marker is not None:
        fields = _parse_marker_kv_fields(marker)
        reason = fields.get("reason", "").strip()
        if not reason:
            reason = _try_extract_plain_marker_token(marker, "SKIP") or ""

    if reason == "endpoint_missing":
        return "FAIL: VIRTIO_SND_CAPTURE_SKIPPED: virtio-snd capture endpoint missing but --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"
    if reason == "flag_not_set":
        return (
            "FAIL: VIRTIO_SND_CAPTURE_SKIPPED: virtio-snd capture test was skipped (flag_not_set) but --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled "
            "(ensure the guest is configured with --test-snd-capture or AERO_VIRTIO_SELFTEST_TEST_SND_CAPTURE=1)"
        )
    if reason == "disabled":
        return (
            "FAIL: VIRTIO_SND_CAPTURE_SKIPPED: virtio-snd capture test was skipped (disabled via --disable-snd or --disable-snd-capture) "
            "but --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"
        )
    if reason == "device_missing":
        return "FAIL: VIRTIO_SND_CAPTURE_SKIPPED: virtio-snd capture test was skipped (device missing) but --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"
    if reason:
        return (
            f"FAIL: VIRTIO_SND_CAPTURE_SKIPPED: virtio-snd capture test was skipped ({reason}) "
            "but --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"
        )
    return "FAIL: VIRTIO_SND_CAPTURE_SKIPPED: virtio-snd capture test was skipped but --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"


def _virtio_snd_duplex_skip_failure_message(tail: bytes, *, marker_line: Optional[str] = None) -> str:
    # Full-duplex marker (render + capture concurrently):
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|PASS/FAIL/SKIP|...
    marker = marker_line
    if marker is not None:
        if (
            not marker.startswith("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|")
            or _try_extract_marker_status(marker) != "SKIP"
        ):
            marker = None
    if marker is None:
        marker = _try_extract_last_marker_line(
            tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP"
        )

    reason = ""
    if marker is not None:
        fields = _parse_marker_kv_fields(marker)
        reason = fields.get("reason", "").strip()
        if not reason:
            reason = _try_extract_plain_marker_token(marker, "SKIP") or ""

    if reason == "endpoint_missing":
        return "FAIL: VIRTIO_SND_DUPLEX_SKIPPED: virtio-snd duplex test was skipped (endpoint_missing) but --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"
    if reason == "flag_not_set":
        return (
            "FAIL: VIRTIO_SND_DUPLEX_SKIPPED: virtio-snd duplex test was skipped (flag_not_set) but --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled "
            "(ensure the guest is configured with --test-snd-capture or AERO_VIRTIO_SELFTEST_TEST_SND_CAPTURE=1)"
        )
    if reason == "disabled":
        return (
            "FAIL: VIRTIO_SND_DUPLEX_SKIPPED: virtio-snd duplex test was skipped (disabled via --disable-snd or --disable-snd-capture) "
            "but --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"
        )
    if reason == "device_missing":
        return "FAIL: VIRTIO_SND_DUPLEX_SKIPPED: virtio-snd duplex test was skipped (device missing) but --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"
    if reason:
        return f"FAIL: VIRTIO_SND_DUPLEX_SKIPPED: virtio-snd duplex test was skipped ({reason}) but --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"
    return "FAIL: VIRTIO_SND_DUPLEX_SKIPPED: virtio-snd duplex test was skipped but --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"


def _virtio_snd_buffer_limits_skip_failure_message(tail: bytes, *, marker_line: Optional[str] = None) -> str:
    # Buffer limits stress test marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|PASS/FAIL/SKIP|...
    marker = marker_line
    if marker is not None:
        if (
            not marker.startswith("AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|")
            or _try_extract_marker_status(marker) != "SKIP"
        ):
            marker = None
    if marker is None:
        marker = _try_extract_last_marker_line(
            tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|SKIP"
        )

    reason = ""
    if marker is not None:
        fields = _parse_marker_kv_fields(marker)
        reason = fields.get("reason", "").strip()
        if not reason:
            reason = _try_extract_plain_marker_token(marker, "SKIP") or ""

    if reason == "flag_not_set":
        return (
            "FAIL: VIRTIO_SND_BUFFER_LIMITS_SKIPPED: virtio-snd-buffer-limits test was skipped (flag_not_set) but "
            "--with-snd-buffer-limits/--with-virtio-snd-buffer-limits/--require-virtio-snd-buffer-limits/--enable-snd-buffer-limits/--enable-virtio-snd-buffer-limits was enabled (provision the guest with --test-snd-buffer-limits or set "
            "AERO_VIRTIO_SELFTEST_TEST_SND_BUFFER_LIMITS=1)"
        )

    if reason:
        return (
            f"FAIL: VIRTIO_SND_BUFFER_LIMITS_SKIPPED: virtio-snd-buffer-limits test was skipped ({reason}) "
            "but --with-snd-buffer-limits/--with-virtio-snd-buffer-limits/--require-virtio-snd-buffer-limits/--enable-snd-buffer-limits/--enable-virtio-snd-buffer-limits was enabled"
        )
    return "FAIL: VIRTIO_SND_BUFFER_LIMITS_SKIPPED: virtio-snd-buffer-limits test was skipped but --with-snd-buffer-limits/--with-virtio-snd-buffer-limits/--require-virtio-snd-buffer-limits/--enable-snd-buffer-limits/--enable-virtio-snd-buffer-limits was enabled"


def _virtio_snd_buffer_limits_required_failure_message(
    tail: bytes,
    *,
    saw_pass: bool = False,
    saw_fail: bool = False,
    saw_skip: bool = False,
    marker_line: Optional[str] = None,
) -> Optional[str]:
    """
    Enforce that virtio-snd-buffer-limits ran and PASSed.

    Returns:
        A "FAIL: ..." message on failure, or None when the marker requirements are satisfied.
    """
    # Prefer explicit "saw_*" flags tracked by the main harness loop (these survive tail truncation),
    # but keep a tail scan fallback to support direct unit tests (and any legacy call sites).
    if saw_pass or b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|PASS" in tail:
        return None
    if saw_fail or b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|FAIL" in tail:
        return _virtio_snd_buffer_limits_fail_failure_message(tail, marker_line=marker_line)
    if saw_skip or b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|SKIP" in tail:
        return _virtio_snd_buffer_limits_skip_failure_message(tail, marker_line=marker_line)
    return (
        "FAIL: MISSING_VIRTIO_SND_BUFFER_LIMITS: did not observe virtio-snd-buffer-limits PASS marker while "
        "--with-snd-buffer-limits/--with-virtio-snd-buffer-limits/--require-virtio-snd-buffer-limits/--enable-snd-buffer-limits/--enable-virtio-snd-buffer-limits was enabled (provision the guest with --test-snd-buffer-limits or set "
        "AERO_VIRTIO_SELFTEST_TEST_SND_BUFFER_LIMITS=1)"
    )


def _virtio_blk_fail_failure_message(tail: bytes, *, marker_line: Optional[str] = None) -> str:
    # virtio-blk marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-blk|FAIL|irq_mode=...|irq_message_count=...|write_ok=...|...
    marker = marker_line
    if marker is not None:
        if (
            not marker.startswith("AERO_VIRTIO_SELFTEST|TEST|virtio-blk|")
            or _try_extract_marker_status(marker) != "FAIL"
        ):
            marker = None
    if marker is None:
        marker = _try_extract_last_marker_line(
            tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|FAIL"
        )

    details = ""
    if marker is not None:
        fields = _parse_marker_kv_fields(marker)
        parts: list[str] = []
        for k in (
            "write_ok",
            "flush_ok",
            "read_ok",
            "write_bytes",
            "read_bytes",
            "write_mbps",
            "read_mbps",
            "irq_mode",
            "irq_message_count",
            "irq_reason",
            "msix_config_vector",
            "msix_queue_vector",
        ):
            v = fields.get(k, "").strip()
            if v:
                parts.append(f"{k}={v}")
        if parts:
            details = " (" + " ".join(parts) + ")"

    return "FAIL: VIRTIO_BLK_FAILED: selftest RESULT=PASS but virtio-blk test reported FAIL" + details


def _virtio_input_fail_failure_message(tail: bytes, *, marker_line: Optional[str] = None) -> str:
    # virtio-input marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-input|FAIL|...|reason=<...>|irq_mode=...|irq_message_count=...
    marker = marker_line
    if marker is not None:
        if (
            not marker.startswith("AERO_VIRTIO_SELFTEST|TEST|virtio-input|")
            or _try_extract_marker_status(marker) != "FAIL"
        ):
            marker = None
    if marker is None:
        marker = _try_extract_last_marker_line(
            tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-input|FAIL"
        )

    details = ""
    if marker is not None:
        fields = _parse_marker_kv_fields(marker)
        parts: list[str] = []
        reason = fields.get("reason", "").strip()
        if reason:
            parts.append(f"reason={reason}")
        for k in (
            "devices",
            "keyboard_devices",
            "consumer_devices",
            "mouse_devices",
            "ambiguous_devices",
            "unknown_devices",
            "keyboard_collections",
            "consumer_collections",
            "mouse_collections",
            "tablet_devices",
            "tablet_collections",
            "irq_mode",
            "irq_message_count",
            "irq_reason",
        ):
            v = fields.get(k, "").strip()
            if v:
                parts.append(f"{k}={v}")
        if parts:
            details = " (" + " ".join(parts) + ")"

    return (
        "FAIL: VIRTIO_INPUT_FAILED: selftest RESULT=PASS but virtio-input test reported FAIL"
        + details
    )


def _virtio_input_bind_fail_failure_message(tail: bytes, *, marker_line: Optional[str] = None) -> str:
    # virtio-input-bind marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|FAIL|reason=<...>|expected=<...>|actual=<...>|pnp_id=<...>|...
    marker = marker_line
    if marker is not None:
        if (
            not marker.startswith("AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|")
            or _try_extract_marker_status(marker) != "FAIL"
        ):
            marker = None
    if marker is None:
        marker = _try_extract_last_marker_line(
            tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|FAIL"
        )

    details = ""
    if marker is not None:
        fields = _parse_marker_kv_fields(marker)
        parts: list[str] = []
        # Keep ordering stable so CI logs are deterministic (and easy to diff).
        for k in (
            "reason",
            "expected",
            "actual",
            "pnp_id",
            "devices",
            "wrong_service",
            "missing_service",
            "problem",
        ):
            v = fields.get(k, "").strip()
            if v:
                parts.append(f"{k}={v}")
        if parts:
            details = " (" + " ".join(parts) + ")"

    return (
        "FAIL: VIRTIO_INPUT_BIND_FAILED: selftest RESULT=PASS but virtio-input-bind test reported FAIL"
        + details
    )


def _virtio_net_fail_failure_message(tail: bytes, *, marker_line: Optional[str] = None) -> str:
    # virtio-net marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-net|FAIL|large_ok=...|...|upload_ok=...|...|msi_messages=...|irq_mode=...
    marker = marker_line
    if marker is not None:
        if (
            not marker.startswith("AERO_VIRTIO_SELFTEST|TEST|virtio-net|")
            or _try_extract_marker_status(marker) != "FAIL"
        ):
            marker = None
    if marker is None:
        marker = _try_extract_last_marker_line(
            tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|FAIL"
        )

    details = ""
    if marker is not None:
        fields = _parse_marker_kv_fields(marker)
        parts: list[str] = []
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
            "irq_mode",
            "irq_message_count",
            "irq_reason",
        ):
            v = fields.get(k, "").strip()
            if v:
                parts.append(f"{k}={v}")
        if parts:
            details = " (" + " ".join(parts) + ")"

    return "FAIL: VIRTIO_NET_FAILED: selftest RESULT=PASS but virtio-net test reported FAIL" + details


def _virtio_blk_reset_skip_failure_message(tail: bytes, *, marker_line: Optional[str] = None) -> str:
    # virtio-blk miniport reset marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|PASS|performed=1|counter_before=...|counter_after=...
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|SKIP|reason=flag_not_set
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|SKIP|reason=not_supported
    marker = marker_line
    if marker is None:
        marker = _try_extract_last_marker_line(
            tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|SKIP|"
        )
    if marker is not None:
        fields = _parse_marker_kv_fields(marker)
        reason = fields.get("reason")
        if not reason:
            # Backcompat: some older selftest binaries emit `...|SKIP|flag_not_set` (no `reason=` field),
            # similar to virtio-blk-resize and other SKIP markers.
            try:
                toks = marker.split("|")
                if toks and "SKIP" in toks:
                    idx = toks.index("SKIP")
                    if idx + 1 < len(toks):
                        tok = toks[idx + 1].strip()
                        if tok and "=" not in tok:
                            reason = tok
            except Exception:
                reason = reason
        if reason:
            if reason == "flag_not_set":
                return (
                    "FAIL: VIRTIO_BLK_RESET_SKIPPED: virtio-blk-reset test was skipped (flag_not_set) but "
                    "--with-blk-reset/--with-virtio-blk-reset/--require-virtio-blk-reset/--enable-virtio-blk-reset was enabled (provision the guest with --test-blk-reset)"
                )
            return (
                f"FAIL: VIRTIO_BLK_RESET_SKIPPED: virtio-blk-reset test was skipped ({reason}) "
                "but --with-blk-reset/--with-virtio-blk-reset/--require-virtio-blk-reset/--enable-virtio-blk-reset was enabled"
            )
    return "FAIL: VIRTIO_BLK_RESET_SKIPPED: virtio-blk-reset test was skipped but --with-blk-reset/--with-virtio-blk-reset/--require-virtio-blk-reset/--enable-virtio-blk-reset was enabled"


def _virtio_blk_reset_fail_failure_message(tail: bytes, *, marker_line: Optional[str] = None) -> str:
    # virtio-blk miniport reset marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|FAIL|reason=...|err=...
    marker = marker_line
    if marker is None:
        marker = _try_extract_last_marker_line(
            tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|FAIL|"
        )
    if marker is not None:
        fields = _parse_marker_kv_fields(marker)
        reason = fields.get("reason", "").strip()
        if not reason:
            # Backcompat: older selftest binaries may emit `...|FAIL|post_reset_io_failed|err=...` (no `reason=` field).
            try:
                toks = marker.split("|")
                if toks and "FAIL" in toks:
                    idx = toks.index("FAIL")
                    if idx + 1 < len(toks):
                        tok = toks[idx + 1].strip()
                        if tok and "=" not in tok:
                            reason = tok
            except Exception:
                reason = reason
        err = fields.get("err", "").strip()
        details = ""
        if reason or err:
            parts: list[str] = []
            if reason:
                parts.append(f"reason={reason}")
            if err:
                parts.append(f"err={err}")
            details = " (" + " ".join(parts) + ")"
        return (
            "FAIL: VIRTIO_BLK_RESET_FAILED: virtio-blk-reset test reported FAIL while "
            f"--with-blk-reset/--with-virtio-blk-reset/--require-virtio-blk-reset/--enable-virtio-blk-reset was enabled{details}"
        )
    return (
        "FAIL: VIRTIO_BLK_RESET_FAILED: virtio-blk-reset test reported FAIL while "
        "--with-blk-reset/--with-virtio-blk-reset/--require-virtio-blk-reset/--enable-virtio-blk-reset was enabled"
    )


def _virtio_blk_reset_missing_failure_message() -> str:
    return (
        "FAIL: MISSING_VIRTIO_BLK_RESET: did not observe virtio-blk-reset PASS marker while --with-blk-reset/--with-virtio-blk-reset/--require-virtio-blk-reset/--enable-virtio-blk-reset was enabled "
        "(guest selftest too old or missing --test-blk-reset)"
    )


def _virtio_net_link_flap_skip_failure_message(tail: bytes, *, marker_line: Optional[str] = None) -> str:
    # virtio-net link flap marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|SKIP|flag_not_set
    prefix = b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|SKIP|"
    prefix_str = "AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|SKIP|"
    if marker_line is not None:
        if prefix_str + "flag_not_set" in marker_line:
            return (
                "FAIL: VIRTIO_NET_LINK_FLAP_SKIPPED: virtio-net-link-flap test was skipped (flag_not_set) but "
                "--with-net-link-flap/--with-virtio-net-link-flap/--require-virtio-net-link-flap/--enable-virtio-net-link-flap was enabled (provision the guest with --test-net-link-flap)"
            )
        if marker_line.startswith(prefix_str):
            reason = marker_line[len(prefix_str) :].strip()
            if reason:
                return (
                    f"FAIL: VIRTIO_NET_LINK_FLAP_SKIPPED: virtio-net-link-flap test was skipped ({reason}) "
                    "but --with-net-link-flap/--with-virtio-net-link-flap/--require-virtio-net-link-flap/--enable-virtio-net-link-flap was enabled"
                )

    if b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|SKIP|flag_not_set" in tail:
        return (
            "FAIL: VIRTIO_NET_LINK_FLAP_SKIPPED: virtio-net-link-flap test was skipped (flag_not_set) but "
            "--with-net-link-flap/--with-virtio-net-link-flap/--require-virtio-net-link-flap/--enable-virtio-net-link-flap was enabled (provision the guest with --test-net-link-flap)"
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
                f"FAIL: VIRTIO_NET_LINK_FLAP_SKIPPED: virtio-net-link-flap test was skipped ({reason_str}) "
                "but --with-net-link-flap/--with-virtio-net-link-flap/--require-virtio-net-link-flap/--enable-virtio-net-link-flap was enabled"
            )
    return "FAIL: VIRTIO_NET_LINK_FLAP_SKIPPED: virtio-net-link-flap test was skipped but --with-net-link-flap/--with-virtio-net-link-flap/--require-virtio-net-link-flap/--enable-virtio-net-link-flap was enabled"


def _virtio_net_link_flap_fail_failure_message(
    tail: bytes, *, marker_line: Optional[str] = None
) -> str:
    # virtio-net link flap marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|FAIL|reason=...|...
    marker = None
    if marker_line is not None and marker_line.startswith(
        "AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|FAIL|"
    ):
        marker = marker_line
    if marker is None:
        marker = _try_extract_last_marker_line(
            tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|FAIL|"
        )
    details = ""
    if marker is not None:
        fields = _parse_marker_kv_fields(marker)
        parts: list[str] = []
        # Keep ordering stable so CI logs are deterministic (and easy to diff).
        for k in (
            "reason",
            "down_sec",
            "up_sec",
            "http_attempts",
            "cfg_vector",
            "cfg_intr_down_delta",
            "cfg_intr_up_delta",
        ):
            v = (fields.get(k) or "").strip()
            if v:
                parts.append(f"{k}={_sanitize_marker_value(v)}")
        if parts:
            details = " (" + " ".join(parts) + ")"
    return (
        "FAIL: VIRTIO_NET_LINK_FLAP_FAILED: virtio-net-link-flap test reported FAIL while "
        f"--with-net-link-flap/--with-virtio-net-link-flap/--require-virtio-net-link-flap/--enable-virtio-net-link-flap was enabled{details}"
    )


def _virtio_net_link_flap_required_failure_message(
    tail: bytes,
    *,
    saw_pass: bool = False,
    saw_fail: bool = False,
    saw_skip: bool = False,
    marker_line: Optional[str] = None,
) -> Optional[str]:
    """
    Enforce that virtio-net-link-flap ran and PASSed.

    Returns:
        A deterministic "FAIL: ..." message on failure, or None when the marker requirement is satisfied.
    """
    # Prefer the incrementally captured last marker line when available so we don't depend on the rolling
    # tail buffer still containing the marker.
    if marker_line is not None:
        toks = marker_line.split("|")
        status_tok = toks[3] if len(toks) >= 4 else ""
        if status_tok == "PASS":
            return None
        if status_tok == "FAIL":
            return _virtio_net_link_flap_fail_failure_message(tail, marker_line=marker_line)
        if status_tok == "SKIP":
            return _virtio_net_link_flap_skip_failure_message(tail, marker_line=marker_line)

    # Prefer explicit "saw_*" flags tracked by the main harness loop (these survive tail truncation), but
    # keep a tail scan fallback to support direct unit tests (and any legacy call sites).
    if saw_pass or b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|PASS" in tail:
        return None
    if saw_fail or b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|FAIL" in tail:
        return _virtio_net_link_flap_fail_failure_message(tail, marker_line=marker_line)
    if saw_skip or b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|SKIP" in tail:
        return _virtio_net_link_flap_skip_failure_message(tail, marker_line=marker_line)
    return (
        "FAIL: MISSING_VIRTIO_NET_LINK_FLAP: did not observe virtio-net-link-flap PASS marker while "
        "--with-net-link-flap/--with-virtio-net-link-flap/--require-virtio-net-link-flap/--enable-virtio-net-link-flap was enabled (provision the guest with --test-net-link-flap)"
    )


def _virtio_net_udp_fail_failure_message(tail: bytes, *, marker_line: Optional[str] = None) -> str:
    # Guest marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|PASS|bytes=...|small_bytes=...|mtu_bytes=...|reason=-|wsa=0
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|FAIL|bytes=...|small_bytes=...|mtu_bytes=...|reason=...|wsa=<err>
    prefix = b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|FAIL|"
    prefix_str = "AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|FAIL|"
    marker = marker_line
    if marker is not None and not marker.startswith(prefix_str):
        marker = None
    if marker is None:
        marker = _try_extract_last_marker_line(tail, prefix)
    if marker is not None:
        fields = _parse_marker_kv_fields(marker)
        parts: list[str] = []
        # Keep ordering stable for log scraping / CI diffs.
        for k in ("reason", "wsa", "bytes", "small_bytes", "mtu_bytes"):
            v = (fields.get(k) or "").strip()
            if v:
                parts.append(f"{k}={_sanitize_marker_value(v)}")
        details = ""
        if parts:
            details = " (" + " ".join(parts) + ")"
        return f"FAIL: VIRTIO_NET_UDP_FAILED: virtio-net-udp test reported FAIL{details}"
    return "FAIL: VIRTIO_NET_UDP_FAILED: virtio-net-udp test reported FAIL"


def _virtio_input_led_skip_failure_message(tail: bytes, *, marker_line: Optional[str] = None) -> str:
    # Guest marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|PASS/FAIL/SKIP|...
    #
    # The guest skip token is expected to be `flag_not_set` when the test was not enabled.
    prefix_str = "AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|SKIP|"
    if marker_line is not None and marker_line.startswith(prefix_str):
        reason = marker_line[len(prefix_str) :].strip()
        if reason:
            if reason == "flag_not_set":
                return (
                    "FAIL: VIRTIO_INPUT_LED_SKIPPED: virtio-input-led test was skipped (flag_not_set) but --with-input-led/--with-virtio-input-led/--require-virtio-input-led/--enable-virtio-input-led was enabled "
                    "(provision the guest with --test-input-led)"
                )
            return (
                f"FAIL: VIRTIO_INPUT_LED_SKIPPED: virtio-input-led test was skipped ({reason}) but --with-input-led/--with-virtio-input-led/--require-virtio-input-led/--enable-virtio-input-led was enabled "
                "(provision the guest with --test-input-led)"
            )

    if b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|SKIP|flag_not_set" in tail:
        return (
            "FAIL: VIRTIO_INPUT_LED_SKIPPED: virtio-input-led test was skipped (flag_not_set) but --with-input-led/--with-virtio-input-led/--require-virtio-input-led/--enable-virtio-input-led was enabled "
            "(provision the guest with --test-input-led)"
        )
    return (
        "FAIL: VIRTIO_INPUT_LED_SKIPPED: virtio-input-led test was skipped but --with-input-led/--with-virtio-input-led/--require-virtio-input-led/--enable-virtio-input-led was enabled "
        "(provision the guest with --test-input-led)"
    )


def _virtio_input_led_fail_failure_message(tail: bytes, *, marker_line: Optional[str] = None) -> str:
    # Guest marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|FAIL|reason=<...>|err=<win32>|sent=<n>|format=<...>|led=<...>
    prefix = b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|FAIL|"
    prefix_str = "AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|FAIL|"
    marker = marker_line
    if marker is not None and not marker.startswith(prefix_str):
        marker = None
    if marker is None:
        marker = _try_extract_last_marker_line(tail, prefix)
    if marker is not None:
        fields = _parse_marker_kv_fields(marker)
        reason = (fields.get("reason") or "").strip()
        if not reason:
            # Backcompat: allow token-only FAIL markers (no `reason=` field).
            try:
                toks = marker.split("|")
                if toks and "FAIL" in toks:
                    idx = toks.index("FAIL")
                    if idx + 1 < len(toks):
                        tok = toks[idx + 1].strip()
                        if tok and "=" not in tok:
                            reason = tok
            except Exception:
                reason = reason
        parts: list[str] = []
        if reason:
            parts.append(f"reason={_sanitize_marker_value(reason)}")
        for k in ("err", "sent", "format", "led"):
            v = (fields.get(k) or "").strip()
            if v:
                parts.append(f"{k}={_sanitize_marker_value(v)}")
        details = ""
        if parts:
            details = " (" + " ".join(parts) + ")"
        return (
            "FAIL: VIRTIO_INPUT_LED_FAILED: virtio-input-led test reported FAIL while --with-input-led/--with-virtio-input-led/--require-virtio-input-led/--enable-virtio-input-led was enabled"
            + details
        )
    return (
        "FAIL: VIRTIO_INPUT_LED_FAILED: virtio-input-led test reported FAIL while --with-input-led/--with-virtio-input-led/--require-virtio-input-led/--enable-virtio-input-led was enabled"
    )


def _virtio_input_led_required_failure_message(
    tail: bytes,
    *,
    saw_pass: bool = False,
    saw_fail: bool = False,
    saw_skip: bool = False,
    marker_line: Optional[str] = None,
) -> Optional[str]:
    """
    Enforce that virtio-input-led ran and PASSed.

    Returns:
        A "FAIL: ..." message on failure, or None when the marker requirements are satisfied.
    """
    # Prefer explicit "saw_*" flags tracked by the main harness loop (these survive tail truncation),
    # but keep a tail scan fallback to support direct unit tests.
    prefix = b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|"
    # Prefer an explicit marker line when available (survives tail truncation).
    if marker_line is not None:
        st = _try_extract_marker_status(marker_line)
        if st == "PASS":
            return None
        if st == "FAIL":
            return _virtio_input_led_fail_failure_message(tail, marker_line=marker_line)
        if st == "SKIP":
            return _virtio_input_led_skip_failure_message(tail, marker_line=marker_line)

    if saw_pass or prefix + b"PASS" in tail:
        return None
    if saw_fail or prefix + b"FAIL" in tail:
        return _virtio_input_led_fail_failure_message(tail, marker_line=marker_line)
    if saw_skip or prefix + b"SKIP" in tail:
        return _virtio_input_led_skip_failure_message(tail, marker_line=marker_line)
    return (
        "FAIL: MISSING_VIRTIO_INPUT_LED: did not observe virtio-input-led PASS marker while --with-input-led/--with-virtio-input-led/--require-virtio-input-led/--enable-virtio-input-led was enabled "
        "(provision the guest with --test-input-led)"
    )


def _virtio_input_events_fail_failure_message(
    tail: bytes, *, marker_line: Optional[str] = None, req_flags_desc: str = "--with-input-events"
) -> str:
    # Guest marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|FAIL|reason=<...>|err=<win32>|kbd_reports=...|mouse_reports=...|...
    prefix = b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|FAIL|"
    prefix_str = "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|FAIL|"
    marker = marker_line
    if marker is not None and not marker.startswith(prefix_str):
        marker = None
    if marker is None:
        marker = _try_extract_last_marker_line(tail, prefix)

    details = ""
    if marker is not None:
        fields = _parse_marker_kv_fields(marker)
        reason = (fields.get("reason") or "").strip()
        if not reason:
            # Backcompat: allow `...|FAIL|timeout|err=...` (no `reason=` key).
            try:
                toks = marker.split("|")
                if toks and "FAIL" in toks:
                    idx = toks.index("FAIL")
                    if idx + 1 < len(toks):
                        tok = toks[idx + 1].strip()
                        if tok and "=" not in tok:
                            reason = tok
            except Exception:
                reason = reason
        parts: list[str] = []
        if reason:
            parts.append(f"reason={_sanitize_marker_value(reason)}")
        err = (fields.get("err") or "").strip()
        if err:
            parts.append(f"err={_sanitize_marker_value(err)}")
        for k in (
            "kbd_reports",
            "mouse_reports",
            "kbd_bad_reports",
            "mouse_bad_reports",
            "kbd_a_down",
            "kbd_a_up",
            "mouse_move",
            "mouse_left_down",
            "mouse_left_up",
        ):
            v = (fields.get(k) or "").strip()
            if v:
                parts.append(f"{k}={_sanitize_marker_value(v)}")
        if parts:
            details = " (" + " ".join(parts) + ")"

    return (
        "FAIL: VIRTIO_INPUT_EVENTS_FAILED: virtio-input-events test reported FAIL while "
        f"{req_flags_desc} was enabled{details}"
    )


def _virtio_input_events_extended_fail_failure_message(
    tail: bytes,
    *,
    modifiers_marker_line: Optional[str] = None,
    buttons_marker_line: Optional[str] = None,
    wheel_marker_line: Optional[str] = None,
) -> str:
    # Guest markers:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-modifiers|FAIL|reason=...|err=...|kbd_reports=...
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-buttons|FAIL|reason=...|err=...|mouse_reports=...
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-wheel|FAIL|reason=...|err=...|wheel_total=...|...
    candidates: list[tuple[str, Optional[str], bytes, str]] = [
        (
            "virtio-input-events-modifiers",
            modifiers_marker_line,
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-modifiers|FAIL|",
            "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-modifiers|FAIL|",
        ),
        (
            "virtio-input-events-buttons",
            buttons_marker_line,
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-buttons|FAIL|",
            "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-buttons|FAIL|",
        ),
        (
            "virtio-input-events-wheel",
            wheel_marker_line,
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-wheel|FAIL|",
            "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-wheel|FAIL|",
        ),
    ]

    marker: Optional[str] = None
    subtest = "virtio-input-events-*"
    for name, ml, _prefix, prefix_str in candidates:
        if ml is not None and ml.startswith(prefix_str):
            marker = ml
            subtest = name
            break

    if marker is None:
        for name, _ml, prefix, _prefix_str in candidates:
            m = _try_extract_last_marker_line(tail, prefix)
            if m is not None:
                marker = m
                subtest = name
                break

    details = ""
    if marker is not None:
        fields = _parse_marker_kv_fields(marker)
        reason = (fields.get("reason") or "").strip()
        if not reason:
            reason = _try_extract_plain_marker_token(marker, "FAIL") or ""
        err = (fields.get("err") or "").strip()
        parts: list[str] = []
        if reason:
            parts.append(f"reason={_sanitize_marker_value(reason)}")
        if err:
            parts.append(f"err={_sanitize_marker_value(err)}")

        keys: tuple[str, ...] = ()
        if subtest == "virtio-input-events-modifiers":
            keys = (
                "kbd_reports",
                "kbd_bad_reports",
                "shift_b",
                "ctrl_down",
                "ctrl_up",
                "alt_down",
                "alt_up",
                "f1_down",
                "f1_up",
            )
        elif subtest == "virtio-input-events-buttons":
            keys = (
                "mouse_reports",
                "mouse_bad_reports",
                "side_down",
                "side_up",
                "extra_down",
                "extra_up",
            )
        elif subtest == "virtio-input-events-wheel":
            keys = (
                "mouse_reports",
                "mouse_bad_reports",
                "wheel_total",
                "hwheel_total",
                "expected_wheel",
                "expected_hwheel",
                "saw_wheel",
                "saw_hwheel",
            )

        for k in keys:
            v = (fields.get(k) or "").strip()
            if v:
                parts.append(f"{k}={_sanitize_marker_value(v)}")

        if parts:
            details = " (" + " ".join(parts) + ")"

    return (
        "FAIL: VIRTIO_INPUT_EVENTS_EXTENDED_FAILED: "
        f"{subtest} reported FAIL while --with-input-events-extended/--with-input-events-extra was enabled{details}"
    )


def _virtio_input_media_keys_fail_failure_message(tail: bytes, *, marker_line: Optional[str] = None) -> str:
    # Guest marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|FAIL|reason=<...>|err=<win32>|reports=<n>|volume_up_down=<0/1>|volume_up_up=<0/1>
    prefix = b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|FAIL|"
    prefix_str = "AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|FAIL|"
    marker = marker_line
    if marker is not None and not marker.startswith(prefix_str):
        marker = None
    if marker is None:
        marker = _try_extract_last_marker_line(tail, prefix)
    if marker is not None:
        fields = _parse_marker_kv_fields(marker)
        reason = (fields.get("reason") or "").strip()
        if not reason:
            # Backcompat: allow token-only FAIL markers (no `reason=` key).
            try:
                toks = marker.split("|")
                if toks and "FAIL" in toks:
                    idx = toks.index("FAIL")
                    if idx + 1 < len(toks):
                        tok = toks[idx + 1].strip()
                        if tok and "=" not in tok:
                            reason = tok
            except Exception:
                reason = reason
        parts: list[str] = []
        if reason:
            parts.append(f"reason={_sanitize_marker_value(reason)}")
        err = (fields.get("err") or "").strip()
        if err:
            parts.append(f"err={_sanitize_marker_value(err)}")
        for k in ("reports", "volume_up_down", "volume_up_up"):
            v = (fields.get(k) or "").strip()
            if v:
                parts.append(f"{k}={_sanitize_marker_value(v)}")
        details = ""
        if parts:
            details = " (" + " ".join(parts) + ")"
        return (
            "FAIL: VIRTIO_INPUT_MEDIA_KEYS_FAILED: virtio-input-media-keys test reported FAIL while "
            f"--with-input-media-keys/--with-virtio-input-media-keys/--require-virtio-input-media-keys/--enable-virtio-input-media-keys was enabled{details}"
        )
    return (
        "FAIL: VIRTIO_INPUT_MEDIA_KEYS_FAILED: virtio-input-media-keys test reported FAIL while "
        "--with-input-media-keys/--with-virtio-input-media-keys/--require-virtio-input-media-keys/--enable-virtio-input-media-keys was enabled"
    )


def _virtio_input_tablet_events_skip_failure_message(
    tail: bytes, *, marker_line: Optional[str] = None
) -> str:
    # Guest marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|READY
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|PASS|...
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|FAIL|...
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|SKIP|flag_not_set
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|SKIP|no_tablet_device
    #
    # Some newer guests may include a `reason=` field, while older markers use token-only reasons
    # after `|SKIP|` (no `=`).
    prefix = b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|SKIP|"
    prefix_str = "AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|SKIP|"
    marker = marker_line
    if marker is not None and not marker.startswith(prefix_str):
        marker = None
    if marker is None:
        marker = _try_extract_last_marker_line(tail, prefix)
    if marker is not None:
        fields = _parse_marker_kv_fields(marker)
        reason = fields.get("reason")
        if not reason:
            # Backcompat: `...|SKIP|flag_not_set`/`...|SKIP|no_tablet_device` (no `reason=` key).
            try:
                toks = marker.split("|")
                if toks and "SKIP" in toks:
                    idx = toks.index("SKIP")
                    if idx + 1 < len(toks):
                        tok = toks[idx + 1].strip()
                        if tok and "=" not in tok:
                            reason = tok
            except Exception:
                reason = reason
        if reason:
            if reason == "flag_not_set":
                return (
                    "FAIL: VIRTIO_INPUT_TABLET_EVENTS_SKIPPED: virtio-input-tablet-events test was skipped (flag_not_set) but "
                    "--with-input-tablet-events/--with-tablet-events/--with-virtio-input-tablet-events/--require-virtio-input-tablet-events/--enable-virtio-input-tablet-events was enabled (provision the guest with --test-input-tablet-events/--test-tablet-events)"
                )
            if reason == "no_tablet_device":
                return (
                    "FAIL: VIRTIO_INPUT_TABLET_EVENTS_SKIPPED: virtio-input-tablet-events test was skipped (no_tablet_device) but "
                    "--with-input-tablet-events/--with-tablet-events/--with-virtio-input-tablet-events/--require-virtio-input-tablet-events/--enable-virtio-input-tablet-events was enabled (ensure a virtio-tablet device is attached "
                    "(--with-virtio-tablet) and the guest tablet driver is installed)"
                )
            return (
                f"FAIL: VIRTIO_INPUT_TABLET_EVENTS_SKIPPED: virtio-input-tablet-events test was skipped ({reason}) but "
                "--with-input-tablet-events/--with-tablet-events/--with-virtio-input-tablet-events/--require-virtio-input-tablet-events/--enable-virtio-input-tablet-events was enabled"
            )
    return (
        "FAIL: VIRTIO_INPUT_TABLET_EVENTS_SKIPPED: virtio-input-tablet-events test was skipped but "
        "--with-input-tablet-events/--with-tablet-events/--with-virtio-input-tablet-events/--require-virtio-input-tablet-events/--enable-virtio-input-tablet-events was enabled"
    )


def _virtio_input_tablet_events_fail_failure_message(
    tail: bytes, *, marker_line: Optional[str] = None
) -> str:
    # Guest marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|FAIL|reason=...|err=...|tablet_reports=...|...
    prefix = b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|FAIL|"
    prefix_str = "AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|FAIL|"
    marker = marker_line
    if marker is not None and not marker.startswith(prefix_str):
        marker = None
    if marker is None:
        marker = _try_extract_last_marker_line(tail, prefix)
    if marker is not None:
        fields = _parse_marker_kv_fields(marker)
        reason = (fields.get("reason") or "").strip()
        if not reason:
            # Backcompat: allow `...|FAIL|missing_tablet_device|err=...` (no `reason=` field).
            try:
                toks = marker.split("|")
                if toks and "FAIL" in toks:
                    idx = toks.index("FAIL")
                    if idx + 1 < len(toks):
                        tok = toks[idx + 1].strip()
                        if tok and "=" not in tok:
                            reason = tok
            except Exception:
                reason = reason
        err = (fields.get("err") or "").strip()
        parts: list[str] = []
        if reason:
            parts.append(f"reason={_sanitize_marker_value(reason)}")
        if err:
            parts.append(f"err={_sanitize_marker_value(err)}")
        for k in (
            "tablet_reports",
            "move_target",
            "left_down",
            "left_up",
            "last_x",
            "last_y",
            "last_left",
        ):
            v = (fields.get(k) or "").strip()
            if v:
                parts.append(f"{k}={_sanitize_marker_value(v)}")
        details = ""
        if parts:
            details = " (" + " ".join(parts) + ")"
        return (
            "FAIL: VIRTIO_INPUT_TABLET_EVENTS_FAILED: virtio-input-tablet-events test reported FAIL while "
            f"--with-input-tablet-events/--with-tablet-events/--with-virtio-input-tablet-events/--require-virtio-input-tablet-events/--enable-virtio-input-tablet-events was enabled{details}"
        )
    return (
        "FAIL: VIRTIO_INPUT_TABLET_EVENTS_FAILED: virtio-input-tablet-events test reported FAIL while "
        "--with-input-tablet-events/--with-tablet-events/--with-virtio-input-tablet-events/--require-virtio-input-tablet-events/--enable-virtio-input-tablet-events was enabled"
    )


def _virtio_input_wheel_skip_failure_message(tail: bytes, *, marker_line: Optional[str] = None) -> str:
    # Guest marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|PASS|wheel_total=...|hwheel_total=...|...
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|FAIL|reason=...|wheel_total=...|hwheel_total=...|...
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|SKIP|flag_not_set
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|SKIP|not_observed|wheel_total=...|hwheel_total=...
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|SKIP|input_events_failed|reason=...|err=...|wheel_total=...|hwheel_total=...
    prefix = b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|SKIP|"
    prefix_str = "AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|SKIP|"
    marker = marker_line
    if marker is not None and not marker.startswith(prefix_str):
        marker = None
    if marker is None:
        marker = _try_extract_last_marker_line(tail, prefix)
    if marker is not None:
        fields = _parse_marker_kv_fields(marker)
        code = ""
        try:
            toks = marker.split("|")
            if toks and "SKIP" in toks:
                idx = toks.index("SKIP")
                if idx + 1 < len(toks):
                    tok = toks[idx + 1].strip()
                    if tok and "=" not in tok:
                        code = tok
        except Exception:
            code = code

        parts: list[str] = []
        if code:
            parts.append(code)
        # Surface additional details when present (notably `reason=`/`err=` for input_events_failed).
        for k in ("reason", "err", "wheel_total", "hwheel_total"):
            v = (fields.get(k) or "").strip()
            if v:
                parts.append(f"{k}={_sanitize_marker_value(v)}")
        details = ""
        if parts:
            details = " (" + " ".join(parts) + ")"

        if code == "flag_not_set":
            return (
                "FAIL: VIRTIO_INPUT_WHEEL_SKIPPED: virtio-input-wheel test was skipped (flag_not_set) but "
                "--with-input-wheel/--with-virtio-input-wheel/--require-virtio-input-wheel/--enable-virtio-input-wheel was enabled "
                "(provision the guest with --test-input-events)"
            )
        return (
            f"FAIL: VIRTIO_INPUT_WHEEL_SKIPPED: virtio-input-wheel test was skipped{details} but "
            "--with-input-wheel/--with-virtio-input-wheel/--require-virtio-input-wheel/--enable-virtio-input-wheel was enabled"
        )
    return (
        "FAIL: VIRTIO_INPUT_WHEEL_SKIPPED: virtio-input-wheel test was skipped but "
        "--with-input-wheel/--with-virtio-input-wheel/--require-virtio-input-wheel/--enable-virtio-input-wheel was enabled"
    )


def _virtio_input_wheel_fail_failure_message(tail: bytes, *, marker_line: Optional[str] = None) -> str:
    # Guest marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|FAIL|reason=missing_axis|wheel_total=...|hwheel_total=...|saw_wheel=...|saw_hwheel=...
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|FAIL|reason=unexpected_delta|wheel_total=...|hwheel_total=...|expected_wheel=...|expected_hwheel=...|wheel_events=...|hwheel_events=...|...
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|FAIL|reason=delta_mismatch|wheel_total=...|hwheel_total=...|expected_wheel=...|expected_hwheel=...|wheel_events=...|hwheel_events=...|...
    prefix = b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|FAIL|"
    prefix_str = "AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|FAIL|"
    marker = marker_line
    if marker is not None and not marker.startswith(prefix_str):
        marker = None
    if marker is None:
        marker = _try_extract_last_marker_line(tail, prefix)
    if marker is not None:
        fields = _parse_marker_kv_fields(marker)
        reason = (fields.get("reason") or "").strip()
        if not reason:
            # Backcompat: allow `...|FAIL|missing_axis|...` (no `reason=` field).
            try:
                toks = marker.split("|")
                if toks and "FAIL" in toks:
                    idx = toks.index("FAIL")
                    if idx + 1 < len(toks):
                        tok = toks[idx + 1].strip()
                        if tok and "=" not in tok:
                            reason = tok
            except Exception:
                reason = reason

        parts: list[str] = []
        if reason:
            parts.append(f"reason={_sanitize_marker_value(reason)}")
        for k in (
            "wheel_total",
            "hwheel_total",
            "expected_wheel",
            "expected_hwheel",
            "wheel_events",
            "hwheel_events",
            "saw_wheel",
            "saw_hwheel",
            "saw_wheel_expected",
            "saw_hwheel_expected",
            "wheel_unexpected_last",
            "hwheel_unexpected_last",
        ):
            v = (fields.get(k) or "").strip()
            if v:
                parts.append(f"{k}={_sanitize_marker_value(v)}")
        details = ""
        if parts:
            details = " (" + " ".join(parts) + ")"
        return (
            "FAIL: VIRTIO_INPUT_WHEEL_FAILED: virtio-input-wheel test reported FAIL while "
            f"--with-input-wheel/--with-virtio-input-wheel/--require-virtio-input-wheel/--enable-virtio-input-wheel was enabled{details}"
        )
    return (
        "FAIL: VIRTIO_INPUT_WHEEL_FAILED: virtio-input-wheel test reported FAIL while "
        "--with-input-wheel/--with-virtio-input-wheel/--require-virtio-input-wheel/--enable-virtio-input-wheel was enabled"
    )


def _virtio_blk_resize_skip_failure_message(tail: bytes, *, marker_line: Optional[str] = None) -> str:
    # Guest marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|READY|disk=<n>|old_bytes=<bytes>
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|PASS|disk=<n>|old_bytes=<bytes>|new_bytes=<bytes>|elapsed_ms=<ms>
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|FAIL|reason=<...>|disk=<n>|old_bytes=<bytes>|last_bytes=<bytes>|err=<win32>
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|SKIP|flag_not_set
    prefix = b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|SKIP|"
    prefix_str = "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|SKIP|"
    marker = marker_line
    if marker is not None and not marker.startswith(prefix_str):
        marker = None
    if marker is None:
        marker = _try_extract_last_marker_line(tail, prefix)
    if marker is not None:
        fields = _parse_marker_kv_fields(marker)
        reason = fields.get("reason")
        if not reason:
            # Backcompat: `...|SKIP|flag_not_set` (no `reason=` key).
            try:
                toks = marker.split("|")
                if toks and "SKIP" in toks:
                    idx = toks.index("SKIP")
                    if idx + 1 < len(toks):
                        tok = toks[idx + 1].strip()
                        if tok and "=" not in tok:
                            reason = tok
            except Exception:
                reason = reason
        if reason:
            if reason == "flag_not_set":
                return (
                    "FAIL: VIRTIO_BLK_RESIZE_SKIPPED: virtio-blk-resize test was skipped (flag_not_set) but "
                    "--with-blk-resize/--with-virtio-blk-resize/--require-virtio-blk-resize/--enable-virtio-blk-resize was enabled (provision the guest with --test-blk-resize)"
                )
            return (
                f"FAIL: VIRTIO_BLK_RESIZE_SKIPPED: virtio-blk-resize test was skipped ({reason}) but "
                "--with-blk-resize/--with-virtio-blk-resize/--require-virtio-blk-resize/--enable-virtio-blk-resize was enabled"
            )
    return "FAIL: VIRTIO_BLK_RESIZE_SKIPPED: virtio-blk-resize test was skipped but --with-blk-resize/--with-virtio-blk-resize/--require-virtio-blk-resize/--enable-virtio-blk-resize was enabled"


def _virtio_blk_resize_fail_failure_message(tail: bytes, *, marker_line: Optional[str] = None) -> str:
    # Guest marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|FAIL|reason=<...>|disk=<n>|old_bytes=<bytes>|last_bytes=<bytes>|err=<win32>
    prefix = b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|FAIL|"
    prefix_str = "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|FAIL|"
    marker = marker_line
    if marker is not None and not marker.startswith(prefix_str):
        marker = None
    if marker is None:
        marker = _try_extract_last_marker_line(tail, prefix)
    if marker is not None:
        fields = _parse_marker_kv_fields(marker)
        reason = (fields.get("reason") or "").strip()
        if not reason:
            # Backcompat: allow `...|FAIL|timeout|err=...` (no `reason=` key).
            try:
                toks = marker.split("|")
                if toks and "FAIL" in toks:
                    idx = toks.index("FAIL")
                    if idx + 1 < len(toks):
                        tok = toks[idx + 1].strip()
                        if tok and "=" not in tok:
                            reason = tok
            except Exception:
                reason = reason
        parts: list[str] = []
        if reason:
            parts.append(f"reason={_sanitize_marker_value(reason)}")
        for k in ("disk", "old_bytes", "last_bytes", "err"):
            v = (fields.get(k) or "").strip()
            if v:
                parts.append(f"{k}={_sanitize_marker_value(v)}")
        details = ""
        if parts:
            details = " (" + " ".join(parts) + ")"
        return (
            "FAIL: VIRTIO_BLK_RESIZE_FAILED: virtio-blk-resize test reported FAIL while "
            f"--with-blk-resize/--with-virtio-blk-resize/--require-virtio-blk-resize/--enable-virtio-blk-resize was enabled{details}"
        )
    return (
        "FAIL: VIRTIO_BLK_RESIZE_FAILED: virtio-blk-resize test reported FAIL while "
        "--with-blk-resize/--with-virtio-blk-resize/--require-virtio-blk-resize/--enable-virtio-blk-resize was enabled"
    )


def _virtio_blk_reset_required_failure_message(
    tail: bytes,
    *,
    saw_pass: bool = False,
    saw_fail: bool = False,
    saw_skip: bool = False,
    marker_line: Optional[str] = None,
) -> Optional[str]:
    """
    Enforce that virtio-blk-reset ran and PASSed.

    Returns:
        A "FAIL: ..." message on failure, or None when the marker requirements are satisfied.
    """
    # Prefer an explicit marker line when available (survives tail truncation).
    if marker_line is not None:
        status = _try_extract_marker_status(marker_line)
        if status == "PASS":
            return None
        if status == "FAIL":
            return _virtio_blk_reset_fail_failure_message(tail, marker_line=marker_line)
        if status == "SKIP":
            return _virtio_blk_reset_skip_failure_message(tail, marker_line=marker_line)

    # Prefer explicit "saw_*" flags tracked by the main harness loop (these survive tail truncation),
    # but keep a tail scan fallback to support direct unit tests (and any legacy call sites).
    if saw_pass or b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|PASS" in tail:
        return None
    if saw_fail or b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|FAIL" in tail:
        return _virtio_blk_reset_fail_failure_message(tail, marker_line=marker_line)
    if saw_skip or b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|SKIP" in tail:
        return _virtio_blk_reset_skip_failure_message(tail, marker_line=marker_line)
    return _virtio_blk_reset_missing_failure_message()


def _virtio_input_binding_required_failure_message(
    tail: bytes,
    *,
    saw_pass: bool = False,
    saw_fail: bool = False,
    saw_skip: bool = False,
    marker_line: Optional[str] = None,
) -> Optional[str]:
    """
    Enforce that virtio-input PCI binding validation ran and PASSed.

    Returns:
        A "FAIL: ..." message on failure, or None when the marker requirements are satisfied.
    """
    prefix = b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|"
    # Prefer the incrementally captured last marker line when available so we don't depend on the rolling
    # tail buffer still containing the marker.
    if marker_line is not None:
        st = _try_extract_marker_status(marker_line)
        if st == "PASS":
            return None
        if st == "SKIP":
            return (
                "FAIL: VIRTIO_INPUT_BINDING_SKIPPED: virtio-input-binding marker reported SKIP while "
                "--require-virtio-input-binding was enabled (guest selftest too old?)"
            )
        if st == "FAIL":
            fields = _parse_marker_kv_fields(marker_line)
            reason = fields.get("reason") or "unknown"
            expected = fields.get("expected") or ""
            actual = fields.get("actual") or ""

            details = f"reason={reason}"
            if expected:
                details += f" expected={expected}"
            if actual:
                details += f" actual={actual}"
            return (
                "FAIL: VIRTIO_INPUT_BINDING_FAILED: virtio-input-binding marker reported FAIL while "
                f"--require-virtio-input-binding was enabled ({details})"
            )

    # Fall back to saw flags tracked by the main harness loop (survive tail truncation), with a tail-scan
    # fallback for unit tests / legacy call sites.
    if saw_pass or b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|PASS" in tail:
        return None

    if saw_fail or b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|FAIL" in tail:
        marker_line2 = _try_extract_last_marker_line(tail, prefix)
        reason = "unknown"
        expected = ""
        actual = ""
        if marker_line2 is not None:
            fields = _parse_marker_kv_fields(marker_line2)
            reason = fields.get("reason") or reason
            expected = fields.get("expected") or ""
            actual = fields.get("actual") or ""

        details = f"reason={reason}"
        if expected:
            details += f" expected={expected}"
        if actual:
            details += f" actual={actual}"
        return (
            "FAIL: VIRTIO_INPUT_BINDING_FAILED: virtio-input-binding marker reported FAIL while "
            f"--require-virtio-input-binding was enabled ({details})"
        )

    if saw_skip or b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|SKIP" in tail:
        return (
            "FAIL: VIRTIO_INPUT_BINDING_SKIPPED: virtio-input-binding marker reported SKIP while "
            "--require-virtio-input-binding was enabled (guest selftest too old?)"
        )

    return (
        "FAIL: MISSING_VIRTIO_INPUT_BINDING: did not observe virtio-input-binding PASS marker while "
        "--require-virtio-input-binding was enabled (guest selftest too old?)"
    )


def _virtio_input_leds_fail_failure_message(tail: bytes, *, marker_line: Optional[str] = None) -> str:
    # Guest marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|FAIL|reason=<...>|err=<win32>|writes=<n>
    prefix = b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|FAIL|"
    prefix_str = "AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|FAIL|"
    marker = marker_line
    if marker is not None and not marker.startswith(prefix_str):
        marker = None
    if marker is None:
        marker = _try_extract_last_marker_line(tail, prefix)
    if marker is not None:
        fields = _parse_marker_kv_fields(marker)
        reason = (fields.get("reason") or "").strip()
        if not reason:
            # Backcompat: allow token-only FAIL markers (no `reason=` field).
            try:
                toks = marker.split("|")
                if toks and "FAIL" in toks:
                    idx = toks.index("FAIL")
                    if idx + 1 < len(toks):
                        tok = toks[idx + 1].strip()
                        if tok and "=" not in tok:
                            reason = tok
            except Exception:
                reason = reason
        parts: list[str] = []
        if reason:
            parts.append(f"reason={_sanitize_marker_value(reason)}")
        for k in ("err", "writes"):
            v = (fields.get(k) or "").strip()
            if v:
                parts.append(f"{k}={_sanitize_marker_value(v)}")
        details = ""
        if parts:
            details = " (" + " ".join(parts) + ")"
        return (
            "FAIL: VIRTIO_INPUT_LEDS_FAILED: virtio-input-leds test reported FAIL while --with-input-leds/--with-virtio-input-leds/--require-virtio-input-leds/--enable-virtio-input-leds was enabled"
            + details
        )
    return (
        "FAIL: VIRTIO_INPUT_LEDS_FAILED: virtio-input-leds test reported FAIL while --with-input-leds/--with-virtio-input-leds/--require-virtio-input-leds/--enable-virtio-input-leds was enabled"
    )


def _virtio_input_leds_required_failure_message(
    tail: bytes,
    *,
    saw_pass: bool = False,
    saw_fail: bool = False,
    saw_skip: bool = False,
    marker_line: Optional[str] = None,
) -> Optional[str]:
    """
    Enforce that virtio-input-leds ran and PASSed.

    Returns:
        A "FAIL: ..." message on failure, or None when the marker requirements are satisfied.
    """
    # Prefer an explicit marker line when available (survives tail truncation).
    if marker_line is not None:
        st = _try_extract_marker_status(marker_line)
        if st == "PASS":
            return None
        if st == "FAIL":
            return _virtio_input_leds_fail_failure_message(tail, marker_line=marker_line)
        if st == "SKIP":
            marker = marker_line
            prefix2 = "AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|SKIP|"
            if marker.startswith(prefix2):
                reason = marker[len(prefix2) :].strip()
                if reason:
                    return (
                        f"FAIL: VIRTIO_INPUT_LEDS_SKIPPED: virtio-input-leds test was skipped ({reason}) "
                        "but --with-input-leds/--with-virtio-input-leds/--require-virtio-input-leds/--enable-virtio-input-leds was enabled (provision the guest with --test-input-leds; "
                        "newer guest selftests also accept --test-input-led)"
                    )

    # Prefer explicit "saw_*" flags tracked by the main harness loop (these survive tail truncation),
    # but keep a tail scan fallback to support direct unit tests (and any legacy call sites).
    if saw_pass or b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|PASS" in tail:
        return None
    if saw_fail or b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|FAIL" in tail:
        return _virtio_input_leds_fail_failure_message(tail, marker_line=marker_line)
    if saw_skip or b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|SKIP" in tail:
        marker = _try_extract_last_marker_line(
            tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|SKIP|"
        )
        if marker is not None:
            prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|SKIP|"
            reason = marker[len(prefix) :].strip()
            if reason:
                return (
                    f"FAIL: VIRTIO_INPUT_LEDS_SKIPPED: virtio-input-leds test was skipped ({reason}) "
                    "but --with-input-leds/--with-virtio-input-leds/--require-virtio-input-leds/--enable-virtio-input-leds was enabled (provision the guest with --test-input-leds; "
                    "newer guest selftests also accept --test-input-led)"
                )
        return (
            "FAIL: VIRTIO_INPUT_LEDS_SKIPPED: virtio-input-leds test was skipped but --with-input-leds/--with-virtio-input-leds/--require-virtio-input-leds/--enable-virtio-input-leds was enabled "
            "(provision the guest with --test-input-leds; newer guest selftests also accept --test-input-led)"
        )
    return (
        "FAIL: MISSING_VIRTIO_INPUT_LEDS: did not observe virtio-input-leds PASS marker while --with-input-leds/--with-virtio-input-leds/--require-virtio-input-leds/--enable-virtio-input-leds was enabled "
        "(guest selftest too old or missing --test-input-leds/--test-input-led)"
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


def _qemu_has_device_strict(qemu_system: str, device_name: str) -> bool:
    """
    Like `_qemu_has_device`, but re-raise when the qemu-system binary itself cannot be executed.

    `_qemu_has_device` is used for optional feature probing and intentionally treats any QEMU probe
    failure as "device not present". For required-device validations we want missing/broken QEMU to
    surface as a clear error (instead of being misreported as missing virtio device support).
    """

    try:
        _qemu_device_help_text(qemu_system, device_name)
        return True
    except RuntimeError as e:
        msg = str(e)
        if msg.startswith("qemu-system binary not found:") or msg.startswith("failed to run '"):
            raise
        return False


_QEMU_DEVICE_VECTORS_RE = re.compile(r"(?m)^\s*vectors\b")


def _assert_qemu_devices_support_vectors_property(
    qemu_system: str, device_names: list[str], *, requested_by: str
) -> None:
    """
    Fail fast with a clear error if the user requested `vectors=` tuning (including `vectors=0`)
    but the running QEMU binary does not expose the `vectors` property for one or more devices.
    """

    missing: list[str] = []
    for device_name in device_names:
        help_text = _qemu_device_help_text(qemu_system, device_name)
        if not _QEMU_DEVICE_VECTORS_RE.search(help_text):
            missing.append(device_name)

    if not missing:
        return

    missing_str = ", ".join(missing)
    raise RuntimeError(
        f"QEMU device(s) do not expose the 'vectors' property: {missing_str}. "
        f"The harness was configured to set virtio 'vectors=' ({requested_by}), but this QEMU build does not support it. "
        "Disable the flag or upgrade QEMU."
    )


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
        "Upgrade QEMU or omit --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd and pass custom QEMU args."
    )


def _resolve_executable_path(cmd: str) -> str:
    """
    Best-effort resolve an executable path for logging/debuggability.

    - If the user passed a path (contains a path separator), resolve it to an absolute path.
    - Otherwise, try PATH lookup via `shutil.which`.
    - If resolution fails, return the original string unchanged.
    """
    if not cmd:
        return cmd

    try:
        has_sep = os.sep in cmd or (os.altsep is not None and os.altsep in cmd)
    except Exception:
        has_sep = False

    if has_sep:
        try:
            return str(Path(cmd).resolve())
        except Exception:
            return cmd

    found = shutil.which(cmd)
    if found:
        try:
            return str(Path(found).resolve())
        except Exception:
            return found

    return cmd


def _build_qemu_args_dry_run(
    args: argparse.Namespace,
    qemu_extra: list[str],
    *,
    disk_image: Path,
    serial_log: Path,
    qmp_endpoint: Optional[_QmpEndpoint],
    virtio_net_vectors: Optional[int],
    virtio_blk_vectors: Optional[int],
    virtio_input_vectors: Optional[int],
    virtio_snd_vectors: Optional[int],
    attach_virtio_tablet: bool,
    virtio_disable_msix: bool,
) -> list[str]:
    """
    Construct the QEMU argv array without probing QEMU (no subprocesses).

    This is used by the host harness dry-run mode to dump the intended QEMU commandline
    without starting QEMU or the harness HTTP server.
    """
    serial_chardev = f"file,id=charserial0,path={_qemu_quote_keyval_value(str(serial_log))}"

    qemu_args: list[str] = [
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
    ]

    if args.virtio_transitional:
        # Transitional mode: use a close-to-default QEMU layout. If explicit vectors are requested
        # for virtio-blk (or INTx-only mode is requested), switch to an explicit virtio-blk-pci device so
        # `vectors=<N>` / `vectors=0` can be applied.
        virtio_blk_args: list[str] = []
        if virtio_blk_vectors is None and not virtio_disable_msix:
            drive = f"file={_qemu_quote_keyval_value(str(disk_image))},if=virtio,cache=writeback"
            if args.snapshot:
                drive += ",snapshot=on"
            virtio_blk_args = ["-drive", drive]
        else:
            drive_id = "drive0"
            drive = f"file={_qemu_quote_keyval_value(str(disk_image))},if=none,id={drive_id},cache=writeback"
            if args.snapshot:
                drive += ",snapshot=on"
            virtio_blk = _qemu_device_arg_add_vectors(f"virtio-blk-pci,drive={drive_id}", virtio_blk_vectors)
            virtio_blk = _qemu_device_arg_disable_msix(virtio_blk, virtio_disable_msix)
            virtio_blk_args = ["-drive", drive, "-device", virtio_blk]

        # Mirror the stable QOM `id=` used by the real harness run so QMP features like `set_link`
        # (net link flap) can target the device consistently even when users copy/paste the dry-run
        # commandline for debugging.
        virtio_net = _qemu_device_arg_add_vectors(
            f"virtio-net-pci,id={_VIRTIO_NET_QMP_ID},netdev=net0",
            virtio_net_vectors,
        )
        virtio_net = _qemu_device_arg_disable_msix(virtio_net, virtio_disable_msix)

        kbd = _qemu_device_arg_add_vectors(
            f"virtio-keyboard-pci,id={_VIRTIO_INPUT_QMP_KEYBOARD_ID}",
            virtio_input_vectors,
        )
        kbd = _qemu_device_arg_disable_msix(kbd, virtio_disable_msix)
        mouse = _qemu_device_arg_add_vectors(
            f"virtio-mouse-pci,id={_VIRTIO_INPUT_QMP_MOUSE_ID}",
            virtio_input_vectors,
        )
        mouse = _qemu_device_arg_disable_msix(mouse, virtio_disable_msix)
        virtio_input_args: list[str] = [
            "-device",
            kbd,
            "-device",
            mouse,
        ]
        if attach_virtio_tablet:
            tablet = _qemu_device_arg_add_vectors(
                _qemu_virtio_tablet_pci_device_arg(disable_legacy=False, pci_revision=None),
                virtio_input_vectors,
            )
            tablet = _qemu_device_arg_disable_msix(tablet, virtio_disable_msix)
            virtio_input_args += [
                "-device",
                tablet,
            ]

        qemu_args += ["-device", virtio_net] + virtio_input_args + virtio_blk_args + qemu_extra
        return qemu_args

    # Contract v1: modern-only virtio-pci + forced PCI revision.
    aero_pci_rev = "0x01"
    drive_id = "drive0"
    drive = f"file={_qemu_quote_keyval_value(str(disk_image))},if=none,id={drive_id},cache=writeback"
    if args.snapshot:
        drive += ",snapshot=on"

    virtio_net = _qemu_device_arg_add_vectors(
        f"virtio-net-pci,id={_VIRTIO_NET_QMP_ID},netdev=net0,disable-legacy=on,x-pci-revision={aero_pci_rev}",
        virtio_net_vectors,
    )
    virtio_net = _qemu_device_arg_disable_msix(virtio_net, virtio_disable_msix)
    virtio_blk = _qemu_device_arg_add_vectors(
        f"virtio-blk-pci,drive={drive_id},disable-legacy=on,x-pci-revision={aero_pci_rev}",
        virtio_blk_vectors,
    )
    virtio_blk = _qemu_device_arg_disable_msix(virtio_blk, virtio_disable_msix)
    virtio_kbd = _qemu_device_arg_add_vectors(
        f"virtio-keyboard-pci,id={_VIRTIO_INPUT_QMP_KEYBOARD_ID},disable-legacy=on,x-pci-revision={aero_pci_rev}",
        virtio_input_vectors,
    )
    virtio_kbd = _qemu_device_arg_disable_msix(virtio_kbd, virtio_disable_msix)
    virtio_mouse = _qemu_device_arg_add_vectors(
        f"virtio-mouse-pci,id={_VIRTIO_INPUT_QMP_MOUSE_ID},disable-legacy=on,x-pci-revision={aero_pci_rev}",
        virtio_input_vectors,
    )
    virtio_mouse = _qemu_device_arg_disable_msix(virtio_mouse, virtio_disable_msix)

    qemu_args += [
        "-device",
        virtio_net,
        "-device",
        virtio_kbd,
        "-device",
        virtio_mouse,
    ]
    if attach_virtio_tablet:
        virtio_tablet = _qemu_device_arg_add_vectors(
            _qemu_virtio_tablet_pci_device_arg(disable_legacy=True, pci_revision=aero_pci_rev),
            virtio_input_vectors,
        )
        virtio_tablet = _qemu_device_arg_disable_msix(virtio_tablet, virtio_disable_msix)
        qemu_args += ["-device", virtio_tablet]

    qemu_args += [
        "-drive",
        drive,
        "-device",
        virtio_blk,
    ]

    if args.enable_virtio_snd:
        device_arg = ",".join(
            [
                "virtio-sound-pci",
                "audiodev=snd0",
                "disable-legacy=on",
                f"x-pci-revision={aero_pci_rev}",
            ]
        )
        device_arg = _qemu_device_arg_add_vectors(device_arg, virtio_snd_vectors)
        device_arg = _qemu_device_arg_disable_msix(device_arg, virtio_disable_msix)

        backend = args.virtio_snd_audio_backend
        if backend == "none":
            audiodev_arg = "none,id=snd0"
        elif backend == "wav":
            wav_path = Path(args.virtio_snd_wav_path).resolve()
            audiodev_arg = f"wav,id=snd0,path={_qemu_quote_keyval_value(str(wav_path))}"
        else:
            raise AssertionError(f"Unhandled backend: {backend}")

        qemu_args += ["-audiodev", audiodev_arg, "-device", device_arg]

    qemu_args += qemu_extra
    return qemu_args


def _format_commandline_for_host(argv: list[str]) -> str:
    """
    Best-effort copy/paste commandline string for the current host platform.

    This is for logs/debugging only; the harness always executes QEMU via an argv array.
    """
    if os.name == "nt":
        return subprocess.list2cmdline([str(a) for a in argv])
    return " ".join(shlex.quote(str(a)) for a in argv)


def _build_arg_parser() -> argparse.ArgumentParser:
    # Use `allow_abbrev=False` so QEMU passthrough args (unknown to the harness) cannot be
    # accidentally consumed as abbreviated harness flags. This avoids surprising behavior when
    # users append additional QEMU options and also makes the CLI surface stable as new flags are
    # added (argparse's abbreviation matching can become ambiguous over time).
    parser = argparse.ArgumentParser(allow_abbrev=False)
    parser.add_argument("--qemu-system", required=True, help="Path to qemu-system-* binary")
    parser.add_argument("--disk-image", required=True, help="Prepared Win7 disk image")
    parser.add_argument(
        "--dry-run",
        "--print-qemu",
        "--print-qemu-cmd",
        dest="dry_run",
        action="store_true",
        help=(
            "Construct and print the full QEMU argv (JSON + shell-escaped single line) and exit 0. "
            "Does not start the HTTP server, QMP, or QEMU."
        ),
    )
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
            "Must start with '/' and must not contain whitespace. "
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
        "--with-blk-reset",
        "--with-virtio-blk-reset",
        "--require-virtio-blk-reset",
        "--enable-virtio-blk-reset",
        dest="with_blk_reset",
        action="store_true",
        help=(
            "Require the guest virtio-blk-reset marker to PASS (treat FAIL/SKIP/missing as failure). "
            "This requires a guest image provisioned with --test-blk-reset "
            "(or env var AERO_VIRTIO_SELFTEST_TEST_BLK_RESET=1)."
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
            "Inject deterministic keyboard/mouse events via QMP (prefers input-send-event, with backcompat fallbacks when unavailable) "
            "and require the guest "
            "virtio-input-events selftest marker to PASS. Also emits a host marker: "
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_EVENTS_INJECT|PASS/FAIL|attempt=<n>|backend=<qmp_input_send_event|hmp_fallback|unknown>|kbd_mode=device/broadcast|mouse_mode=device/broadcast "
            "(may appear multiple times due to retries). "
            "This requires a guest image provisioned with --test-input-events (or env var)."
        ),
    )
    parser.add_argument(
        "--with-input-leds",
        "--with-virtio-input-leds",
        "--require-virtio-input-leds",
        "--enable-virtio-input-leds",
        dest="with_input_leds",
        action="store_true",
        help=(
            "Require the guest virtio-input-leds selftest marker to PASS. This validates the virtio-input statusq output path "
            "end-to-end (user-mode HID output report write -> KMDF HID minidriver -> virtqueue). "
            "This requires a guest image provisioned with --test-input-leds (or env var). "
            "Newer guest selftests also accept --test-input-led and emit the legacy marker for compatibility."
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
            "Inject deterministic Consumer Control (media key) events via QMP (prefers input-send-event, with backcompat fallbacks when unavailable) and require the guest "
            "virtio-input-media-keys selftest marker to PASS. Also emits a host marker: "
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_MEDIA_KEYS_INJECT|PASS/FAIL|attempt=<n>|backend=<qmp_input_send_event|hmp_fallback|unknown>|kbd_mode=device/broadcast "
            "(may appear multiple times due to retries). "
            "This requires a guest image provisioned with --test-input-media-keys (or env var)."
        ),
    )
    parser.add_argument(
        "--with-input-led",
        "--with-virtio-input-led",
        "--require-virtio-input-led",
        "--enable-virtio-input-led",
        dest="with_input_led",
        action="store_true",
        help=(
            "Require the guest virtio-input-led (keyboard LED/statusq) marker to PASS. "
            "This requires a guest image provisioned with --test-input-led (or env var: AERO_VIRTIO_SELFTEST_TEST_INPUT_LED=1)."
        ),
    )
    parser.add_argument(
        "--with-input-wheel",
        "--with-virtio-input-wheel",
        "--require-virtio-input-wheel",
        "--enable-virtio-input-wheel",
        dest="with_input_wheel",
        action="store_true",
        help=(
            "When injecting virtio-input events, also inject vertical + horizontal scroll wheel events "
            "(QMP rel axes: wheel/vscroll + hscroll/hwheel; the harness retries common axis name fallbacks) and "
            "require the guest virtio-input-wheel marker to PASS. "
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
            "QMP input-send-event. Require the guest virtio-input-tablet-events selftest marker to PASS. "
            "Also emits a host marker: "
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_TABLET_EVENTS_INJECT|PASS/FAIL|attempt=<n>|backend=<qmp_input_send_event|unknown>|tablet_mode=device/broadcast "
            "(may appear multiple times due to retries). "
            "This requires a guest image provisioned with --test-input-tablet-events/--test-tablet-events "
            "(or env var: AERO_VIRTIO_SELFTEST_TEST_INPUT_TABLET_EVENTS=1 or AERO_VIRTIO_SELFTEST_TEST_TABLET_EVENTS=1)."
        ),
    )
    parser.add_argument(
        "--with-net-link-flap",
        "--with-virtio-net-link-flap",
        "--require-virtio-net-link-flap",
        "--enable-virtio-net-link-flap",
        dest="with_net_link_flap",
        action="store_true",
        help=(
            "After the guest emits AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|READY, "
            "toggle virtio-net link down/up via QMP set_link and require the guest virtio-net-link-flap "
            "selftest marker to PASS. This requires a guest image provisioned with --test-net-link-flap "
            "(or env var)."
        ),
    )
    parser.add_argument(
        "--with-blk-resize",
        "--with-virtio-blk-resize",
        "--require-virtio-blk-resize",
        "--enable-virtio-blk-resize",
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
        "--require-net-msix",
        dest="require_virtio_net_msix",
        action="store_true",
        help=(
            "Require virtio-net to run with MSI-X enabled. This performs a host-side MSI-X enable check via QMP "
            "and also requires the guest marker: "
            "AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|PASS|mode=msix|... "
            "This requires a guest image with an updated aero-virtio-selftest.exe; "
            "to make the guest fail-fast, provision the guest selftest with the guest flag --require-net-msix "
            "(or env var AERO_VIRTIO_SELFTEST_REQUIRE_NET_MSIX=1). "
            "If provisioning via New-AeroWin7TestImage.ps1, use -RequireNetMsix."
        ),
    )
    parser.add_argument(
        "--require-virtio-blk-msix",
        "--require-blk-msix",
        action="store_true",
        help=(
            "Require virtio-blk to run with MSI-X enabled. This performs a host-side MSI-X enable check via QMP "
            "and also requires the guest marker: "
            "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-msix|PASS|mode=msix|... "
            "Tip: to make the guest fail-fast when virtio-blk is not using MSI/MSI-X, provision it with "
            "--expect-blk-msi (or env var AERO_VIRTIO_SELFTEST_EXPECT_BLK_MSI=1); "
            "when provisioning via New-AeroWin7TestImage.ps1, use -ExpectBlkMsi."
        ),
    )
    parser.add_argument(
        "--require-virtio-snd-msix",
        "--require-snd-msix",
        action="store_true",
        help=(
            "Require virtio-snd to run with MSI-X enabled. This performs a host-side MSI-X enable check via QMP "
            "and also requires the guest marker: "
            "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|PASS|mode=msix|... "
            "Tip: to make the guest fail-fast, provision the guest selftest with the guest flag --require-snd-msix "
            "(or env var AERO_VIRTIO_SELFTEST_REQUIRE_SND_MSIX=1); when provisioning via "
            "New-AeroWin7TestImage.ps1, use -RequireSndMsix. "
            "(this option requires --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd)."
        ),
    )
    parser.add_argument(
        "--require-virtio-input-msix",
        "--require-input-msix",
        dest="require_virtio_input_msix",
        action="store_true",
        help=(
            "Require the guest virtio-input-msix marker to report mode=msix. "
            "This is optional so older guest selftest binaries (which don't emit the marker) can still run. "
            "Tip: to make the guest fail-fast, provision the guest selftest with the guest flag --require-input-msix "
            "(or env var AERO_VIRTIO_SELFTEST_REQUIRE_INPUT_MSIX=1); when provisioning via "
            "New-AeroWin7TestImage.ps1, use -RequireInputMsix."
        ),
    )
    parser.add_argument(
        "--require-virtio-input-binding",
        dest="require_virtio_input_binding",
        action="store_true",
        help=(
            "Require the guest virtio-input-binding marker to PASS (ensures at least one virtio-input PCI device is "
            "present and bound to the expected Aero driver service)."
        ),
    )
    parser.add_argument(
        "--require-net-csum-offload",
        "--require-virtio-net-csum-offload",
        action="store_true",
        help=(
            "Require at least one checksum-offloaded packet from the virtio-net driver. "
            "This checks the guest marker: "
            "AERO_VIRTIO_SELFTEST|TEST|virtio-net-offload-csum|PASS|tx_csum=... "
            "(fails if the marker is missing/FAIL or tx_csum=0)."
        ),
    )
    parser.add_argument(
        "--require-net-udp-csum-offload",
        "--require-virtio-net-udp-csum-offload",
        action="store_true",
        help=(
            "Require at least one UDP checksum-offloaded TX packet from the virtio-net driver. "
            "This checks the guest marker: "
            "AERO_VIRTIO_SELFTEST|TEST|virtio-net-offload-csum|PASS|tx_udp=... "
            "(fails if the marker is missing/FAIL, missing tx_udp (and tx_udp4/tx_udp6) fields, or tx_udp=0)."
        ),
    )
    parser.add_argument(
        "--with-virtio-snd",
        "--require-virtio-snd",
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
        "--require-virtio-snd-buffer-limits",
        "--enable-snd-buffer-limits",
        "--enable-virtio-snd-buffer-limits",
        dest="with_snd_buffer_limits",
        action="store_true",
        help=(
            "Require the guest virtio-snd-buffer-limits stress test marker to PASS. "
            "This requires a guest image provisioned with --test-snd-buffer-limits "
            "(or env var AERO_VIRTIO_SELFTEST_TEST_SND_BUFFER_LIMITS=1) and also requires "
            "--with-virtio-snd/--require-virtio-snd/--enable-virtio-snd so a virtio-snd device is attached."
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
            "(requires --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd and --virtio-snd-audio-backend=wav)"
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
    # NOTE: `--with-virtio-input-events`/`--require-virtio-input-events`/`--enable-virtio-input-events` used to be separate flags; they remain accepted
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
            "Requires a QEMU build that supports the 'vectors' property (the harness fails fast if unsupported). "
            "Typical values: 2, 4, 8. Windows may still allocate fewer messages; drivers fall back. "
            "Disabled by default."
        ),
    )
    parser.add_argument(
        "--virtio-disable-msix",
        "--force-intx",
        "--intx-only",
        action="store_true",
        help=(
            "Disable MSI-X for virtio-pci devices created by the harness (virtio-net/blk/input/snd) "
            "so Windows 7 must use legacy INTx + ISR paths. This appends ',vectors=0' to each virtio "
            "`-device` arg. Requires a QEMU build that supports the virtio 'vectors' property and "
            "accepts `vectors=0`. Aliases: --force-intx/--intx-only."
        ),
    )
    parser.add_argument(
        "--virtio-net-vectors",
        "--virtio-net-msix-vectors",
        type=int,
        default=None,
        metavar="N",
        help=(
            "Override virtio-net MSI-X vectors via `-device virtio-net-pci,...,vectors=N` "
            "(requires QEMU virtio `vectors` property)."
        ),
    )
    parser.add_argument(
        "--virtio-blk-vectors",
        "--virtio-blk-msix-vectors",
        type=int,
        default=None,
        metavar="N",
        help=(
            "Override virtio-blk MSI-X vectors via `-device virtio-blk-pci,...,vectors=N` "
            "(requires QEMU virtio `vectors` property)."
        ),
    )
    parser.add_argument(
        "--virtio-snd-vectors",
        "--virtio-snd-msix-vectors",
        type=int,
        default=None,
        metavar="N",
        help=(
            "Override virtio-snd MSI-X vectors via `-device virtio-snd-pci,...,vectors=N` "
            "(requires QEMU virtio `vectors` property and --with-virtio-snd)."
        ),
    )
    parser.add_argument(
        "--virtio-input-vectors",
        "--virtio-input-msix-vectors",
        type=int,
        default=None,
        metavar="N",
        help=(
            "Override virtio-input MSI-X vectors via `-device virtio-*-pci,...,vectors=N` "
            "(requires QEMU virtio `vectors` property)."
        ),
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
    parser.add_argument(
        "--require-expect-blk-msi",
        dest="require_expect_blk_msi",
        action="store_true",
        help=(
            "Fail deterministically if the guest selftest was not provisioned with "
            "--expect-blk-msi (i.e. CONFIG marker expect_blk_msi=1). "
            "Useful for MSI/MSI-X-specific CI to catch mis-provisioned images "
            "(re-provision with New-AeroWin7TestImage.ps1 -ExpectBlkMsi)."
        ),
    )
    parser.add_argument(
        "--require-no-blk-recovery",
        action="store_true",
        help=(
            "Fail the harness if the virtio-blk miniport reports any abort/reset/PnP/IOCTL-reset activity "
            "(via abort_srb/reset_device_srb/reset_bus_srb/pnp_srb/ioctl_reset counters) when available from either:\n"
            "  - legacy fields on the guest virtio-blk test marker, or\n"
            "  - the dedicated virtio-blk-counters marker.\n"
            "On failure, emits: "
            "FAIL: VIRTIO_BLK_RECOVERY_NONZERO:"
        ),
    )
    parser.add_argument(
        "--fail-on-blk-recovery",
        action="store_true",
        help=(
            "Fail the harness if the guest reports non-zero virtio-blk recovery/reset activity "
            "(checks abort/reset_device/reset_bus only). "
            "This prefers the dedicated virtio-blk-counters marker; if it is missing entirely, "
            "falls back to legacy abort_srb/reset_*_srb fields on the virtio-blk marker. "
            "If virtio-blk-counters is present but SKIP, counters are treated as unavailable."
        ),
    )
    parser.add_argument(
        "--require-no-blk-reset-recovery",
        action="store_true",
        help=(
            "Fail the harness if the guest reports non-zero virtio-blk timeout/error recovery activity "
            "(checks reset_detected/hw_reset_bus). "
            "This prefers the dedicated virtio-blk-reset-recovery marker; if it is missing entirely, "
            "falls back to the legacy miniport diagnostic line virtio-blk-miniport-reset-recovery|INFO|... "
            "(WARN/SKIP treated as unavailable). "
            "On failure, emits: FAIL: VIRTIO_BLK_RESET_RECOVERY_NONZERO:"
        ),
    )
    parser.add_argument(
        "--fail-on-blk-reset-recovery",
        action="store_true",
        help=(
            "Fail the harness if the guest reports non-zero virtio-blk reset activity "
            "(checks hw_reset_bus only). "
            "This prefers the dedicated virtio-blk-reset-recovery marker; if it is missing entirely, "
            "falls back to the legacy miniport diagnostic line virtio-blk-miniport-reset-recovery|INFO|... "
            "(WARN/SKIP treated as unavailable)."
        ),
    )
    parser.add_argument(
        "--require-no-blk-miniport-flags",
        action="store_true",
        help=(
            "Fail the harness if the guest virtio-blk miniport flags diagnostic reports any non-zero "
            "removed/surprise_removed/reset_in_progress/reset_pending bits (best-effort; ignores missing/WARN markers). "
            "On failure, emits: FAIL: VIRTIO_BLK_MINIPORT_FLAGS_NONZERO:"
        ),
    )
    parser.add_argument(
        "--fail-on-blk-miniport-flags",
        action="store_true",
        help=(
            "Fail the harness if the guest virtio-blk miniport flags diagnostic reports removal "
            "activity (removed or surprise_removed set). This is a looser subset of "
            "--require-no-blk-miniport-flags and ignores reset_in_progress/reset_pending."
        ),
    )

    return parser


def main() -> int:
    parser = _build_arg_parser()

    # Any remaining args are passed directly to QEMU.
    args, qemu_extra = parser.parse_known_args()
    args.qemu_system = _resolve_executable_path(args.qemu_system)
    if args.qemu_system:
        try:
            has_sep = os.sep in args.qemu_system or (os.altsep is not None and os.altsep in args.qemu_system)
        except Exception:
            has_sep = False
        if has_sep:
            try:
                qemu_path = Path(args.qemu_system)
                if qemu_path.exists() and qemu_path.is_dir():
                    print(f"ERROR: qemu system binary path is a directory: {qemu_path}", file=sys.stderr)
                    return 2
            except Exception:
                pass
    need_blk_reset = bool(getattr(args, "with_blk_reset", False))
    need_input_wheel = bool(getattr(args, "with_input_wheel", False))
    need_input_events_extended = bool(getattr(args, "with_input_events_extended", False))
    need_input_events = bool(args.with_input_events) or need_input_wheel or need_input_events_extended
    need_input_leds = bool(getattr(args, "with_input_leds", False))
    need_input_media_keys = bool(getattr(args, "with_input_media_keys", False))
    need_input_led = bool(getattr(args, "with_input_led", False))
    need_input_tablet_events = bool(getattr(args, "with_input_tablet_events", False))
    attach_virtio_tablet = bool(args.with_virtio_tablet or need_input_tablet_events)
    need_blk_resize = bool(getattr(args, "with_blk_resize", False))
    need_net_link_flap = bool(getattr(args, "with_net_link_flap", False))
    need_msix_check = bool(
        args.require_virtio_net_msix or args.require_virtio_blk_msix or args.require_virtio_snd_msix
    )

    input_events_req_flags: list[str] = []
    if bool(args.with_input_events):
        input_events_req_flags.append(
            "--with-input-events/--with-virtio-input-events/--require-virtio-input-events/--enable-virtio-input-events"
        )
    if need_input_wheel:
        input_events_req_flags.append(
            "--with-input-wheel/--with-virtio-input-wheel/--require-virtio-input-wheel/--enable-virtio-input-wheel"
        )
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
    virtio_disable_msix = bool(args.virtio_disable_msix)

    if virtio_disable_msix and (
        args.virtio_msix_vectors is not None
        or args.virtio_net_vectors is not None
        or args.virtio_blk_vectors is not None
        or args.virtio_snd_vectors is not None
        or args.virtio_input_vectors is not None
    ):
        parser.error(
            "--virtio-disable-msix is mutually exclusive with --virtio-msix-vectors/--virtio-*-vectors "
            "(INTx-only mode disables MSI-X by forcing vectors=0). "
            "Aliases: --force-intx/--intx-only."
        )

    # `--require-virtio-{net,blk,snd}-msix` uses a host-side QMP MSI-X-enabled check, while
    # `--require-virtio-input-msix` is a guest marker check. Both are incompatible with INTx-only
    # mode, since `--virtio-disable-msix` disables message interrupts by forcing `vectors=0`.
    if virtio_disable_msix and (
        need_msix_check or bool(getattr(args, "require_virtio_input_msix", False))
    ):
        # Keep the `--require-virtio-*-msix` phrasing stable for tests and greppability; add alias
        # hints for usability.
        parser.error(
            "--virtio-disable-msix is incompatible with --require-virtio-*-msix "
            "(aliases: --require-net-msix/--require-blk-msix/--require-snd-msix/--require-input-msix) "
            "(MSI-X is disabled). "
            "Aliases: --force-intx/--intx-only."
        )

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
        parser.error(
            "--require-virtio-snd-msix/--require-snd-msix requires --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd"
        )
    if args.udp_port <= 0 or args.udp_port > 65535:
        parser.error("--udp-port must be in the range 1..65535")
    if args.http_port <= 0 or args.http_port > 65535:
        parser.error("--http-port must be in the range 1..65535")
    if not args.http_path.startswith("/"):
        parser.error("--http-path must start with '/'")
    if any(ch.isspace() for ch in args.http_path):
        parser.error("--http-path must not contain whitespace")
    if args.memory_mb <= 0:
        parser.error("--memory-mb must be a positive integer")
    if args.smp <= 0:
        parser.error("--smp must be a positive integer")
    if args.timeout_seconds <= 0:
        parser.error("--timeout-seconds must be a positive integer")

    # Note: INTx-only mode (vectors=0) is handled separately via `_qemu_device_arg_disable_msix` and
    # a dedicated preflight that verifies QEMU accepts `vectors=0` for the relevant devices.

    if not args.enable_virtio_snd:
        if args.with_snd_buffer_limits:
            parser.error(
                "--with-snd-buffer-limits requires --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd "
                "(aliases: --with-virtio-snd-buffer-limits/--require-virtio-snd-buffer-limits/--enable-snd-buffer-limits/--enable-virtio-snd-buffer-limits)"
            )
        if args.virtio_snd_audio_backend != "none" or args.virtio_snd_wav_path is not None:
            parser.error(
                "--virtio-snd-* options require --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd"
            )
    elif args.virtio_snd_audio_backend == "wav" and not args.virtio_snd_wav_path:
        parser.error("--virtio-snd-wav-path is required when --virtio-snd-audio-backend=wav")
    elif args.virtio_snd_audio_backend == "wav":
        # Prepare the wav output path eagerly so we fail fast with a clear error instead of letting
        # QEMU fail to create the file (or accidentally overwriting a directory path).
        wav_path = Path(args.virtio_snd_wav_path).resolve()
        if wav_path.exists() and wav_path.is_dir():
            parser.error(f"--virtio-snd-wav-path must be a file path (got directory): {wav_path}")
        if not args.dry_run:
            try:
                wav_path.parent.mkdir(parents=True, exist_ok=True)
            except OSError as e:
                parser.error(f"failed to create virtio-snd wav output directory {wav_path.parent}: {e}")
            try:
                if wav_path.exists():
                    wav_path.unlink()
            except OSError as e:
                parser.error(f"failed to remove existing virtio-snd wav output file {wav_path}: {e}")

    if args.virtio_snd_verify_wav:
        if not args.enable_virtio_snd:
            parser.error(
                "--virtio-snd-verify-wav requires --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd"
            )
        if args.virtio_snd_audio_backend != "wav":
            parser.error("--virtio-snd-verify-wav requires --virtio-snd-audio-backend=wav")
        if int(args.virtio_snd_wav_peak_threshold) < 0:
            parser.error("--virtio-snd-wav-peak-threshold must be >= 0")
        if int(args.virtio_snd_wav_rms_threshold) < 0:
            parser.error("--virtio-snd-wav-rms-threshold must be >= 0")

    if args.virtio_transitional and args.enable_virtio_snd:
        parser.error(
            "--virtio-transitional is incompatible with --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd "
            "(virtio-snd testing requires modern-only virtio-pci + contract revision overrides)"
        )

    if args.virtio_snd_vectors is not None and not args.enable_virtio_snd:
        parser.error(
            "--virtio-snd-vectors requires --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd"
        )
    if need_blk_resize:
        if args.virtio_transitional:
            parser.error(
                "--with-blk-resize/--with-virtio-blk-resize/--require-virtio-blk-resize/--enable-virtio-blk-resize is incompatible with --virtio-transitional "
                "(blk resize uses the contract-v1 drive layout with id=drive0)"
            )
        if int(args.blk_resize_delta_mib) <= 0:
            parser.error(
                "--blk-resize-delta-mib must be > 0 when "
                "--with-blk-resize/--with-virtio-blk-resize/--require-virtio-blk-resize/--enable-virtio-blk-resize is enabled"
            )

    if need_input_events and not args.dry_run:
        # In default (contract-v1) mode we already validate virtio-keyboard-pci/virtio-mouse-pci via
        # `_assert_qemu_supports_aero_w7_virtio_contract_v1`. In transitional mode virtio-input is
        # optional, but input event injection requires these devices to exist.
        try:
            have_kbd = _qemu_has_device_strict(args.qemu_system, "virtio-keyboard-pci")
            have_mouse = _qemu_has_device_strict(args.qemu_system, "virtio-mouse-pci")
        except RuntimeError as e:
            parser.error(str(e))
        if not have_kbd or not have_mouse:
            parser.error(
                "--with-input-events/--with-virtio-input-events/--require-virtio-input-events/--enable-virtio-input-events"
                "/--with-input-wheel/--with-virtio-input-wheel/--require-virtio-input-wheel/--enable-virtio-input-wheel"
                "/--with-input-events-extended/--with-input-events-extra requires "
                "QEMU virtio-keyboard-pci and virtio-mouse-pci support. Upgrade QEMU or omit input event injection."
            )

    if need_input_led and not args.dry_run:
        # The guest virtio-input sanity test requires both keyboard and mouse. Fail fast with a clearer
        # host-side error when the running QEMU build does not advertise one of these devices.
        try:
            have_kbd = _qemu_has_device_strict(args.qemu_system, "virtio-keyboard-pci")
            have_mouse = _qemu_has_device_strict(args.qemu_system, "virtio-mouse-pci")
        except RuntimeError as e:
            parser.error(str(e))
        if not have_kbd or not have_mouse:
            parser.error(
                "--with-input-led/--with-virtio-input-led/--require-virtio-input-led/--enable-virtio-input-led requires QEMU virtio-keyboard-pci and virtio-mouse-pci support. "
                "Upgrade QEMU or omit LED/statusq validation."
            )

    if need_input_leds and not args.dry_run:
        try:
            have_kbd = _qemu_has_device_strict(args.qemu_system, "virtio-keyboard-pci")
            have_mouse = _qemu_has_device_strict(args.qemu_system, "virtio-mouse-pci")
        except RuntimeError as e:
            parser.error(str(e))
        if not have_kbd or not have_mouse:
            parser.error(
                "--with-input-leds/--with-virtio-input-leds/--require-virtio-input-leds/--enable-virtio-input-leds requires QEMU virtio-keyboard-pci and virtio-mouse-pci support. "
                "Upgrade QEMU or omit LED/statusq validation."
            )

    if need_input_media_keys and not args.dry_run:
        try:
            have_kbd = _qemu_has_device_strict(args.qemu_system, "virtio-keyboard-pci")
        except RuntimeError as e:
            parser.error(str(e))
        if not have_kbd:
            parser.error(
                "--with-input-media-keys/--with-virtio-input-media-keys/--require-virtio-input-media-keys/--enable-virtio-input-media-keys requires QEMU virtio-keyboard-pci support. Upgrade QEMU or omit media key injection."
            )

    if attach_virtio_tablet and not args.dry_run:
        try:
            help_text = _qemu_device_list_help_text(args.qemu_system)
        except RuntimeError as e:
            parser.error(str(e))
        if "virtio-tablet-pci" not in help_text:
            parser.error(
                "--with-virtio-tablet/--with-input-tablet-events/--with-tablet-events/--with-virtio-input-tablet-events/--require-virtio-input-tablet-events/--enable-virtio-input-tablet-events requires "
                "QEMU virtio-tablet-pci support. Upgrade QEMU or omit tablet support."
            )

    if not args.virtio_transitional and not args.dry_run:
        try:
            _assert_qemu_supports_aero_w7_virtio_contract_v1(
                args.qemu_system,
                with_virtio_snd=args.enable_virtio_snd,
                with_virtio_tablet=attach_virtio_tablet,
            )
        except RuntimeError as e:
            print(f"ERROR: {e}", file=sys.stderr)
            return 2

    vectors_requested = any(
        v is not None
        for v in (virtio_net_vectors, virtio_blk_vectors, virtio_input_vectors, virtio_snd_vectors)
    )
    if vectors_requested and not args.dry_run:
        requested_by_parts: list[str] = []
        if args.virtio_disable_msix:
            requested_by_parts.append("--virtio-disable-msix")
        if args.virtio_msix_vectors is not None:
            requested_by_parts.append(f"--virtio-msix-vectors={int(args.virtio_msix_vectors)}")
        if args.virtio_net_vectors is not None:
            requested_by_parts.append(f"--virtio-net-vectors={int(args.virtio_net_vectors)}")
        if args.virtio_blk_vectors is not None:
            requested_by_parts.append(f"--virtio-blk-vectors={int(args.virtio_blk_vectors)}")
        if args.virtio_input_vectors is not None:
            requested_by_parts.append(f"--virtio-input-vectors={int(args.virtio_input_vectors)}")
        if args.virtio_snd_vectors is not None:
            requested_by_parts.append(f"--virtio-snd-vectors={int(args.virtio_snd_vectors)}")
        requested_by = "/".join(requested_by_parts) if requested_by_parts else "--virtio-msix-vectors"

        devices: list[str] = []
        if virtio_net_vectors is not None:
            devices.append("virtio-net-pci")
        if virtio_blk_vectors is not None:
            devices.append("virtio-blk-pci")
        if virtio_input_vectors is not None:
            if args.virtio_transitional:
                if _qemu_has_device(args.qemu_system, "virtio-keyboard-pci"):
                    devices.append("virtio-keyboard-pci")
                if _qemu_has_device(args.qemu_system, "virtio-mouse-pci"):
                    devices.append("virtio-mouse-pci")
                if attach_virtio_tablet:
                    devices.append("virtio-tablet-pci")
            else:
                devices += ["virtio-keyboard-pci", "virtio-mouse-pci"]
                if attach_virtio_tablet:
                    devices.append("virtio-tablet-pci")
        if args.enable_virtio_snd and virtio_snd_vectors is not None:
            devices.append(_detect_virtio_snd_device(args.qemu_system))

        if devices:
            try:
                _assert_qemu_devices_support_vectors_property(
                    args.qemu_system,
                    devices,
                    requested_by=requested_by,
                )
            except RuntimeError as e:
                print(f"ERROR: {e}", file=sys.stderr)
                return 2

    disk_image = Path(args.disk_image).resolve()
    if disk_image.exists() and disk_image.is_dir():
        print(f"ERROR: disk image path is a directory: {disk_image}", file=sys.stderr)
        return 2
    if not disk_image.exists():
        if args.dry_run:
            print(f"WARNING: disk image not found: {disk_image}", file=sys.stderr)
        else:
            print(f"ERROR: disk image not found: {disk_image}", file=sys.stderr)
            return 2
    serial_log = Path(args.serial_log).resolve()
    if serial_log.exists() and serial_log.is_dir():
        print(f"ERROR: serial log path is a directory: {serial_log}", file=sys.stderr)
        return 2
    if not args.dry_run:
        try:
            serial_log.parent.mkdir(parents=True, exist_ok=True)
        except OSError as e:
            print(f"ERROR: failed to create serial log directory: {serial_log.parent}: {e}", file=sys.stderr)
            return 2
        try:
            if serial_log.exists():
                serial_log.unlink()
        except OSError as e:
            print(f"ERROR: failed to remove existing serial log: {serial_log}: {e}", file=sys.stderr)
            return 2

    qemu_stderr_log = serial_log.with_name(serial_log.stem + ".qemu.stderr.log")
    if not args.dry_run:
        if qemu_stderr_log.exists() and qemu_stderr_log.is_dir():
            print(f"ERROR: qemu stderr log path is a directory: {qemu_stderr_log}", file=sys.stderr)
            return 2
        try:
            qemu_stderr_log.unlink()
        except FileNotFoundError:
            pass
        except OSError as e:
            print(
                f"ERROR: failed to remove existing QEMU stderr log: {qemu_stderr_log}: {e}",
                file=sys.stderr,
            )
            return 2

    if virtio_disable_msix and not args.dry_run:
        # INTx-only mode: verify the running QEMU build accepts `vectors=0` for the virtio-pci devices
        # the harness will create. Some QEMU builds expose the `vectors` property but reject `0`.
        devices_to_check = ["virtio-net-pci", "virtio-blk-pci"]
        if args.virtio_transitional:
            if _qemu_has_device(args.qemu_system, "virtio-keyboard-pci"):
                devices_to_check.append("virtio-keyboard-pci")
            if _qemu_has_device(args.qemu_system, "virtio-mouse-pci"):
                devices_to_check.append("virtio-mouse-pci")
            if attach_virtio_tablet:
                devices_to_check.append("virtio-tablet-pci")
        else:
            # Contract-v1 mode: virtio-keyboard/mouse are required.
            devices_to_check += ["virtio-keyboard-pci", "virtio-mouse-pci"]
            if attach_virtio_tablet:
                devices_to_check.append("virtio-tablet-pci")
        if args.enable_virtio_snd:
            devices_to_check.append(_detect_virtio_snd_device(args.qemu_system))

        try:
            for dev in devices_to_check:
                if not _qemu_device_supports_property(args.qemu_system, dev, "vectors"):
                    raise RuntimeError(
                        f"QEMU device '{dev}' does not advertise a 'vectors' property (needed for vectors=0)."
                    )
                _qemu_device_help_text(args.qemu_system, f"{dev},vectors=0")
        except RuntimeError as e:
            # Mirror the QEMU output into the usual sidecar log path so CI artifacts remain consistent
            # with other early-exit failures.
            try:
                qemu_stderr_log.write_text(str(e) + "\n", encoding="utf-8", errors="replace")
            except Exception:
                pass
            print(
                "ERROR: --virtio-disable-msix requested, but this QEMU build rejected 'vectors=0' "
                "(needed to disable MSI-X and force INTx). "
                "Upgrade QEMU or omit --virtio-disable-msix (aliases: --force-intx/--intx-only).",
                file=sys.stderr,
            )
            print(f"  Wrote QEMU output to: {qemu_stderr_log}", file=sys.stderr)
            print(f"  Details: {e}", file=sys.stderr)
            return 2

    # QMP endpoint used to:
    # - request a graceful shutdown (so the wav audiodev can flush/finalize)
    # - optionally inject virtio-input events (keyboard + mouse) via QMP (prefers `input-send-event` with backcompat fallbacks)
    # - optionally introspect PCI state to verify MSI-X enablement (`--require-virtio-*-msix`)
    #
    # Historically we enabled QMP only when we needed a graceful exit for `-audiodev wav` output, so we
    # wouldn't introduce extra host port/socket dependencies in non-audio harness runs. Input injection
    # also requires QMP, but remains opt-in via:
    # - --with-input-events / --with-virtio-input-events / --require-virtio-input-events / --enable-virtio-input-events
    # - --with-input-media-keys / --with-virtio-input-media-keys / --require-virtio-input-media-keys / --enable-virtio-input-media-keys
    # - --with-input-wheel
    # - --with-input-events-extended / --with-input-events-extra
    # - --with-input-tablet-events / --with-tablet-events / --with-virtio-input-tablet-events / --require-virtio-input-tablet-events / --enable-virtio-input-tablet-events
    # - --with-net-link-flap
    # - --with-blk-resize
    # - --require-virtio-*-msix
    # - --qemu-preflight-pci / --qmp-preflight-pci
    use_qmp = (
        (args.enable_virtio_snd and args.virtio_snd_audio_backend == "wav")
        or need_input_events
        or need_input_media_keys
        or need_input_tablet_events
        or need_net_link_flap
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
                if not args.dry_run:
                    try:
                        qmp_socket.unlink()
                    except FileNotFoundError:
                        pass
                    except OSError as e:
                        print(
                            f"WARNING: failed to remove existing QMP socket path {qmp_socket}: {e}. Falling back to TCP QMP.",
                            file=sys.stderr,
                        )
                        qmp_socket = None
                if qmp_socket is not None:
                    qmp_endpoint = _QmpEndpoint(unix_socket=qmp_socket)

        if qmp_endpoint is None:
            port = _find_free_tcp_port()
            if port is None:
                if (
                    need_input_events
                    or need_input_media_keys
                    or need_input_tablet_events
                    or need_net_link_flap
                    or need_blk_resize
                    or need_msix_check
                    or bool(args.qemu_preflight_pci)
                ):
                    req_flags: list[str] = []
                    if bool(args.with_input_events):
                        req_flags.append(
                            "--with-input-events/--with-virtio-input-events/--require-virtio-input-events/--enable-virtio-input-events"
                        )
                    if need_input_media_keys:
                        req_flags.append(
                            "--with-input-media-keys/--with-virtio-input-media-keys/--require-virtio-input-media-keys/--enable-virtio-input-media-keys"
                        )
                    if need_input_wheel:
                        req_flags.append(
                            "--with-input-wheel/--with-virtio-input-wheel/--require-virtio-input-wheel/--enable-virtio-input-wheel"
                        )
                    if need_input_events_extended:
                        req_flags.append("--with-input-events-extended/--with-input-events-extra")
                    if need_input_tablet_events:
                        req_flags.append(
                            "--with-input-tablet-events/--with-tablet-events/--with-virtio-input-tablet-events/--require-virtio-input-tablet-events/--enable-virtio-input-tablet-events"
                        )
                    if need_net_link_flap:
                        req_flags.append(
                            "--with-net-link-flap/--with-virtio-net-link-flap/--require-virtio-net-link-flap/--enable-virtio-net-link-flap"
                        )
                    if need_blk_resize:
                        req_flags.append(
                            "--with-blk-resize/--with-virtio-blk-resize/--require-virtio-blk-resize/--enable-virtio-blk-resize"
                        )
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
        or need_net_link_flap
        or need_blk_resize
        or need_msix_check
        or bool(args.qemu_preflight_pci)
    ) and qmp_endpoint is None:
        req_flags: list[str] = []
        if bool(args.with_input_events):
            req_flags.append(
                "--with-input-events/--with-virtio-input-events/--require-virtio-input-events/--enable-virtio-input-events"
            )
        if need_input_media_keys:
            req_flags.append(
                "--with-input-media-keys/--with-virtio-input-media-keys/--require-virtio-input-media-keys/--enable-virtio-input-media-keys"
            )
        if need_input_wheel:
            req_flags.append(
                "--with-input-wheel/--with-virtio-input-wheel/--require-virtio-input-wheel/--enable-virtio-input-wheel"
            )
        if need_input_events_extended:
            req_flags.append("--with-input-events-extended/--with-input-events-extra")
        if need_input_tablet_events:
            req_flags.append(
                "--with-input-tablet-events/--with-tablet-events/--with-virtio-input-tablet-events/--require-virtio-input-tablet-events/--enable-virtio-input-tablet-events"
            )
        if need_net_link_flap:
            req_flags.append(
                "--with-net-link-flap/--with-virtio-net-link-flap/--require-virtio-net-link-flap/--enable-virtio-net-link-flap"
            )
        if need_blk_resize:
            req_flags.append(
                "--with-blk-resize/--with-virtio-blk-resize/--require-virtio-blk-resize/--enable-virtio-blk-resize"
            )
        if need_msix_check:
            req_flags.append("--require-virtio-*-msix")
        if bool(args.qemu_preflight_pci):
            req_flags.append("--qemu-preflight-pci/--qmp-preflight-pci")
        print(
            f"ERROR: {'/'.join(req_flags)} requires QMP, but a QMP endpoint could not be allocated",
            file=sys.stderr,
        )
        return 2

    if args.dry_run:
        qemu_args = _build_qemu_args_dry_run(
            args,
            qemu_extra,
            disk_image=disk_image,
            serial_log=serial_log,
            qmp_endpoint=qmp_endpoint,
            virtio_net_vectors=virtio_net_vectors,
            virtio_blk_vectors=virtio_blk_vectors,
            virtio_input_vectors=virtio_input_vectors,
            virtio_snd_vectors=virtio_snd_vectors,
            attach_virtio_tablet=attach_virtio_tablet,
            virtio_disable_msix=virtio_disable_msix,
        )
        # First line: machine-readable JSON argv array.
        print(json.dumps(qemu_args, separators=(",", ":")))
        # Second line: best-effort single-line command for copy/paste.
        print(_format_commandline_for_host(qemu_args))

        # Any additional human-oriented diagnostics go to stderr so stdout remains easy to parse
        # for tooling/CI (first line JSON, second line command).
        print("", file=sys.stderr)
        print("DryRun: derived settings:", file=sys.stderr)
        mode = "transitional" if args.virtio_transitional else "contract-v1"
        print(f"  mode={mode}", file=sys.stderr)
        if qmp_endpoint is None:
            print("  qmp=disabled", file=sys.stderr)
        else:
            print(f"  qmp=enabled ({qmp_endpoint.qemu_arg()})", file=sys.stderr)

        print(f"  virtio_disable_msix={virtio_disable_msix}", file=sys.stderr)

        # Vector resolution is derived from the global/per-device flags. In dry-run mode we do not
        # probe QEMU to validate that the running build supports the `vectors` property, so the argv
        # printed here may differ from a real harness run: in non-dry-run mode the harness performs a
        # `-device <name>,help` preflight and fails fast if `vectors` isn't supported.
        def _fmt_vectors(name: str, value: Optional[int], flag: str) -> str:
            if name == "snd" and not args.enable_virtio_snd:
                return f"{name}=disabled"
            if virtio_disable_msix:
                return f"{name}=0 (forced by --virtio-disable-msix/--force-intx/--intx-only)"
            if value is None:
                return f"{name}=default"
            return f"{name}={value} (from {flag})"

        print(
            "  vectors: "
            + ", ".join(
                [
                    _fmt_vectors("net", virtio_net_vectors, virtio_net_vectors_flag),
                    _fmt_vectors("blk", virtio_blk_vectors, virtio_blk_vectors_flag),
                    _fmt_vectors("input", virtio_input_vectors, virtio_input_vectors_flag),
                    _fmt_vectors("snd", virtio_snd_vectors, virtio_snd_vectors_flag),
                ]
            ),
            file=sys.stderr,
        )

        if args.enable_virtio_snd:
            # In non-dry-run mode, the harness probes QEMU to resolve whether the device is named
            # `virtio-sound-pci` or `virtio-snd-pci`. Dry-run mode skips subprocesses, so we log
            # the default name used by the dry-run argv builder.
            print(
                "  virtio_snd_device=virtio-sound-pci (dry-run default; QEMU probe skipped)",
                file=sys.stderr,
            )
            print(f"  virtio_snd_audio_backend={args.virtio_snd_audio_backend}", file=sys.stderr)

        if attach_virtio_tablet:
            note = ""
            if args.virtio_transitional:
                note = " (transitional: actual attachment is conditional on QEMU support)"
            print(f"  virtio_tablet=attached{note}", file=sys.stderr)
        else:
            print("  virtio_tablet=not-attached", file=sys.stderr)

        if qemu_extra:
            print(f"  qemu_extra_args={qemu_extra!r}", file=sys.stderr)

        print(
            "DryRun: NOTE: QEMU feature probes are skipped; device aliases and vectors support are not validated.",
            file=sys.stderr,
        )
        return 0

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
            if virtio_blk_vectors is None and not virtio_disable_msix:
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
                virtio_blk = _qemu_device_arg_disable_msix(virtio_blk, virtio_disable_msix)
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
                kbd = _qemu_device_arg_disable_msix(kbd, virtio_disable_msix)
                virtio_input_args += ["-device", kbd]
            if have_mouse:
                mouse = _qemu_device_arg_maybe_add_vectors(
                    args.qemu_system,
                    "virtio-mouse-pci",
                    f"virtio-mouse-pci,id={_VIRTIO_INPUT_QMP_MOUSE_ID}",
                    virtio_input_vectors,
                    flag_name=virtio_input_vectors_flag,
                )
                mouse = _qemu_device_arg_disable_msix(mouse, virtio_disable_msix)
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
                tablet = _qemu_device_arg_disable_msix(tablet, virtio_disable_msix)
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
                _qemu_device_arg_disable_msix(
                    _qemu_device_arg_maybe_add_vectors(
                        args.qemu_system,
                        "virtio-net-pci",
                        f"virtio-net-pci,id={_VIRTIO_NET_QMP_ID},netdev=net0",
                        virtio_net_vectors,
                        flag_name=virtio_net_vectors_flag,
                    ),
                    virtio_disable_msix,
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
                f"virtio-net-pci,id={_VIRTIO_NET_QMP_ID},netdev=net0,disable-legacy=on,x-pci-revision={aero_pci_rev}",
                virtio_net_vectors,
                flag_name=virtio_net_vectors_flag,
            )
            virtio_net = _qemu_device_arg_disable_msix(virtio_net, virtio_disable_msix)
            virtio_blk = _qemu_device_arg_maybe_add_vectors(
                args.qemu_system,
                "virtio-blk-pci",
                f"virtio-blk-pci,drive={drive_id},disable-legacy=on,x-pci-revision={aero_pci_rev}",
                virtio_blk_vectors,
                flag_name=virtio_blk_vectors_flag,
            )
            virtio_blk = _qemu_device_arg_disable_msix(virtio_blk, virtio_disable_msix)
            virtio_kbd = _qemu_device_arg_maybe_add_vectors(
                args.qemu_system,
                "virtio-keyboard-pci",
                f"virtio-keyboard-pci,id={_VIRTIO_INPUT_QMP_KEYBOARD_ID},disable-legacy=on,x-pci-revision={aero_pci_rev}",
                virtio_input_vectors,
                flag_name=virtio_input_vectors_flag,
            )
            virtio_kbd = _qemu_device_arg_disable_msix(virtio_kbd, virtio_disable_msix)
            virtio_mouse = _qemu_device_arg_maybe_add_vectors(
                args.qemu_system,
                "virtio-mouse-pci",
                f"virtio-mouse-pci,id={_VIRTIO_INPUT_QMP_MOUSE_ID},disable-legacy=on,x-pci-revision={aero_pci_rev}",
                virtio_input_vectors,
                flag_name=virtio_input_vectors_flag,
            )
            virtio_mouse = _qemu_device_arg_disable_msix(virtio_mouse, virtio_disable_msix)
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
                virtio_tablet = _qemu_device_arg_disable_msix(virtio_tablet, virtio_disable_msix)

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
                    device_arg = _qemu_device_arg_disable_msix(device_arg, virtio_disable_msix)
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

        if virtio_disable_msix:
            print("AERO_VIRTIO_WIN7_HOST|CONFIG|force_intx=1")

        print("Launching QEMU:")
        print("  " + _format_commandline_for_host(qemu_args))
        stderr_f = qemu_stderr_log.open("wb")
        try:
            proc = subprocess.Popen(qemu_args, stderr=stderr_f)
        except FileNotFoundError:
            msg = f"qemu-system binary not found: {args.qemu_system}"
            print(f"ERROR: {msg}", file=sys.stderr)
            try:
                stderr_f.write((msg + "\n").encode("utf-8", errors="replace"))
            except Exception:
                pass
            try:
                stderr_f.close()
            except Exception:
                pass
            if udp_server is not None:
                udp_server.close()
            httpd.shutdown()
            if qmp_socket is not None:
                try:
                    qmp_socket.unlink()
                except FileNotFoundError:
                    pass
                except OSError:
                    pass
            return 2
        except OSError as e:
            msg = f"failed to start qemu-system binary: {args.qemu_system}: {e}"
            print(f"ERROR: {msg}", file=sys.stderr)
            try:
                stderr_f.write((msg + "\n").encode("utf-8", errors="replace"))
            except Exception:
                pass
            try:
                stderr_f.close()
            except Exception:
                pass
            if udp_server is not None:
                udp_server.close()
            httpd.shutdown()
            if qmp_socket is not None:
                try:
                    qmp_socket.unlink()
                except FileNotFoundError:
                    pass
                except OSError:
                    pass
            return 2
        result_code: Optional[int] = None
        try:
            if args.qemu_preflight_pci:
                if qmp_endpoint is None:
                    raise AssertionError(
                        "--qemu-preflight-pci/--qmp-preflight-pci requested but QMP endpoint is not configured"
                    )
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
            virtio_blk_counters_marker_line: Optional[str] = None
            virtio_blk_counters_marker_carry = b""
            virtio_blk_reset_recovery_marker_line: Optional[str] = None
            virtio_blk_reset_recovery_marker_carry = b""
            virtio_blk_miniport_flags_marker_line: Optional[str] = None
            virtio_blk_miniport_flags_marker_carry = b""
            virtio_blk_miniport_reset_recovery_marker_line: Optional[str] = None
            virtio_blk_miniport_reset_recovery_marker_carry = b""
            virtio_blk_resize_marker_line: Optional[str] = None
            virtio_blk_resize_marker_carry = b""
            virtio_blk_reset_marker_line: Optional[str] = None
            virtio_blk_reset_marker_carry = b""
            virtio_blk_msix_marker_line: Optional[str] = None
            virtio_blk_msix_marker_carry = b""
            virtio_net_msix_marker_line: Optional[str] = None
            virtio_net_msix_marker_carry = b""
            virtio_net_marker_line: Optional[str] = None
            virtio_net_marker_carry = b""
            virtio_net_udp_marker_line: Optional[str] = None
            virtio_net_udp_marker_carry = b""
            virtio_net_udp_dns_marker_line: Optional[str] = None
            virtio_net_udp_dns_marker_carry = b""
            virtio_net_link_flap_marker_line: Optional[str] = None
            virtio_net_link_flap_marker_carry = b""
            virtio_net_offload_csum_marker_line: Optional[str] = None
            virtio_net_offload_csum_marker_carry = b""
            virtio_net_diag_marker_line: Optional[str] = None
            virtio_net_diag_marker_carry = b""
            virtio_snd_marker_line: Optional[str] = None
            virtio_snd_marker_carry = b""
            virtio_snd_skip_reason: Optional[str] = None
            virtio_snd_skip_reason_carry = b""
            virtio_snd_capture_marker_line: Optional[str] = None
            virtio_snd_capture_marker_carry = b""
            virtio_snd_duplex_marker_line: Optional[str] = None
            virtio_snd_duplex_marker_carry = b""
            virtio_snd_buffer_limits_marker_line: Optional[str] = None
            virtio_snd_buffer_limits_marker_carry = b""
            virtio_snd_msix_marker_line: Optional[str] = None
            virtio_snd_msix_marker_carry = b""
            virtio_input_marker_line: Optional[str] = None
            virtio_input_marker_carry = b""
            virtio_input_bind_marker_line: Optional[str] = None
            virtio_input_bind_marker_carry = b""
            virtio_input_msix_marker_line: Optional[str] = None
            virtio_input_msix_marker_carry = b""
            virtio_input_leds_marker_line: Optional[str] = None
            virtio_input_leds_marker_carry = b""
            virtio_input_events_marker_line: Optional[str] = None
            virtio_input_events_marker_carry = b""
            virtio_input_media_keys_marker_line: Optional[str] = None
            virtio_input_media_keys_marker_carry = b""
            virtio_input_led_marker_line: Optional[str] = None
            virtio_input_led_marker_carry = b""
            virtio_input_wheel_marker_line: Optional[str] = None
            virtio_input_wheel_marker_carry = b""
            virtio_input_events_modifiers_marker_line: Optional[str] = None
            virtio_input_events_modifiers_marker_carry = b""
            virtio_input_events_buttons_marker_line: Optional[str] = None
            virtio_input_events_buttons_marker_carry = b""
            virtio_input_events_wheel_marker_line: Optional[str] = None
            virtio_input_events_wheel_marker_carry = b""
            virtio_input_tablet_events_marker_line: Optional[str] = None
            virtio_input_tablet_events_marker_carry = b""
            virtio_input_msix_marker: Optional[_VirtioInputMsixMarker] = None
            virtio_input_binding_marker_line: Optional[str] = None
            virtio_input_binding_marker_carry = b""
            selftest_config_marker_line: Optional[str] = None
            selftest_config_marker_carry = b""
            selftest_result_marker_line: Optional[str] = None
            selftest_result_marker_carry = b""
            expect_blk_msi_config: Optional[str] = None
            udp_port_config: Optional[str] = None
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
            saw_virtio_blk_reset_pass = False
            saw_virtio_blk_reset_skip = False
            saw_virtio_blk_reset_fail = False
            saw_virtio_input_pass = False
            saw_virtio_input_fail = False
            saw_virtio_input_bind_pass = False
            saw_virtio_input_bind_fail = False
            saw_virtio_input_binding_pass = False
            saw_virtio_input_binding_fail = False
            saw_virtio_input_binding_skip = False
            virtio_input_marker_time: Optional[float] = None
            saw_virtio_input_leds_pass = False
            saw_virtio_input_leds_fail = False
            saw_virtio_input_leds_skip = False
            saw_virtio_input_events_ready = False
            saw_virtio_input_events_pass = False
            saw_virtio_input_events_fail = False
            saw_virtio_input_events_skip = False
            saw_virtio_input_media_keys_ready = False
            saw_virtio_input_media_keys_pass = False
            saw_virtio_input_media_keys_fail = False
            saw_virtio_input_media_keys_skip = False
            saw_virtio_input_led_pass = False
            saw_virtio_input_led_fail = False
            saw_virtio_input_led_skip = False
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
            virtio_net_marker_time: Optional[float] = None
            msix_checked = False
            saw_virtio_net_udp_pass = False
            saw_virtio_net_udp_fail = False
            saw_virtio_net_udp_skip = False
            saw_virtio_net_link_flap_ready = False
            saw_virtio_net_link_flap_pass = False
            saw_virtio_net_link_flap_fail = False
            saw_virtio_net_link_flap_skip = False
            did_net_link_flap = False
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
                    virtio_blk_counters_marker_line, virtio_blk_counters_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_blk_counters_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|",
                        carry=virtio_blk_counters_marker_carry,
                    )
                    virtio_blk_reset_recovery_marker_line, virtio_blk_reset_recovery_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_blk_reset_recovery_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|",
                        carry=virtio_blk_reset_recovery_marker_carry,
                    )
                    virtio_blk_miniport_flags_marker_line, virtio_blk_miniport_flags_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_blk_miniport_flags_marker_line,
                        chunk,
                        prefix=b"virtio-blk-miniport-flags|",
                        carry=virtio_blk_miniport_flags_marker_carry,
                    )
                    virtio_blk_miniport_reset_recovery_marker_line, virtio_blk_miniport_reset_recovery_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_blk_miniport_reset_recovery_marker_line,
                        chunk,
                        prefix=b"virtio-blk-miniport-reset-recovery|",
                        carry=virtio_blk_miniport_reset_recovery_marker_carry,
                    )
                    virtio_blk_resize_marker_line, virtio_blk_resize_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_blk_resize_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|",
                        carry=virtio_blk_resize_marker_carry,
                    )
                    virtio_blk_reset_marker_line, virtio_blk_reset_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_blk_reset_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|",
                        carry=virtio_blk_reset_marker_carry,
                    )
                    virtio_blk_msix_marker_line, virtio_blk_msix_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_blk_msix_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-msix|",
                        carry=virtio_blk_msix_marker_carry,
                    )
                    virtio_net_msix_marker_line, virtio_net_msix_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_net_msix_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|",
                        carry=virtio_net_msix_marker_carry,
                    )
                    virtio_net_marker_line, virtio_net_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_net_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|",
                        carry=virtio_net_marker_carry,
                    )
                    virtio_net_udp_marker_line, virtio_net_udp_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_net_udp_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|",
                        carry=virtio_net_udp_marker_carry,
                    )
                    virtio_net_udp_dns_marker_line, virtio_net_udp_dns_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_net_udp_dns_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp-dns|",
                        carry=virtio_net_udp_dns_marker_carry,
                    )
                    virtio_net_link_flap_marker_line, virtio_net_link_flap_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_net_link_flap_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|",
                        carry=virtio_net_link_flap_marker_carry,
                    )
                    virtio_net_offload_csum_marker_line, virtio_net_offload_csum_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_net_offload_csum_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-offload-csum|",
                        carry=virtio_net_offload_csum_marker_carry,
                    )
                    virtio_net_diag_marker_line, virtio_net_diag_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_net_diag_marker_line,
                        chunk,
                        prefix=b"virtio-net-diag|",
                        carry=virtio_net_diag_marker_carry,
                    )
                    virtio_snd_marker_line, virtio_snd_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_snd_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd|",
                        carry=virtio_snd_marker_carry,
                    )
                    virtio_snd_skip_reason, virtio_snd_skip_reason_carry = _update_virtio_snd_skip_reason_from_chunk(
                        virtio_snd_skip_reason,
                        chunk,
                        carry=virtio_snd_skip_reason_carry,
                    )
                    virtio_snd_capture_marker_line, virtio_snd_capture_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_snd_capture_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|",
                        carry=virtio_snd_capture_marker_carry,
                    )
                    virtio_snd_duplex_marker_line, virtio_snd_duplex_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_snd_duplex_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|",
                        carry=virtio_snd_duplex_marker_carry,
                    )
                    virtio_snd_buffer_limits_marker_line, virtio_snd_buffer_limits_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_snd_buffer_limits_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|",
                        carry=virtio_snd_buffer_limits_marker_carry,
                    )
                    virtio_snd_msix_marker_line, virtio_snd_msix_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_snd_msix_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|",
                        carry=virtio_snd_msix_marker_carry,
                    )
                    virtio_input_marker_line, virtio_input_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_input_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-input|",
                        carry=virtio_input_marker_carry,
                    )
                    virtio_input_bind_marker_line, virtio_input_bind_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_input_bind_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|",
                        carry=virtio_input_bind_marker_carry,
                    )
                    virtio_input_msix_marker_line, virtio_input_msix_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_input_msix_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|",
                        carry=virtio_input_msix_marker_carry,
                    )
                    virtio_input_leds_marker_line, virtio_input_leds_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_input_leds_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|",
                        carry=virtio_input_leds_marker_carry,
                    )
                    virtio_input_events_marker_line, virtio_input_events_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_input_events_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|",
                        carry=virtio_input_events_marker_carry,
                    )
                    virtio_input_media_keys_marker_line, virtio_input_media_keys_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_input_media_keys_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|",
                        carry=virtio_input_media_keys_marker_carry,
                    )
                    virtio_input_led_marker_line, virtio_input_led_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_input_led_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|",
                        carry=virtio_input_led_marker_carry,
                    )
                    virtio_input_wheel_marker_line, virtio_input_wheel_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_input_wheel_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|",
                        carry=virtio_input_wheel_marker_carry,
                    )
                    virtio_input_events_modifiers_marker_line, virtio_input_events_modifiers_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_input_events_modifiers_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-modifiers|",
                        carry=virtio_input_events_modifiers_marker_carry,
                    )
                    virtio_input_events_buttons_marker_line, virtio_input_events_buttons_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_input_events_buttons_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-buttons|",
                        carry=virtio_input_events_buttons_marker_carry,
                    )
                    virtio_input_events_wheel_marker_line, virtio_input_events_wheel_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_input_events_wheel_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-wheel|",
                        carry=virtio_input_events_wheel_marker_carry,
                    )
                    virtio_input_tablet_events_marker_line, virtio_input_tablet_events_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_input_tablet_events_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|",
                        carry=virtio_input_tablet_events_marker_carry,
                    )
                    virtio_input_binding_marker_line, virtio_input_binding_marker_carry = _update_last_marker_line_from_chunk(
                        virtio_input_binding_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|",
                        carry=virtio_input_binding_marker_carry,
                    )
                    selftest_config_marker_line, selftest_config_marker_carry = _update_last_marker_line_from_chunk(
                        selftest_config_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|CONFIG|",
                        carry=selftest_config_marker_carry,
                    )
                    selftest_result_marker_line, selftest_result_marker_carry = _update_last_marker_line_from_chunk(
                        selftest_result_marker_line,
                        chunk,
                        prefix=b"AERO_VIRTIO_SELFTEST|RESULT|",
                        carry=selftest_result_marker_carry,
                    )
                    tail = _append_serial_tail(tail, chunk)
                    if expect_blk_msi_config is None:
                        if selftest_config_marker_line is not None:
                            expect_blk_msi_config = _parse_marker_kv_fields(selftest_config_marker_line).get(
                                "expect_blk_msi"
                            )
                        elif b"AERO_VIRTIO_SELFTEST|CONFIG|" in tail:
                            expect_blk_msi_config = _try_get_selftest_config_expect_blk_msi(tail)
                        if args.require_expect_blk_msi and expect_blk_msi_config == "0":
                            print(
                                "FAIL: EXPECT_BLK_MSI_NOT_SET: guest selftest CONFIG expect_blk_msi=0 "
                                "(re-provision the image with --expect-blk-msi / New-AeroWin7TestImage.ps1 -ExpectBlkMsi)",
                                file=sys.stderr,
                            )
                            _print_tail(serial_log)
                            result_code = 1
                            break
                    if udp_port_config is None:
                        if selftest_config_marker_line is not None:
                            udp_port_config = _parse_marker_kv_fields(selftest_config_marker_line).get(
                                "udp_port"
                            )
                        elif b"AERO_VIRTIO_SELFTEST|CONFIG|" in tail:
                            udp_port_config = _try_get_selftest_config_udp_port(tail)
                        if udp_port_config is not None and udp_server is not None:
                            try:
                                guest_port = int(udp_port_config, 10)
                            except Exception:
                                guest_port = None
                            host_port = int(udp_server.port)
                            if guest_port is not None and guest_port != host_port:
                                print(
                                    "FAIL: UDP_PORT_MISMATCH: guest selftest CONFIG udp_port="
                                    f"{guest_port} but host harness UDP echo server is on {host_port}. "
                                    f"Run the harness with --udp-port {guest_port}, or re-provision the guest "
                                    f"to use --udp-port {host_port} (New-AeroWin7TestImage.ps1 -UdpPort {host_port}).",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                    # Prefer the incrementally captured marker line so we don't miss virtio-input-msix when
                    # the rolling tail buffer truncates earlier output.
                    if virtio_input_msix_marker is None:
                        if virtio_input_msix_marker_line is not None:
                            parts = virtio_input_msix_marker_line.split("|")
                            status = parts[3] if len(parts) >= 4 else ""
                            fields = _parse_marker_kv_fields(virtio_input_msix_marker_line)
                            virtio_input_msix_marker = _VirtioInputMsixMarker(
                                status=status,
                                fields=fields,
                                line=virtio_input_msix_marker_line,
                            )
                        elif b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|" in tail:
                            marker = _parse_virtio_input_msix_marker(tail)
                            if marker is not None:
                                virtio_input_msix_marker = marker

                    # Prefer the incrementally captured virtio-blk test marker line so we don't miss
                    # PASS/FAIL when the rolling tail buffer truncates earlier output.
                    if virtio_blk_marker_line is not None:
                        status_tok = _try_extract_marker_status(virtio_blk_marker_line)
                        if not saw_virtio_blk_pass and status_tok == "PASS":
                            saw_virtio_blk_pass = True
                            if virtio_blk_marker_time is None:
                                virtio_blk_marker_time = time.monotonic()
                        if not saw_virtio_blk_fail and status_tok == "FAIL":
                            saw_virtio_blk_fail = True
                            if virtio_blk_marker_time is None:
                                virtio_blk_marker_time = time.monotonic()

                    if not saw_virtio_blk_pass and b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS" in tail:
                        saw_virtio_blk_pass = True
                        if virtio_blk_marker_time is None:
                            virtio_blk_marker_time = time.monotonic()
                    if not saw_virtio_blk_fail and b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|FAIL" in tail:
                        saw_virtio_blk_fail = True
                        if virtio_blk_marker_time is None:
                            virtio_blk_marker_time = time.monotonic()

                    # Prefer the incrementally captured marker line (virtio_blk_resize_marker_line) so we
                    # don't miss the READY/PASS/FAIL/SKIP token when the rolling tail buffer truncates.
                    if virtio_blk_resize_marker_line is not None:
                        toks = virtio_blk_resize_marker_line.split("|")
                        status_tok = toks[3] if len(toks) >= 4 else ""
                        if not saw_virtio_blk_resize_pass and status_tok == "PASS":
                            saw_virtio_blk_resize_pass = True
                        if not saw_virtio_blk_resize_fail and status_tok == "FAIL":
                            saw_virtio_blk_resize_fail = True
                        if not saw_virtio_blk_resize_skip and status_tok == "SKIP":
                            saw_virtio_blk_resize_skip = True
                        if not saw_virtio_blk_resize_ready and status_tok == "READY":
                            fields = _parse_marker_kv_fields(virtio_blk_resize_marker_line)
                            if "old_bytes" in fields:
                                try:
                                    blk_resize_old_bytes = int(fields["old_bytes"], 0)
                                except Exception:
                                    blk_resize_old_bytes = None
                                if blk_resize_old_bytes is not None:
                                    saw_virtio_blk_resize_ready = True
                                    if need_blk_resize:
                                        delta_bytes = int(args.blk_resize_delta_mib) * 1024 * 1024
                                        blk_resize_new_bytes = _virtio_blk_resize_compute_new_bytes(
                                            blk_resize_old_bytes, delta_bytes
                                        )

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

                    # Prefer the incrementally captured marker line (virtio_blk_reset_marker_line) so we
                    # don't miss PASS/FAIL/SKIP when the rolling tail buffer truncates earlier output.
                    if virtio_blk_reset_marker_line is not None:
                        toks = virtio_blk_reset_marker_line.split("|")
                        status_tok = toks[3] if len(toks) >= 4 else ""
                        if not saw_virtio_blk_reset_pass and status_tok == "PASS":
                            saw_virtio_blk_reset_pass = True
                        if not saw_virtio_blk_reset_skip and status_tok == "SKIP":
                            saw_virtio_blk_reset_skip = True
                        if not saw_virtio_blk_reset_fail and status_tok == "FAIL":
                            saw_virtio_blk_reset_fail = True
                            if need_blk_reset:
                                print(
                                    _virtio_blk_reset_fail_failure_message(
                                        tail,
                                        marker_line=virtio_blk_reset_marker_line,
                                    ),
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break

                    if (
                        not saw_virtio_blk_reset_pass
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|PASS" in tail
                    ):
                        saw_virtio_blk_reset_pass = True
                    if (
                        not saw_virtio_blk_reset_skip
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|SKIP" in tail
                    ):
                        saw_virtio_blk_reset_skip = True
                    if (
                        not saw_virtio_blk_reset_fail
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|FAIL" in tail
                    ):
                        saw_virtio_blk_reset_fail = True

                        if need_blk_reset:
                            if saw_virtio_blk_reset_skip:
                                print(
                                    _virtio_blk_reset_skip_failure_message(
                                        tail,
                                        marker_line=virtio_blk_reset_marker_line,
                                    ),
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if saw_virtio_blk_reset_fail:
                                print(
                                    _virtio_blk_reset_fail_failure_message(
                                        tail,
                                        marker_line=virtio_blk_reset_marker_line,
                                    ),
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break

                    # Prefer the incrementally captured virtio-input marker line so we don't miss PASS/FAIL
                    # when the rolling tail buffer truncates earlier output.
                    if virtio_input_marker_line is not None:
                        status_tok = _try_extract_marker_status(virtio_input_marker_line)
                        if not saw_virtio_input_pass and status_tok == "PASS":
                            saw_virtio_input_pass = True
                            if virtio_input_marker_time is None:
                                virtio_input_marker_time = time.monotonic()
                        if not saw_virtio_input_fail and status_tok == "FAIL":
                            saw_virtio_input_fail = True
                            if virtio_input_marker_time is None:
                                virtio_input_marker_time = time.monotonic()
                    if not saw_virtio_input_pass and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input|PASS" in tail:
                        saw_virtio_input_pass = True
                        if virtio_input_marker_time is None:
                            virtio_input_marker_time = time.monotonic()
                    if not saw_virtio_input_fail and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input|FAIL" in tail:
                        saw_virtio_input_fail = True
                        if virtio_input_marker_time is None:
                            virtio_input_marker_time = time.monotonic()

                    # Prefer the incrementally captured virtio-input-bind marker line so we don't miss
                    # PASS/FAIL when the rolling tail buffer truncates earlier output.
                    if virtio_input_bind_marker_line is not None:
                        status_tok = _try_extract_marker_status(virtio_input_bind_marker_line)
                        if not saw_virtio_input_bind_pass and status_tok == "PASS":
                            saw_virtio_input_bind_pass = True
                        if not saw_virtio_input_bind_fail and status_tok == "FAIL":
                            saw_virtio_input_bind_fail = True
                    if (
                        not saw_virtio_input_bind_pass
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|PASS" in tail
                    ):
                        saw_virtio_input_bind_pass = True
                    if (
                        not saw_virtio_input_bind_fail
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|FAIL" in tail
                    ):
                        saw_virtio_input_bind_fail = True

                    # Prefer incrementally captured virtio-input sub-test markers so we don't miss them
                    # when the rolling tail buffer truncates earlier output (or when a large read chunk
                    # exceeds the tail cap).
                    if virtio_input_leds_marker_line is not None:
                        status_tok = _try_extract_marker_status(virtio_input_leds_marker_line)
                        if not saw_virtio_input_leds_pass and status_tok == "PASS":
                            saw_virtio_input_leds_pass = True
                        if not saw_virtio_input_leds_fail and status_tok == "FAIL":
                            saw_virtio_input_leds_fail = True
                        if not saw_virtio_input_leds_skip and status_tok == "SKIP":
                            saw_virtio_input_leds_skip = True

                    if virtio_input_events_marker_line is not None:
                        toks = virtio_input_events_marker_line.split("|")
                        status_tok = toks[3] if len(toks) >= 4 else ""
                        if not saw_virtio_input_events_ready and status_tok == "READY":
                            saw_virtio_input_events_ready = True
                        if not saw_virtio_input_events_pass and status_tok == "PASS":
                            saw_virtio_input_events_pass = True
                        if not saw_virtio_input_events_fail and status_tok == "FAIL":
                            saw_virtio_input_events_fail = True
                        if not saw_virtio_input_events_skip and status_tok == "SKIP":
                            saw_virtio_input_events_skip = True

                    if virtio_input_media_keys_marker_line is not None:
                        toks = virtio_input_media_keys_marker_line.split("|")
                        status_tok = toks[3] if len(toks) >= 4 else ""
                        if not saw_virtio_input_media_keys_ready and status_tok == "READY":
                            saw_virtio_input_media_keys_ready = True
                        if not saw_virtio_input_media_keys_pass and status_tok == "PASS":
                            saw_virtio_input_media_keys_pass = True
                        if not saw_virtio_input_media_keys_fail and status_tok == "FAIL":
                            saw_virtio_input_media_keys_fail = True
                        if not saw_virtio_input_media_keys_skip and status_tok == "SKIP":
                            saw_virtio_input_media_keys_skip = True

                    if virtio_input_led_marker_line is not None:
                        status_tok = _try_extract_marker_status(virtio_input_led_marker_line)
                        if not saw_virtio_input_led_pass and status_tok == "PASS":
                            saw_virtio_input_led_pass = True
                        if not saw_virtio_input_led_fail and status_tok == "FAIL":
                            saw_virtio_input_led_fail = True
                        if not saw_virtio_input_led_skip and status_tok == "SKIP":
                            saw_virtio_input_led_skip = True

                    if virtio_input_wheel_marker_line is not None:
                        status_tok = _try_extract_marker_status(virtio_input_wheel_marker_line)
                        if not saw_virtio_input_wheel_pass and status_tok == "PASS":
                            saw_virtio_input_wheel_pass = True
                        if not saw_virtio_input_wheel_fail and status_tok == "FAIL":
                            saw_virtio_input_wheel_fail = True
                        if not saw_virtio_input_wheel_skip and status_tok == "SKIP":
                            saw_virtio_input_wheel_skip = True

                    if virtio_input_events_modifiers_marker_line is not None:
                        status_tok = _try_extract_marker_status(virtio_input_events_modifiers_marker_line)
                        if not saw_virtio_input_events_modifiers_pass and status_tok == "PASS":
                            saw_virtio_input_events_modifiers_pass = True
                        if not saw_virtio_input_events_modifiers_fail and status_tok == "FAIL":
                            saw_virtio_input_events_modifiers_fail = True
                        if not saw_virtio_input_events_modifiers_skip and status_tok == "SKIP":
                            saw_virtio_input_events_modifiers_skip = True

                    if virtio_input_events_buttons_marker_line is not None:
                        status_tok = _try_extract_marker_status(virtio_input_events_buttons_marker_line)
                        if not saw_virtio_input_events_buttons_pass and status_tok == "PASS":
                            saw_virtio_input_events_buttons_pass = True
                        if not saw_virtio_input_events_buttons_fail and status_tok == "FAIL":
                            saw_virtio_input_events_buttons_fail = True
                        if not saw_virtio_input_events_buttons_skip and status_tok == "SKIP":
                            saw_virtio_input_events_buttons_skip = True

                    if virtio_input_events_wheel_marker_line is not None:
                        status_tok = _try_extract_marker_status(virtio_input_events_wheel_marker_line)
                        if not saw_virtio_input_events_wheel_pass and status_tok == "PASS":
                            saw_virtio_input_events_wheel_pass = True
                        if not saw_virtio_input_events_wheel_fail and status_tok == "FAIL":
                            saw_virtio_input_events_wheel_fail = True
                        if not saw_virtio_input_events_wheel_skip and status_tok == "SKIP":
                            saw_virtio_input_events_wheel_skip = True

                    if virtio_input_tablet_events_marker_line is not None:
                        toks = virtio_input_tablet_events_marker_line.split("|")
                        status_tok = toks[3] if len(toks) >= 4 else ""
                        if not saw_virtio_input_tablet_events_ready and status_tok == "READY":
                            saw_virtio_input_tablet_events_ready = True
                        if not saw_virtio_input_tablet_events_pass and status_tok == "PASS":
                            saw_virtio_input_tablet_events_pass = True
                        if not saw_virtio_input_tablet_events_fail and status_tok == "FAIL":
                            saw_virtio_input_tablet_events_fail = True
                        if not saw_virtio_input_tablet_events_skip and status_tok == "SKIP":
                            saw_virtio_input_tablet_events_skip = True
                    if (
                        not saw_virtio_input_leds_pass
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|PASS" in tail
                    ):
                        saw_virtio_input_leds_pass = True
                    if (
                        not saw_virtio_input_leds_fail
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|FAIL" in tail
                    ):
                        saw_virtio_input_leds_fail = True
                    if (
                        not saw_virtio_input_leds_skip
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|SKIP" in tail
                    ):
                        saw_virtio_input_leds_skip = True
                    if (
                        not saw_virtio_input_binding_pass
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|PASS" in tail
                    ):
                        saw_virtio_input_binding_pass = True
                    if (
                        not saw_virtio_input_binding_fail
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|FAIL" in tail
                    ):
                        saw_virtio_input_binding_fail = True
                    if (
                        not saw_virtio_input_binding_skip
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|SKIP" in tail
                    ):
                        saw_virtio_input_binding_skip = True
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
                        not saw_virtio_input_led_pass
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|PASS" in tail
                    ):
                        saw_virtio_input_led_pass = True
                    if (
                        not saw_virtio_input_led_fail
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|FAIL" in tail
                    ):
                        saw_virtio_input_led_fail = True
                    if (
                        not saw_virtio_input_led_skip
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|SKIP" in tail
                    ):
                        saw_virtio_input_led_skip = True
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

                    # If input LED/statusq testing is required, fail fast when the guest reports SKIP/FAIL
                    # for virtio-input-leds (e.g. the guest was provisioned without --test-input-leds).
                    if need_input_leds and (saw_virtio_input_leds_skip or saw_virtio_input_leds_fail):
                        msg = _virtio_input_leds_required_failure_message(
                            tail,
                            saw_pass=saw_virtio_input_leds_pass,
                            saw_fail=saw_virtio_input_leds_fail,
                            saw_skip=saw_virtio_input_leds_skip,
                            marker_line=virtio_input_leds_marker_line,
                        )
                        if msg is None:
                            raise AssertionError(
                                "need_input_leds is enabled and saw skip/fail, but required marker helper returned None"
                            )
                        print(msg, file=sys.stderr)
                        _print_tail(serial_log)
                        result_code = 1
                        break

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
                                _virtio_input_events_fail_failure_message(
                                    tail,
                                    marker_line=virtio_input_events_marker_line,
                                    req_flags_desc=input_events_req_flags_desc,
                                ),
                                file=sys.stderr,
                            )
                            _print_tail(serial_log)
                            result_code = 1
                            break

                    if need_input_media_keys:
                        if saw_virtio_input_media_keys_skip:
                            print(
                                "FAIL: VIRTIO_INPUT_MEDIA_KEYS_SKIPPED: virtio-input-media-keys test was skipped (flag_not_set) but "
                                "--with-input-media-keys/--with-virtio-input-media-keys/--require-virtio-input-media-keys/--enable-virtio-input-media-keys was enabled (provision the guest with --test-input-media-keys)",
                                file=sys.stderr,
                            )
                            _print_tail(serial_log)
                            result_code = 1
                            break
                        if saw_virtio_input_media_keys_fail:
                            print(
                                _virtio_input_media_keys_fail_failure_message(
                                    tail,
                                    marker_line=virtio_input_media_keys_marker_line,
                                ),
                                file=sys.stderr,
                            )
                            _print_tail(serial_log)
                            result_code = 1
                            break

                    if need_input_led:
                        if saw_virtio_input_led_skip:
                            print(
                                _virtio_input_led_skip_failure_message(
                                    tail,
                                    marker_line=virtio_input_led_marker_line,
                                ),
                                file=sys.stderr,
                            )
                            _print_tail(serial_log)
                            result_code = 1
                            break
                        if saw_virtio_input_led_fail:
                            print(
                                _virtio_input_led_fail_failure_message(
                                    tail,
                                    marker_line=virtio_input_led_marker_line,
                                ),
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
                            skipped = "virtio-input-events-*"
                            if saw_virtio_input_events_modifiers_skip:
                                skipped = "virtio-input-events-modifiers"
                            elif saw_virtio_input_events_buttons_skip:
                                skipped = "virtio-input-events-buttons"
                            elif saw_virtio_input_events_wheel_skip:
                                skipped = "virtio-input-events-wheel"
                            print(
                                f"FAIL: VIRTIO_INPUT_EVENTS_EXTENDED_SKIPPED: {skipped} was skipped (flag_not_set) but "
                                "--with-input-events-extended/--with-input-events-extra was enabled (provision the guest with --test-input-events-extended)",
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
                                _virtio_input_events_extended_fail_failure_message(
                                    tail,
                                    modifiers_marker_line=virtio_input_events_modifiers_marker_line,
                                    buttons_marker_line=virtio_input_events_buttons_marker_line,
                                    wheel_marker_line=virtio_input_events_wheel_marker_line,
                                ),
                                file=sys.stderr,
                            )
                            _print_tail(serial_log)
                            result_code = 1
                            break

                    if need_input_wheel:
                        if saw_virtio_input_wheel_skip:
                            print(
                                _virtio_input_wheel_skip_failure_message(
                                    tail,
                                    marker_line=virtio_input_wheel_marker_line,
                                ),
                                file=sys.stderr,
                            )
                            _print_tail(serial_log)
                            result_code = 1
                            break
                        if saw_virtio_input_wheel_fail:
                            print(
                                _virtio_input_wheel_fail_failure_message(
                                    tail,
                                    marker_line=virtio_input_wheel_marker_line,
                                ),
                                file=sys.stderr,
                            )
                            _print_tail(serial_log)
                            result_code = 1
                            break

                    if need_input_tablet_events:
                        if saw_virtio_input_tablet_events_skip:
                            print(
                                _virtio_input_tablet_events_skip_failure_message(
                                    tail,
                                    marker_line=virtio_input_tablet_events_marker_line,
                                ),
                                file=sys.stderr,
                            )
                            _print_tail(serial_log)
                            result_code = 1
                            break
                        if saw_virtio_input_tablet_events_fail:
                            print(
                                _virtio_input_tablet_events_fail_failure_message(
                                    tail,
                                    marker_line=virtio_input_tablet_events_marker_line,
                                ),
                                file=sys.stderr,
                            )
                            _print_tail(serial_log)
                            result_code = 1
                            break

                    if need_blk_reset:
                        if saw_virtio_blk_reset_skip:
                            print(
                                _virtio_blk_reset_skip_failure_message(
                                    tail,
                                    marker_line=virtio_blk_reset_marker_line,
                                ),
                                file=sys.stderr,
                            )
                            _print_tail(serial_log)
                            result_code = 1
                            break
                        if saw_virtio_blk_reset_fail:
                            print(
                                _virtio_blk_reset_fail_failure_message(
                                    tail,
                                    marker_line=virtio_blk_reset_marker_line,
                                ),
                                file=sys.stderr,
                            )
                            _print_tail(serial_log)
                            result_code = 1
                            break

                    if need_blk_resize:
                        if saw_virtio_blk_resize_skip:
                            print(
                                _virtio_blk_resize_skip_failure_message(
                                    tail,
                                    marker_line=virtio_blk_resize_marker_line,
                                ),
                                file=sys.stderr,
                            )
                            _print_tail(serial_log)
                            result_code = 1
                            break
                        if saw_virtio_blk_resize_fail:
                            print(
                                _virtio_blk_resize_fail_failure_message(
                                    tail,
                                    marker_line=virtio_blk_resize_marker_line,
                                ),
                                file=sys.stderr,
                            )
                            _print_tail(serial_log)
                            result_code = 1
                            break

                    # Prefer incrementally captured virtio-snd markers so we don't miss PASS/FAIL/SKIP when
                    # the rolling tail buffer truncates earlier output (or when a large read chunk exceeds
                    # the tail cap).
                    if virtio_snd_marker_line is not None:
                        status_tok = _try_extract_marker_status(virtio_snd_marker_line)
                        if not saw_virtio_snd_pass and status_tok == "PASS":
                            saw_virtio_snd_pass = True
                        if not saw_virtio_snd_skip and status_tok == "SKIP":
                            saw_virtio_snd_skip = True
                        if not saw_virtio_snd_fail and status_tok == "FAIL":
                            saw_virtio_snd_fail = True
                    if virtio_snd_capture_marker_line is not None:
                        status_tok = _try_extract_marker_status(virtio_snd_capture_marker_line)
                        if not saw_virtio_snd_capture_pass and status_tok == "PASS":
                            saw_virtio_snd_capture_pass = True
                        if not saw_virtio_snd_capture_skip and status_tok == "SKIP":
                            saw_virtio_snd_capture_skip = True
                        if not saw_virtio_snd_capture_fail and status_tok == "FAIL":
                            saw_virtio_snd_capture_fail = True
                    if virtio_snd_duplex_marker_line is not None:
                        status_tok = _try_extract_marker_status(virtio_snd_duplex_marker_line)
                        if not saw_virtio_snd_duplex_pass and status_tok == "PASS":
                            saw_virtio_snd_duplex_pass = True
                        if not saw_virtio_snd_duplex_skip and status_tok == "SKIP":
                            saw_virtio_snd_duplex_skip = True
                        if not saw_virtio_snd_duplex_fail and status_tok == "FAIL":
                            saw_virtio_snd_duplex_fail = True
                    if virtio_snd_buffer_limits_marker_line is not None:
                        status_tok = _try_extract_marker_status(virtio_snd_buffer_limits_marker_line)
                        if not saw_virtio_snd_buffer_limits_pass and status_tok == "PASS":
                            saw_virtio_snd_buffer_limits_pass = True
                        if not saw_virtio_snd_buffer_limits_skip and status_tok == "SKIP":
                            saw_virtio_snd_buffer_limits_skip = True
                        if not saw_virtio_snd_buffer_limits_fail and status_tok == "FAIL":
                            saw_virtio_snd_buffer_limits_fail = True

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

                    # Prefer incrementally captured virtio-net markers so we don't miss PASS/FAIL when
                    # the rolling tail buffer truncates earlier output (or when a large read chunk
                    # exceeds the tail cap).
                    if virtio_net_marker_line is not None:
                        status_tok = _try_extract_marker_status(virtio_net_marker_line)
                        if not saw_virtio_net_pass and status_tok == "PASS":
                            saw_virtio_net_pass = True
                            if virtio_net_marker_time is None:
                                virtio_net_marker_time = time.monotonic()
                        if not saw_virtio_net_fail and status_tok == "FAIL":
                            saw_virtio_net_fail = True
                            if virtio_net_marker_time is None:
                                virtio_net_marker_time = time.monotonic()

                    if virtio_net_udp_marker_line is not None:
                        status_tok = _try_extract_marker_status(virtio_net_udp_marker_line)
                        if not saw_virtio_net_udp_pass and status_tok == "PASS":
                            saw_virtio_net_udp_pass = True
                        if not saw_virtio_net_udp_fail and status_tok == "FAIL":
                            saw_virtio_net_udp_fail = True
                        if not saw_virtio_net_udp_skip and status_tok == "SKIP":
                            saw_virtio_net_udp_skip = True

                    if virtio_net_link_flap_marker_line is not None:
                        toks = virtio_net_link_flap_marker_line.split("|")
                        status_tok = toks[3] if len(toks) >= 4 else ""
                        if not saw_virtio_net_link_flap_ready and status_tok == "READY":
                            saw_virtio_net_link_flap_ready = True
                        if not saw_virtio_net_link_flap_pass and status_tok == "PASS":
                            saw_virtio_net_link_flap_pass = True
                        if not saw_virtio_net_link_flap_fail and status_tok == "FAIL":
                            saw_virtio_net_link_flap_fail = True
                        if not saw_virtio_net_link_flap_skip and status_tok == "SKIP":
                            saw_virtio_net_link_flap_skip = True
                    if not saw_virtio_net_pass and b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS" in tail:
                        saw_virtio_net_pass = True
                        if virtio_net_marker_time is None:
                            virtio_net_marker_time = time.monotonic()
                    if not saw_virtio_net_fail and b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|FAIL" in tail:
                        saw_virtio_net_fail = True
                        if virtio_net_marker_time is None:
                            virtio_net_marker_time = time.monotonic()
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
                    if (
                        not saw_virtio_net_link_flap_ready
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|READY" in tail
                    ):
                        saw_virtio_net_link_flap_ready = True
                    if (
                        not saw_virtio_net_link_flap_pass
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|PASS" in tail
                    ):
                        saw_virtio_net_link_flap_pass = True
                    if (
                        not saw_virtio_net_link_flap_fail
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|FAIL" in tail
                    ):
                        saw_virtio_net_link_flap_fail = True
                    if (
                        not saw_virtio_net_link_flap_skip
                        and b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|SKIP" in tail
                    ):
                        saw_virtio_net_link_flap_skip = True

                    # Fail fast when the guest reports SKIP/FAIL for virtio-net-link-flap. This saves
                    # CI time when the guest image was provisioned without --test-net-link-flap.
                    if need_net_link_flap and (
                        saw_virtio_net_link_flap_skip or saw_virtio_net_link_flap_fail
                    ):
                        msg = _virtio_net_link_flap_required_failure_message(
                            tail,
                            saw_pass=saw_virtio_net_link_flap_pass,
                            saw_fail=saw_virtio_net_link_flap_fail,
                            saw_skip=saw_virtio_net_link_flap_skip,
                            marker_line=virtio_net_link_flap_marker_line,
                        )
                        if msg is not None:
                            print(msg, file=sys.stderr)
                            _print_tail(serial_log)
                            result_code = 1
                            break

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

                    result_status: Optional[str] = None
                    if selftest_result_marker_line is not None:
                        result_status = _try_extract_marker_status(selftest_result_marker_line)
                    if result_status is None:
                        if b"AERO_VIRTIO_SELFTEST|RESULT|PASS" in tail:
                            result_status = "PASS"
                        elif b"AERO_VIRTIO_SELFTEST|RESULT|FAIL" in tail:
                            result_status = "FAIL"

                    if result_status == "PASS":
                        if args.require_expect_blk_msi:
                            expect_blk_msi_config, msg = _require_expect_blk_msi_config(
                                tail, expect_blk_msi_config=expect_blk_msi_config
                            )
                            if msg is not None:
                                print(msg, file=sys.stderr)
                                _print_tail(serial_log)
                                result_code = 1
                                break
                        if not require_per_test_markers and getattr(
                            args, "require_virtio_input_binding", False
                        ):
                            bind_fail = _check_required_virtio_input_bind_marker(
                                require_per_test_markers=True,
                                saw_pass=saw_virtio_input_bind_pass,
                                saw_fail=saw_virtio_input_bind_fail,
                            )
                            if bind_fail == "VIRTIO_INPUT_BIND_FAILED":
                                print(
                                    _virtio_input_bind_fail_failure_message(
                                        tail,
                                        marker_line=virtio_input_bind_marker_line,
                                    ),
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if bind_fail == "MISSING_VIRTIO_INPUT_BIND":
                                print(
                                    "FAIL: MISSING_VIRTIO_INPUT_BIND: selftest RESULT=PASS but did not emit virtio-input-bind test marker",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if require_per_test_markers:
                                # Require per-test markers so older selftest binaries cannot
                                # accidentally pass the host harness.
                                if saw_virtio_blk_fail:
                                    print(
                                        _virtio_blk_fail_failure_message(
                                            tail,
                                            marker_line=virtio_blk_marker_line,
                                        ),
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
                                        _virtio_blk_resize_fail_failure_message(
                                            tail,
                                            marker_line=virtio_blk_resize_marker_line,
                                        ),
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_blk_resize_pass:
                                    if saw_virtio_blk_resize_skip:
                                        print(
                                            _virtio_blk_resize_skip_failure_message(
                                                tail,
                                                marker_line=virtio_blk_resize_marker_line,
                                            ),
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(
                                            "FAIL: MISSING_VIRTIO_BLK_RESIZE: selftest RESULT=PASS but did not emit virtio-blk-resize test marker "
                                            "while --with-blk-resize/--with-virtio-blk-resize/--require-virtio-blk-resize/--enable-virtio-blk-resize was enabled",
                                            file=sys.stderr,
                                        )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if saw_virtio_input_fail:
                                print(
                                    _virtio_input_fail_failure_message(
                                        tail,
                                        marker_line=virtio_input_marker_line,
                                    ),
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
                            bind_fail = _check_required_virtio_input_bind_marker(
                                require_per_test_markers=require_per_test_markers,
                                saw_pass=saw_virtio_input_bind_pass,
                                saw_fail=saw_virtio_input_bind_fail,
                            )
                            if bind_fail == "VIRTIO_INPUT_BIND_FAILED":
                                print(
                                    _virtio_input_bind_fail_failure_message(
                                        tail,
                                        marker_line=virtio_input_bind_marker_line,
                                    )
                                    + " (see serial log for bound service name / ConfigManager error details)",
                                    file=sys.stderr,
                                )
                                _print_virtio_input_bind_diagnostics(serial_log)
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if bind_fail == "MISSING_VIRTIO_INPUT_BIND":
                                print(
                                    "FAIL: MISSING_VIRTIO_INPUT_BIND: selftest RESULT=PASS but did not emit virtio-input-bind test marker "
                                    "(guest selftest too old; update the image/selftest binary)",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if need_input_leds:
                                msg = _virtio_input_leds_required_failure_message(
                                    tail,
                                    saw_pass=saw_virtio_input_leds_pass,
                                    saw_fail=saw_virtio_input_leds_fail,
                                    saw_skip=saw_virtio_input_leds_skip,
                                    marker_line=virtio_input_leds_marker_line,
                                )
                                if msg is not None:
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if need_input_events:
                                if saw_virtio_input_events_fail:
                                    print(
                                        _virtio_input_events_fail_failure_message(
                                            tail,
                                            marker_line=virtio_input_events_marker_line,
                                            req_flags_desc=input_events_req_flags_desc,
                                        ),
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
                                        _virtio_input_media_keys_fail_failure_message(
                                            tail,
                                            marker_line=virtio_input_media_keys_marker_line,
                                        ),
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_input_media_keys_pass:
                                    if saw_virtio_input_media_keys_skip:
                                        print(
                                            "FAIL: VIRTIO_INPUT_MEDIA_KEYS_SKIPPED: virtio-input-media-keys test was skipped (flag_not_set) but "
                                            "--with-input-media-keys/--with-virtio-input-media-keys/--require-virtio-input-media-keys/--enable-virtio-input-media-keys was enabled (provision the guest with --test-input-media-keys)",
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(
                                            "FAIL: MISSING_VIRTIO_INPUT_MEDIA_KEYS: selftest RESULT=PASS but did not emit virtio-input-media-keys test marker "
                                            "while --with-input-media-keys/--with-virtio-input-media-keys/--require-virtio-input-media-keys/--enable-virtio-input-media-keys was enabled",
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
                                        _virtio_input_events_extended_fail_failure_message(
                                            tail,
                                            modifiers_marker_line=virtio_input_events_modifiers_marker_line,
                                            buttons_marker_line=virtio_input_events_buttons_marker_line,
                                            wheel_marker_line=virtio_input_events_wheel_marker_line,
                                        ),
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
                                            "--with-input-events-extended/--with-input-events-extra was enabled (provision the guest with --test-input-events-extended)",
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(
                                            f"FAIL: MISSING_VIRTIO_INPUT_EVENTS_EXTENDED: did not observe {name} PASS marker while "
                                            "--with-input-events-extended/--with-input-events-extra was enabled",
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
                                        _virtio_input_wheel_fail_failure_message(
                                            tail,
                                            marker_line=virtio_input_wheel_marker_line,
                                        ),
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_input_wheel_pass:
                                    if saw_virtio_input_wheel_skip:
                                        print(
                                            _virtio_input_wheel_skip_failure_message(
                                                tail,
                                                marker_line=virtio_input_wheel_marker_line,
                                            ),
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(
                                            "FAIL: MISSING_VIRTIO_INPUT_WHEEL: selftest RESULT=PASS but did not emit virtio-input-wheel test marker "
                                            "while --with-input-wheel/--with-virtio-input-wheel/--require-virtio-input-wheel/--enable-virtio-input-wheel was enabled",
                                            file=sys.stderr,
                                        )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break

                            if need_input_tablet_events:
                                if saw_virtio_input_tablet_events_fail:
                                    print(
                                        _virtio_input_tablet_events_fail_failure_message(
                                            tail,
                                            marker_line=virtio_input_tablet_events_marker_line,
                                        ),
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_input_tablet_events_pass:
                                    if saw_virtio_input_tablet_events_skip:
                                        print(
                                            _virtio_input_tablet_events_skip_failure_message(
                                                tail,
                                                marker_line=virtio_input_tablet_events_marker_line,
                                            ),
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(
                                            "FAIL: MISSING_VIRTIO_INPUT_TABLET_EVENTS: selftest RESULT=PASS but did not emit virtio-input-tablet-events test marker "
                                            "while --with-input-tablet-events/--with-tablet-events/--with-virtio-input-tablet-events/--require-virtio-input-tablet-events/--enable-virtio-input-tablet-events was enabled",
                                            file=sys.stderr,
                                        )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if saw_virtio_snd_fail:
                                print(
                                    _virtio_snd_fail_failure_message(
                                        tail,
                                        marker_line=virtio_snd_marker_line,
                                    ),
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break

                            if args.enable_virtio_snd:
                                # When we explicitly attach virtio-snd, the guest test must actually run and PASS
                                # (it must not be skipped via --disable-snd).
                                if not saw_virtio_snd_pass:
                                    msg = "FAIL: MISSING_VIRTIO_SND: virtio-snd test did not PASS while --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"
                                    if saw_virtio_snd_skip:
                                        msg = _virtio_snd_skip_failure_message(
                                            tail,
                                            marker_line=virtio_snd_marker_line,
                                            skip_reason=virtio_snd_skip_reason,
                                        )
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if saw_virtio_snd_capture_fail:
                                    print(
                                        _virtio_snd_capture_fail_failure_message(
                                            tail,
                                            marker_line=virtio_snd_capture_marker_line,
                                        ),
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_snd_capture_pass:
                                    msg = "FAIL: MISSING_VIRTIO_SND_CAPTURE: virtio-snd capture test did not PASS while --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"
                                    if saw_virtio_snd_capture_skip:
                                        msg = _virtio_snd_capture_skip_failure_message(
                                            tail,
                                            marker_line=virtio_snd_capture_marker_line,
                                        )
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if saw_virtio_snd_duplex_fail:
                                    print(
                                        _virtio_snd_duplex_fail_failure_message(
                                            tail,
                                            marker_line=virtio_snd_duplex_marker_line,
                                        ),
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_snd_duplex_pass:
                                    if saw_virtio_snd_duplex_skip:
                                        msg = _virtio_snd_duplex_skip_failure_message(
                                            tail,
                                            marker_line=virtio_snd_duplex_marker_line,
                                        )
                                    else:
                                        msg = (
                                            "FAIL: MISSING_VIRTIO_SND_DUPLEX: selftest RESULT=PASS but did not emit virtio-snd-duplex test marker "
                                            "while --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"
                                        )
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break

                                if args.with_snd_buffer_limits:
                                    msg = _virtio_snd_buffer_limits_required_failure_message(
                                        tail,
                                        saw_pass=saw_virtio_snd_buffer_limits_pass,
                                        saw_fail=saw_virtio_snd_buffer_limits_fail,
                                        saw_skip=saw_virtio_snd_buffer_limits_skip,
                                        marker_line=virtio_snd_buffer_limits_marker_line,
                                    )
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
                                        _virtio_snd_capture_fail_failure_message(
                                            tail,
                                            marker_line=virtio_snd_capture_marker_line,
                                        ),
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
                                        _virtio_snd_duplex_fail_failure_message(
                                            tail,
                                            marker_line=virtio_snd_duplex_marker_line,
                                        ),
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
                                    _virtio_net_fail_failure_message(
                                        tail,
                                        marker_line=virtio_net_marker_line,
                                    ),
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
                                        _virtio_net_udp_fail_failure_message(
                                            tail,
                                            marker_line=virtio_net_udp_marker_line,
                                        ),
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
                            if need_net_link_flap:
                                msg = _virtio_net_link_flap_required_failure_message(
                                    tail,
                                    saw_pass=saw_virtio_net_link_flap_pass,
                                    saw_fail=saw_virtio_net_link_flap_fail,
                                    saw_skip=saw_virtio_net_link_flap_skip,
                                    marker_line=virtio_net_link_flap_marker_line,
                                )
                                if msg is not None:
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                        elif args.enable_virtio_snd:
                            # Transitional mode: don't require virtio-input markers, but if the caller
                            # explicitly attached virtio-snd, require the virtio-snd marker to avoid
                            # false positives.
                            if saw_virtio_snd_fail:
                                print(
                                    _virtio_snd_fail_failure_message(
                                        tail,
                                        marker_line=virtio_snd_marker_line,
                                    ),
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if not saw_virtio_snd_pass:
                                msg = "FAIL: MISSING_VIRTIO_SND: virtio-snd test did not PASS while --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"
                                if saw_virtio_snd_skip:
                                    msg = _virtio_snd_skip_failure_message(
                                        tail,
                                        marker_line=virtio_snd_marker_line,
                                        skip_reason=virtio_snd_skip_reason,
                                    )
                                print(msg, file=sys.stderr)
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if saw_virtio_snd_capture_fail:
                                print(
                                    _virtio_snd_capture_fail_failure_message(
                                        tail,
                                        marker_line=virtio_snd_capture_marker_line,
                                    ),
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if not saw_virtio_snd_capture_pass:
                                msg = "FAIL: MISSING_VIRTIO_SND_CAPTURE: virtio-snd capture test did not PASS while --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"
                                if saw_virtio_snd_capture_skip:
                                    msg = _virtio_snd_capture_skip_failure_message(
                                        tail,
                                        marker_line=virtio_snd_capture_marker_line,
                                    )
                                print(msg, file=sys.stderr)
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if saw_virtio_snd_duplex_fail:
                                print(
                                    _virtio_snd_duplex_fail_failure_message(
                                        tail,
                                        marker_line=virtio_snd_duplex_marker_line,
                                    ),
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if not saw_virtio_snd_duplex_pass:
                                if saw_virtio_snd_duplex_skip:
                                    msg = _virtio_snd_duplex_skip_failure_message(
                                        tail,
                                        marker_line=virtio_snd_duplex_marker_line,
                                    )
                                else:
                                    msg = (
                                        "FAIL: MISSING_VIRTIO_SND_DUPLEX: selftest RESULT=PASS but did not emit virtio-snd-duplex test marker "
                                        "while --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"
                                    )
                                print(msg, file=sys.stderr)
                                _print_tail(serial_log)
                                result_code = 1
                                break

                            if args.with_snd_buffer_limits:
                                msg = _virtio_snd_buffer_limits_required_failure_message(
                                    tail,
                                    saw_pass=saw_virtio_snd_buffer_limits_pass,
                                    saw_fail=saw_virtio_snd_buffer_limits_fail,
                                    saw_skip=saw_virtio_snd_buffer_limits_skip,
                                    marker_line=virtio_snd_buffer_limits_marker_line,
                                )
                                if msg is not None:
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break

                        if need_blk_reset:
                            if saw_virtio_blk_reset_fail:
                                print(
                                    _virtio_blk_reset_fail_failure_message(
                                        tail,
                                        marker_line=virtio_blk_reset_marker_line,
                                    ),
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if not saw_virtio_blk_reset_pass:
                                if saw_virtio_blk_reset_skip:
                                    print(
                                        _virtio_blk_reset_skip_failure_message(
                                            tail,
                                            marker_line=virtio_blk_reset_marker_line,
                                        ),
                                        file=sys.stderr,
                                    )
                                else:
                                    print(_virtio_blk_reset_missing_failure_message(), file=sys.stderr)
                                _print_tail(serial_log)
                                result_code = 1
                                break
                        if need_input_leds:
                            msg = _virtio_input_leds_required_failure_message(
                                tail,
                                saw_pass=saw_virtio_input_leds_pass,
                                saw_fail=saw_virtio_input_leds_fail,
                                saw_skip=saw_virtio_input_leds_skip,
                                marker_line=virtio_input_leds_marker_line,
                            )
                            if msg is not None:
                                print(msg, file=sys.stderr)
                                _print_tail(serial_log)
                                result_code = 1
                                break
                        if need_input_events:
                            if saw_virtio_input_events_fail:
                                print(
                                    _virtio_input_events_fail_failure_message(
                                        tail,
                                        marker_line=virtio_input_events_marker_line,
                                        req_flags_desc=input_events_req_flags_desc,
                                    ),
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
                                    _virtio_input_media_keys_fail_failure_message(
                                        tail,
                                        marker_line=virtio_input_media_keys_marker_line,
                                    ),
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if not saw_virtio_input_media_keys_pass:
                                if saw_virtio_input_media_keys_skip:
                                    print(
                                        "FAIL: VIRTIO_INPUT_MEDIA_KEYS_SKIPPED: virtio-input-media-keys test was skipped (flag_not_set) but "
                                        "--with-input-media-keys/--with-virtio-input-media-keys/--require-virtio-input-media-keys/--enable-virtio-input-media-keys was enabled (provision the guest with --test-input-media-keys)",
                                        file=sys.stderr,
                                    )
                                else:
                                    print(
                                        "FAIL: MISSING_VIRTIO_INPUT_MEDIA_KEYS: did not observe virtio-input-media-keys PASS marker while "
                                        "--with-input-media-keys/--with-virtio-input-media-keys/--require-virtio-input-media-keys/--enable-virtio-input-media-keys was enabled",
                                        file=sys.stderr,
                                    )
                                _print_tail(serial_log)
                                result_code = 1
                                break

                        if need_blk_resize:
                            if saw_virtio_blk_resize_fail:
                                print(
                                    _virtio_blk_resize_fail_failure_message(
                                        tail,
                                        marker_line=virtio_blk_resize_marker_line,
                                    ),
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if not saw_virtio_blk_resize_pass:
                                if saw_virtio_blk_resize_skip:
                                    print(
                                        _virtio_blk_resize_skip_failure_message(
                                            tail,
                                            marker_line=virtio_blk_resize_marker_line,
                                        ),
                                        file=sys.stderr,
                                    )
                                else:
                                    print(
                                        "FAIL: MISSING_VIRTIO_BLK_RESIZE: did not observe virtio-blk-resize PASS marker while --with-blk-resize/--with-virtio-blk-resize/--require-virtio-blk-resize/--enable-virtio-blk-resize was enabled",
                                        file=sys.stderr,
                                    )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                        if need_net_link_flap:
                            msg = _virtio_net_link_flap_required_failure_message(
                                tail,
                                saw_pass=saw_virtio_net_link_flap_pass,
                                saw_fail=saw_virtio_net_link_flap_fail,
                                saw_skip=saw_virtio_net_link_flap_skip,
                                marker_line=virtio_net_link_flap_marker_line,
                            )
                            if msg is not None:
                                print(msg, file=sys.stderr)
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
                                    _virtio_input_events_extended_fail_failure_message(
                                        tail,
                                        modifiers_marker_line=virtio_input_events_modifiers_marker_line,
                                        buttons_marker_line=virtio_input_events_buttons_marker_line,
                                        wheel_marker_line=virtio_input_events_wheel_marker_line,
                                    ),
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
                                        "--with-input-events-extended/--with-input-events-extra was enabled (provision the guest with --test-input-events-extended)",
                                        file=sys.stderr,
                                    )
                                else:
                                    print(
                                        f"FAIL: MISSING_VIRTIO_INPUT_EVENTS_EXTENDED: did not observe {name} PASS marker while "
                                        "--with-input-events-extended/--with-input-events-extra was enabled",
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
                                    _virtio_input_wheel_fail_failure_message(
                                        tail,
                                        marker_line=virtio_input_wheel_marker_line,
                                    ),
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if not saw_virtio_input_wheel_pass:
                                if saw_virtio_input_wheel_skip:
                                    print(
                                        _virtio_input_wheel_skip_failure_message(
                                            tail,
                                            marker_line=virtio_input_wheel_marker_line,
                                        ),
                                        file=sys.stderr,
                                    )
                                else:
                                    print(
                                        "FAIL: MISSING_VIRTIO_INPUT_WHEEL: did not observe virtio-input-wheel PASS marker while "
                                        "--with-input-wheel/--with-virtio-input-wheel/--require-virtio-input-wheel/--enable-virtio-input-wheel was enabled",
                                        file=sys.stderr,
                                    )
                                _print_tail(serial_log)
                                result_code = 1
                                break

                        if need_input_tablet_events:
                            if saw_virtio_input_tablet_events_fail:
                                print(
                                    _virtio_input_tablet_events_fail_failure_message(
                                        tail,
                                        marker_line=virtio_input_tablet_events_marker_line,
                                    ),
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                            if not saw_virtio_input_tablet_events_pass:
                                if saw_virtio_input_tablet_events_skip:
                                    print(
                                        _virtio_input_tablet_events_skip_failure_message(
                                            tail,
                                            marker_line=virtio_input_tablet_events_marker_line,
                                        ),
                                        file=sys.stderr,
                                    )
                                else:
                                    print(
                                        "FAIL: MISSING_VIRTIO_INPUT_TABLET_EVENTS: did not observe virtio-input-tablet-events PASS marker while --with-input-tablet-events/--with-tablet-events/--with-virtio-input-tablet-events/--require-virtio-input-tablet-events/--enable-virtio-input-tablet-events was enabled",
                                        file=sys.stderr,
                                    )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                        if need_blk_reset:
                            try:
                                reset_tail = serial_log.read_bytes()
                            except Exception:
                                reset_tail = tail
                            msg = _virtio_blk_reset_required_failure_message(
                                reset_tail,
                                saw_pass=saw_virtio_blk_reset_pass,
                                saw_fail=saw_virtio_blk_reset_fail,
                                saw_skip=saw_virtio_blk_reset_skip,
                                marker_line=virtio_blk_reset_marker_line,
                            )
                            if msg is not None:
                                print(msg, file=sys.stderr)
                                _print_tail(serial_log)
                                result_code = 1
                                break

                        if need_net_link_flap:
                            msg = _virtio_net_link_flap_required_failure_message(
                                tail,
                                saw_pass=saw_virtio_net_link_flap_pass,
                                saw_fail=saw_virtio_net_link_flap_fail,
                                saw_skip=saw_virtio_net_link_flap_skip,
                                marker_line=virtio_net_link_flap_marker_line,
                            )
                            if msg is not None:
                                print(msg, file=sys.stderr)
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
                        if args.require_virtio_net_msix:
                            msix_tail = (
                                virtio_net_msix_marker_line.encode("utf-8")
                                if virtio_net_msix_marker_line is not None
                                else tail
                            )
                            ok, reason = _require_virtio_net_msix_marker(msix_tail)
                            if not ok:
                                if reason.startswith("missing virtio-net-msix marker"):
                                    print(
                                        "FAIL: MISSING_VIRTIO_NET_MSIX: did not observe virtio-net-msix marker while "
                                        "--require-virtio-net-msix/--require-net-msix was enabled (guest selftest too old?)",
                                        file=sys.stderr,
                                    )
                                else:
                                    print(
                                        "FAIL: VIRTIO_NET_MSIX_REQUIRED: "
                                        f"{reason} (while --require-virtio-net-msix/--require-net-msix was enabled)",
                                        file=sys.stderr,
                                    )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                        if args.require_virtio_blk_msix:
                            msix_tail = (
                                virtio_blk_msix_marker_line.encode("utf-8")
                                if virtio_blk_msix_marker_line is not None
                                else tail
                            )
                            ok, reason = _require_virtio_blk_msix_marker(msix_tail)
                            if not ok:
                                print(
                                    "FAIL: VIRTIO_BLK_MSIX_REQUIRED: "
                                    f"{reason} (while --require-virtio-blk-msix/--require-blk-msix was enabled)",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                        if args.require_virtio_snd_msix:
                            msix_tail = (
                                virtio_snd_msix_marker_line.encode("utf-8")
                                if virtio_snd_msix_marker_line is not None
                                else tail
                            )
                            ok, reason = _require_virtio_snd_msix_marker(msix_tail)
                            if not ok:
                                print(
                                    "FAIL: VIRTIO_SND_MSIX_REQUIRED: "
                                    f"{reason} (while --require-virtio-snd-msix/--require-snd-msix was enabled)",
                                    file=sys.stderr,
                                )
                                _print_tail(serial_log)
                                result_code = 1
                                break

                        if need_input_led:
                            msg = _virtio_input_led_required_failure_message(
                                tail,
                                saw_pass=saw_virtio_input_led_pass,
                                saw_fail=saw_virtio_input_led_fail,
                                saw_skip=saw_virtio_input_led_skip,
                                marker_line=virtio_input_led_marker_line,
                            )
                            if msg is not None:
                                print(msg, file=sys.stderr)
                                _print_tail(serial_log)
                                result_code = 1
                                break

                        if bool(args.require_virtio_input_msix):
                            # Prefer the parsed marker line if we captured it earlier so we don't rely on
                            # the rolling tail buffer still containing the marker.
                            msix_tail = (
                                virtio_input_msix_marker.line.encode("utf-8")
                                if virtio_input_msix_marker is not None
                                else (
                                    virtio_input_msix_marker_line.encode("utf-8")
                                    if virtio_input_msix_marker_line is not None
                                    else tail
                                )
                            )
                            ok, reason = _require_virtio_input_msix_marker(msix_tail)
                            if not ok:
                                if reason.startswith("missing virtio-input-msix marker"):
                                    print(
                                        "FAIL: MISSING_VIRTIO_INPUT_MSIX: did not observe virtio-input-msix marker while "
                                        "--require-virtio-input-msix/--require-input-msix was enabled (guest selftest too old?)",
                                        file=sys.stderr,
                                    )
                                else:
                                    print(
                                        "FAIL: VIRTIO_INPUT_MSIX_REQUIRED: "
                                        f"{reason} (while --require-virtio-input-msix/--require-input-msix was enabled)",
                                        file=sys.stderr,
                                    )
                                _print_tail(serial_log)
                                result_code = 1
                                break
                        if bool(getattr(args, "require_virtio_input_binding", False)):
                            msg = _virtio_input_binding_required_failure_message(
                                tail,
                                saw_pass=saw_virtio_input_binding_pass,
                                saw_fail=saw_virtio_input_binding_fail,
                                saw_skip=saw_virtio_input_binding_skip,
                                marker_line=virtio_input_binding_marker_line,
                            )
                            if msg is not None:
                                print(msg, file=sys.stderr)
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
                        if args.require_no_blk_recovery:
                            msg = _check_no_blk_recovery_requirement(
                                tail,
                                blk_test_line=virtio_blk_marker_line,
                                blk_counters_line=virtio_blk_counters_marker_line,
                            )
                            if msg is not None:
                                print(msg, file=sys.stderr)
                                _print_tail(serial_log)
                                result_code = 1
                                break
                        if args.fail_on_blk_recovery:
                            msg = _check_fail_on_blk_recovery_requirement(
                                tail,
                                blk_test_line=virtio_blk_marker_line,
                                blk_counters_line=virtio_blk_counters_marker_line,
                            )
                            if msg is not None:
                                print(msg, file=sys.stderr)
                                _print_tail(serial_log)
                                result_code = 1
                                break
                        if args.require_no_blk_reset_recovery:
                            msg = _check_no_blk_reset_recovery_requirement(
                                tail,
                                blk_reset_recovery_line=(
                                    virtio_blk_reset_recovery_marker_line
                                    or virtio_blk_miniport_reset_recovery_marker_line
                                ),
                            )
                            if msg is not None:
                                print(msg, file=sys.stderr)
                                _print_tail(serial_log)
                                result_code = 1
                                break
                        if args.fail_on_blk_reset_recovery:
                            msg = _check_fail_on_blk_reset_recovery_requirement(
                                tail,
                                blk_reset_recovery_line=(
                                    virtio_blk_reset_recovery_marker_line
                                    or virtio_blk_miniport_reset_recovery_marker_line
                                ),
                            )
                            if msg is not None:
                                print(msg, file=sys.stderr)
                                _print_tail(serial_log)
                                result_code = 1
                                break
                        if args.require_no_blk_miniport_flags:
                            msg = _check_no_blk_miniport_flags_requirement(
                                tail,
                                blk_miniport_flags_line=virtio_blk_miniport_flags_marker_line,
                            )
                            if msg is not None:
                                print(msg, file=sys.stderr)
                                _print_tail(serial_log)
                                result_code = 1
                                break
                        if args.fail_on_blk_miniport_flags:
                            msg = _check_fail_on_blk_miniport_flags_requirement(
                                tail,
                                blk_miniport_flags_line=virtio_blk_miniport_flags_marker_line,
                            )
                            if msg is not None:
                                print(msg, file=sys.stderr)
                                _print_tail(serial_log)
                                result_code = 1
                                break

                        if args.require_net_csum_offload or getattr(args, "require_net_udp_csum_offload", False):
                            csum_tail = (
                                virtio_net_offload_csum_marker_line.encode("utf-8")
                                if virtio_net_offload_csum_marker_line is not None
                                else tail
                            )
                            stats = _extract_virtio_net_offload_csum_stats(csum_tail)
                            if stats is None:
                                if args.require_net_csum_offload:
                                    print(
                                        "FAIL: MISSING_VIRTIO_NET_CSUM_OFFLOAD: missing virtio-net-offload-csum marker while "
                                        "--require-net-csum-offload/--require-virtio-net-csum-offload was enabled",
                                        file=sys.stderr,
                                    )
                                else:
                                    print(
                                        "FAIL: MISSING_VIRTIO_NET_UDP_CSUM_OFFLOAD: missing virtio-net-offload-csum marker while "
                                        "--require-net-udp-csum-offload/--require-virtio-net-udp-csum-offload was enabled",
                                        file=sys.stderr,
                                    )
                                _print_tail(serial_log)
                                result_code = 1
                                break

                            if stats.get("status") != "PASS":
                                if args.require_net_csum_offload:
                                    print(
                                        "FAIL: VIRTIO_NET_CSUM_OFFLOAD_FAILED: virtio-net-offload-csum marker did not PASS "
                                        f"(status={stats.get('status')})",
                                        file=sys.stderr,
                                    )
                                else:
                                    print(
                                        "FAIL: VIRTIO_NET_UDP_CSUM_OFFLOAD_FAILED: virtio-net-offload-csum marker did not PASS "
                                        f"(status={stats.get('status')})",
                                        file=sys.stderr,
                                    )
                                _print_tail(serial_log)
                                result_code = 1
                                break

                            if args.require_net_csum_offload:
                                tx_csum = stats.get("tx_csum")
                                if tx_csum is None:
                                    print(
                                        "FAIL: VIRTIO_NET_CSUM_OFFLOAD_MISSING_FIELDS: virtio-net-offload-csum marker missing tx_csum field",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break

                                if int(tx_csum) <= 0:
                                    rx_csum = stats.get("rx_csum")
                                    fallback = stats.get("fallback")
                                    print(
                                        "FAIL: VIRTIO_NET_CSUM_OFFLOAD_ZERO: checksum offload requirement not met "
                                        f"(tx_csum={tx_csum} rx_csum={rx_csum} fallback={fallback})",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break

                            if getattr(args, "require_net_udp_csum_offload", False):
                                tx_udp = stats.get("tx_udp")
                                tx_udp4 = stats.get("tx_udp4")
                                tx_udp6 = stats.get("tx_udp6")
                                if tx_udp is None:
                                    if tx_udp4 is None and tx_udp6 is None:
                                        print(
                                            "FAIL: VIRTIO_NET_UDP_CSUM_OFFLOAD_MISSING_FIELDS: virtio-net-offload-csum marker missing tx_udp/tx_udp4/tx_udp6 fields",
                                            file=sys.stderr,
                                        )
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                                    tx_udp = int(tx_udp4 or 0) + int(tx_udp6 or 0)

                                if int(tx_udp) <= 0:
                                    fallback = stats.get("fallback")
                                    print(
                                        "FAIL: VIRTIO_NET_UDP_CSUM_OFFLOAD_ZERO: UDP checksum offload requirement not met "
                                        f"(tx_udp={tx_udp} tx_udp4={tx_udp4} tx_udp6={tx_udp6} fallback={fallback})",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                        print("PASS: AERO_VIRTIO_SELFTEST|RESULT|PASS")
                        result_code = 0
                        break
                    if result_status == "FAIL":
                        if args.require_expect_blk_msi:
                            expect_blk_msi_config, msg = _require_expect_blk_msi_config(
                                tail, expect_blk_msi_config=expect_blk_msi_config
                            )
                            if msg is not None:
                                print(msg, file=sys.stderr)
                                _print_tail(serial_log)
                                result_code = 1
                                break
                        msg = _try_virtio_snd_force_null_backend_failure_message(tail)
                        if msg is not None:
                            print(msg)
                        else:
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
                        "--with-blk-resize/--with-virtio-blk-resize/--require-virtio-blk-resize/--enable-virtio-blk-resize was enabled (guest selftest too old or missing --test-blk-resize)",
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
                            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESIZE|FAIL|"
                            f"reason={reason}|old_bytes={int(blk_resize_old_bytes)}|new_bytes={int(blk_resize_new_bytes)}|drive_id=drive0",
                            file=sys.stderr,
                        )
                        print(
                            f"FAIL: QMP_BLK_RESIZE_FAILED: failed to resize virtio-blk device via QMP: {e}",
                            file=sys.stderr,
                        )
                        _print_tail(serial_log)
                        result_code = 1
                        break

                if (
                    need_net_link_flap
                    and virtio_net_marker_time is not None
                    and not saw_virtio_net_link_flap_ready
                    and not saw_virtio_net_link_flap_pass
                    and not saw_virtio_net_link_flap_fail
                    and not saw_virtio_net_link_flap_skip
                    and time.monotonic() - virtio_net_marker_time > 20.0
                ):
                    print(
                        "FAIL: MISSING_VIRTIO_NET_LINK_FLAP: did not observe virtio-net-link-flap marker after virtio-net completed while "
                        "--with-net-link-flap/--with-virtio-net-link-flap/--require-virtio-net-link-flap/--enable-virtio-net-link-flap was enabled (guest selftest too old or missing --test-net-link-flap)",
                        file=sys.stderr,
                    )
                    _print_tail(serial_log)
                    result_code = 1
                    break

                # If the guest never emits READY/SKIP/PASS/FAIL after completing virtio-input, assume the
                # guest selftest is too old (or misconfigured) and fail early to avoid burning the full
                # virtio-net timeout.
                if (
                    need_input_leds
                    and virtio_input_marker_time is not None
                    and not saw_virtio_input_leds_pass
                    and not saw_virtio_input_leds_fail
                    and not saw_virtio_input_leds_skip
                    and time.monotonic() - virtio_input_marker_time > 20.0
                ):
                    msg = _virtio_input_leds_required_failure_message(
                        tail,
                        saw_pass=saw_virtio_input_leds_pass,
                        saw_fail=saw_virtio_input_leds_fail,
                        saw_skip=saw_virtio_input_leds_skip,
                        marker_line=virtio_input_leds_marker_line,
                    )
                    if msg is None:
                        raise AssertionError(
                            "need_input_leds is enabled and marker is missing, but required marker helper returned None"
                        )
                    print(msg, file=sys.stderr)
                    _print_tail(serial_log)
                    result_code = 1
                    break

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
                        "--with-input-media-keys/--with-virtio-input-media-keys/--require-virtio-input-media-keys/--enable-virtio-input-media-keys was enabled (guest selftest too old or missing --test-input-media-keys)",
                        file=sys.stderr,
                    )
                    _print_tail(serial_log)
                    result_code = 1
                    break

                if (
                    need_input_led
                    and virtio_input_marker_time is not None
                    and not saw_virtio_input_led_pass
                    and not saw_virtio_input_led_fail
                    and not saw_virtio_input_led_skip
                    and time.monotonic() - virtio_input_marker_time > 20.0
                ):
                    print(
                        "FAIL: MISSING_VIRTIO_INPUT_LED: did not observe virtio-input-led marker after virtio-input completed while "
                        "--with-input-led/--with-virtio-input-led/--require-virtio-input-led/--enable-virtio-input-led was enabled (guest selftest too old or missing --test-input-led)",
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
                        "--with-input-tablet-events/--with-tablet-events/--with-virtio-input-tablet-events/--require-virtio-input-tablet-events/--enable-virtio-input-tablet-events was enabled (guest selftest too old or missing --test-input-tablet-events/--test-tablet-events)",
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
                        _emit_virtio_input_events_inject_host_marker(
                            ok=True,
                            attempt=input_events_inject_attempts,
                            backend=info.backend,
                            kbd_mode=kbd_mode,
                            mouse_mode=mouse_mode,
                        )
                    except Exception as e:
                        _emit_virtio_input_events_inject_host_marker(
                            ok=False,
                            attempt=input_events_inject_attempts,
                            reason=str(e) or type(e).__name__,
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
                        _emit_virtio_input_media_keys_inject_host_marker(
                            ok=True,
                            attempt=input_media_keys_inject_attempts,
                            backend=info.backend,
                            kbd_mode=kbd_mode,
                        )
                    except Exception as e:
                        _emit_virtio_input_media_keys_inject_host_marker(
                            ok=False,
                            attempt=input_media_keys_inject_attempts,
                            reason=str(e) or type(e).__name__,
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
                        _emit_virtio_input_tablet_events_inject_host_marker(
                            ok=True,
                            attempt=input_tablet_events_inject_attempts,
                            backend=info.backend,
                            tablet_mode=tablet_mode,
                        )
                    except Exception as e:
                        _emit_virtio_input_tablet_events_inject_host_marker(
                            ok=False,
                            attempt=input_tablet_events_inject_attempts,
                            reason=str(e) or type(e).__name__,
                        )
                        print(
                            f"FAIL: QMP_INPUT_TABLET_INJECT_FAILED: failed to inject virtio-input tablet events via QMP: {e}",
                            file=sys.stderr,
                        )
                        _print_tail(serial_log)
                        result_code = 1
                        break

                # When requested, flap the virtio-net link after the guest has emitted the READY marker.
                # This is a one-shot deterministic sequence (down for 3s, then up).
                if (
                    need_net_link_flap
                    and saw_virtio_net_link_flap_ready
                    and not did_net_link_flap
                    and not saw_virtio_net_link_flap_pass
                    and not saw_virtio_net_link_flap_fail
                    and not saw_virtio_net_link_flap_skip
                ):
                    down_delay_sec = 3
                    if qmp_endpoint is None:
                        print(
                            f"AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_LINK_FLAP|FAIL|name={_VIRTIO_NET_QMP_ID}|down_delay_sec={down_delay_sec}|reason=no_qmp",
                            file=sys.stderr,
                        )
                        print(
                            "FAIL: QMP_NET_LINK_FLAP_FAILED: --with-net-link-flap/--with-virtio-net-link-flap/--require-virtio-net-link-flap/--enable-virtio-net-link-flap requires QMP, but QMP was not enabled",
                            file=sys.stderr,
                        )
                        _print_tail(serial_log)
                        result_code = 1
                        break
                    did_net_link_flap = True
                    try:
                        name_used = _try_qmp_net_link_flap(
                            qmp_endpoint,
                            names=[_VIRTIO_NET_QMP_ID, "net0"],
                            down_delay_seconds=float(down_delay_sec),
                        )
                        print(
                            "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_LINK_FLAP|PASS|"
                            f"name={_sanitize_marker_value(name_used)}|down_delay_sec={down_delay_sec}"
                        )
                    except Exception as e:
                        name_used = getattr(e, "name_used", None)
                        if not name_used:
                            # If we failed before determining which name QMP accepts (or before toggling
                            # the link down), include the attempted names list for debugging.
                            name_used = ",".join([_VIRTIO_NET_QMP_ID, "net0"])
                        name_tok = _sanitize_marker_value(str(name_used))
                        reason = _sanitize_marker_value(str(e) or type(e).__name__)
                        print(
                            f"AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_LINK_FLAP|FAIL|name={name_tok}|down_delay_sec={down_delay_sec}|reason={reason}",
                            file=sys.stderr,
                        )
                        print(
                            f"FAIL: QMP_NET_LINK_FLAP_FAILED: failed to flap virtio-net link via QMP: {e}",
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
                        virtio_blk_resize_marker_line, virtio_blk_resize_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_blk_resize_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|",
                            carry=virtio_blk_resize_marker_carry,
                        )
                        virtio_blk_reset_marker_line, virtio_blk_reset_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_blk_reset_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|",
                            carry=virtio_blk_reset_marker_carry,
                        )
                        virtio_blk_msix_marker_line, virtio_blk_msix_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_blk_msix_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-msix|",
                            carry=virtio_blk_msix_marker_carry,
                        )
                        virtio_net_msix_marker_line, virtio_net_msix_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_net_msix_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|",
                            carry=virtio_net_msix_marker_carry,
                        )
                        virtio_net_marker_line, virtio_net_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_net_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|",
                            carry=virtio_net_marker_carry,
                        )
                        virtio_net_udp_marker_line, virtio_net_udp_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_net_udp_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|",
                            carry=virtio_net_udp_marker_carry,
                        )
                        virtio_net_udp_dns_marker_line, virtio_net_udp_dns_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_net_udp_dns_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp-dns|",
                            carry=virtio_net_udp_dns_marker_carry,
                        )
                        virtio_net_link_flap_marker_line, virtio_net_link_flap_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_net_link_flap_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|",
                            carry=virtio_net_link_flap_marker_carry,
                        )
                        virtio_net_offload_csum_marker_line, virtio_net_offload_csum_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_net_offload_csum_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-offload-csum|",
                            carry=virtio_net_offload_csum_marker_carry,
                        )
                        virtio_net_diag_marker_line, virtio_net_diag_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_net_diag_marker_line,
                            chunk2,
                            prefix=b"virtio-net-diag|",
                            carry=virtio_net_diag_marker_carry,
                        )
                        virtio_snd_marker_line, virtio_snd_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_snd_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd|",
                            carry=virtio_snd_marker_carry,
                        )
                        virtio_snd_skip_reason, virtio_snd_skip_reason_carry = _update_virtio_snd_skip_reason_from_chunk(
                            virtio_snd_skip_reason,
                            chunk2,
                            carry=virtio_snd_skip_reason_carry,
                        )
                        virtio_snd_capture_marker_line, virtio_snd_capture_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_snd_capture_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|",
                            carry=virtio_snd_capture_marker_carry,
                        )
                        virtio_snd_duplex_marker_line, virtio_snd_duplex_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_snd_duplex_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|",
                            carry=virtio_snd_duplex_marker_carry,
                        )
                        virtio_snd_buffer_limits_marker_line, virtio_snd_buffer_limits_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_snd_buffer_limits_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|",
                            carry=virtio_snd_buffer_limits_marker_carry,
                        )
                        virtio_snd_msix_marker_line, virtio_snd_msix_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_snd_msix_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|",
                            carry=virtio_snd_msix_marker_carry,
                        )
                        virtio_input_marker_line, virtio_input_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_input_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-input|",
                            carry=virtio_input_marker_carry,
                        )
                        virtio_input_bind_marker_line, virtio_input_bind_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_input_bind_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|",
                            carry=virtio_input_bind_marker_carry,
                        )
                        virtio_input_msix_marker_line, virtio_input_msix_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_input_msix_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|",
                            carry=virtio_input_msix_marker_carry,
                        )
                        virtio_input_leds_marker_line, virtio_input_leds_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_input_leds_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|",
                            carry=virtio_input_leds_marker_carry,
                        )
                        virtio_input_events_marker_line, virtio_input_events_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_input_events_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|",
                            carry=virtio_input_events_marker_carry,
                        )
                        virtio_input_media_keys_marker_line, virtio_input_media_keys_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_input_media_keys_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|",
                            carry=virtio_input_media_keys_marker_carry,
                        )
                        virtio_input_led_marker_line, virtio_input_led_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_input_led_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|",
                            carry=virtio_input_led_marker_carry,
                        )
                        virtio_input_wheel_marker_line, virtio_input_wheel_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_input_wheel_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|",
                            carry=virtio_input_wheel_marker_carry,
                        )
                        virtio_input_events_modifiers_marker_line, virtio_input_events_modifiers_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_input_events_modifiers_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-modifiers|",
                            carry=virtio_input_events_modifiers_marker_carry,
                        )
                        virtio_input_events_buttons_marker_line, virtio_input_events_buttons_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_input_events_buttons_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-buttons|",
                            carry=virtio_input_events_buttons_marker_carry,
                        )
                        virtio_input_events_wheel_marker_line, virtio_input_events_wheel_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_input_events_wheel_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-wheel|",
                            carry=virtio_input_events_wheel_marker_carry,
                        )
                        virtio_input_tablet_events_marker_line, virtio_input_tablet_events_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_input_tablet_events_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|",
                            carry=virtio_input_tablet_events_marker_carry,
                        )
                        virtio_input_binding_marker_line, virtio_input_binding_marker_carry = _update_last_marker_line_from_chunk(
                            virtio_input_binding_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|",
                            carry=virtio_input_binding_marker_carry,
                        )
                        selftest_config_marker_line, selftest_config_marker_carry = _update_last_marker_line_from_chunk(
                            selftest_config_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|CONFIG|",
                            carry=selftest_config_marker_carry,
                        )
                        selftest_result_marker_line, selftest_result_marker_carry = _update_last_marker_line_from_chunk(
                            selftest_result_marker_line,
                            chunk2,
                            prefix=b"AERO_VIRTIO_SELFTEST|RESULT|",
                            carry=selftest_result_marker_carry,
                        )
                        tail = _append_serial_tail(tail, chunk2)
                        if expect_blk_msi_config is None:
                            if selftest_config_marker_line is not None:
                                expect_blk_msi_config = _parse_marker_kv_fields(
                                    selftest_config_marker_line
                                ).get("expect_blk_msi")
                            elif b"AERO_VIRTIO_SELFTEST|CONFIG|" in tail:
                                expect_blk_msi_config = _try_get_selftest_config_expect_blk_msi(tail)
                        if udp_port_config is None:
                            if selftest_config_marker_line is not None:
                                udp_port_config = _parse_marker_kv_fields(selftest_config_marker_line).get(
                                    "udp_port"
                                )
                            elif b"AERO_VIRTIO_SELFTEST|CONFIG|" in tail:
                                udp_port_config = _try_get_selftest_config_udp_port(tail)
                            if udp_port_config is not None and udp_server is not None:
                                try:
                                    guest_port = int(udp_port_config, 10)
                                except Exception:
                                    guest_port = None
                                host_port = int(udp_server.port)
                                if guest_port is not None and guest_port != host_port:
                                    print(
                                        "FAIL: UDP_PORT_MISMATCH: guest selftest CONFIG udp_port="
                                        f"{guest_port} but host harness UDP echo server is on {host_port}. "
                                        f"Run the harness with --udp-port {guest_port}, or re-provision the guest "
                                        f"to use --udp-port {host_port} (New-AeroWin7TestImage.ps1 -UdpPort {host_port}).",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                        if virtio_input_msix_marker is None:
                            if virtio_input_msix_marker_line is not None:
                                parts = virtio_input_msix_marker_line.split("|")
                                status = parts[3] if len(parts) >= 4 else ""
                                fields = _parse_marker_kv_fields(virtio_input_msix_marker_line)
                                virtio_input_msix_marker = _VirtioInputMsixMarker(
                                    status=status,
                                    fields=fields,
                                    line=virtio_input_msix_marker_line,
                                )
                            elif b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|" in tail:
                                marker = _parse_virtio_input_msix_marker(tail)
                                if marker is not None:
                                    virtio_input_msix_marker = marker

                        if virtio_blk_marker_line is not None:
                            status_tok = _try_extract_marker_status(virtio_blk_marker_line)
                            if not saw_virtio_blk_pass and status_tok == "PASS":
                                saw_virtio_blk_pass = True
                                if virtio_blk_marker_time is None:
                                    virtio_blk_marker_time = time.monotonic()
                            if not saw_virtio_blk_fail and status_tok == "FAIL":
                                saw_virtio_blk_fail = True
                                if virtio_blk_marker_time is None:
                                    virtio_blk_marker_time = time.monotonic()
                        if not saw_virtio_blk_pass and b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS" in tail:
                            saw_virtio_blk_pass = True
                            if virtio_blk_marker_time is None:
                                virtio_blk_marker_time = time.monotonic()
                        if not saw_virtio_blk_fail and b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|FAIL" in tail:
                            saw_virtio_blk_fail = True
                            if virtio_blk_marker_time is None:
                                virtio_blk_marker_time = time.monotonic()

                        # Prefer the incrementally captured marker line (virtio_blk_resize_marker_line) so we
                        # don't miss the READY/PASS/FAIL/SKIP token when the rolling tail buffer truncates.
                        if virtio_blk_resize_marker_line is not None:
                            toks = virtio_blk_resize_marker_line.split("|")
                            status_tok = toks[3] if len(toks) >= 4 else ""
                            if not saw_virtio_blk_resize_pass and status_tok == "PASS":
                                saw_virtio_blk_resize_pass = True
                            if not saw_virtio_blk_resize_fail and status_tok == "FAIL":
                                saw_virtio_blk_resize_fail = True
                            if not saw_virtio_blk_resize_skip and status_tok == "SKIP":
                                saw_virtio_blk_resize_skip = True
                            if not saw_virtio_blk_resize_ready and status_tok == "READY":
                                fields = _parse_marker_kv_fields(virtio_blk_resize_marker_line)
                                if "old_bytes" in fields:
                                    try:
                                        blk_resize_old_bytes = int(fields["old_bytes"], 0)
                                    except Exception:
                                        blk_resize_old_bytes = None
                                    if blk_resize_old_bytes is not None:
                                        saw_virtio_blk_resize_ready = True
                                        if need_blk_resize:
                                            delta_bytes = int(args.blk_resize_delta_mib) * 1024 * 1024
                                            blk_resize_new_bytes = _virtio_blk_resize_compute_new_bytes(
                                                blk_resize_old_bytes, delta_bytes
                                            )

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

                        # Prefer the incrementally captured marker line (virtio_blk_reset_marker_line) so we
                        # don't miss PASS/FAIL/SKIP when the rolling tail buffer truncates earlier output.
                        if virtio_blk_reset_marker_line is not None:
                            toks = virtio_blk_reset_marker_line.split("|")
                            status_tok = toks[3] if len(toks) >= 4 else ""
                            if not saw_virtio_blk_reset_pass and status_tok == "PASS":
                                saw_virtio_blk_reset_pass = True
                            if not saw_virtio_blk_reset_skip and status_tok == "SKIP":
                                saw_virtio_blk_reset_skip = True
                            if not saw_virtio_blk_reset_fail and status_tok == "FAIL":
                                saw_virtio_blk_reset_fail = True

                        if (
                            not saw_virtio_blk_reset_pass
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|PASS" in tail
                        ):
                            saw_virtio_blk_reset_pass = True
                        if (
                            not saw_virtio_blk_reset_skip
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|SKIP" in tail
                        ):
                            saw_virtio_blk_reset_skip = True
                        if (
                            not saw_virtio_blk_reset_fail
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|FAIL" in tail
                        ):
                            saw_virtio_blk_reset_fail = True
                        if virtio_input_marker_line is not None:
                            status_tok = _try_extract_marker_status(virtio_input_marker_line)
                            if not saw_virtio_input_pass and status_tok == "PASS":
                                saw_virtio_input_pass = True
                                if virtio_input_marker_time is None:
                                    virtio_input_marker_time = time.monotonic()
                            if not saw_virtio_input_fail and status_tok == "FAIL":
                                saw_virtio_input_fail = True
                                if virtio_input_marker_time is None:
                                    virtio_input_marker_time = time.monotonic()
                        if not saw_virtio_input_pass and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input|PASS" in tail:
                            saw_virtio_input_pass = True
                            if virtio_input_marker_time is None:
                                virtio_input_marker_time = time.monotonic()
                        if not saw_virtio_input_fail and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input|FAIL" in tail:
                            saw_virtio_input_fail = True
                            if virtio_input_marker_time is None:
                                virtio_input_marker_time = time.monotonic()
                        if virtio_input_bind_marker_line is not None:
                            status_tok = _try_extract_marker_status(virtio_input_bind_marker_line)
                            if not saw_virtio_input_bind_pass and status_tok == "PASS":
                                saw_virtio_input_bind_pass = True
                            if not saw_virtio_input_bind_fail and status_tok == "FAIL":
                                saw_virtio_input_bind_fail = True
                        if (
                            not saw_virtio_input_bind_pass
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|PASS" in tail
                        ):
                            saw_virtio_input_bind_pass = True
                        if (
                            not saw_virtio_input_bind_fail
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|FAIL" in tail
                        ):
                            saw_virtio_input_bind_fail = True

                        if virtio_input_leds_marker_line is not None:
                            status_tok = _try_extract_marker_status(virtio_input_leds_marker_line)
                            if not saw_virtio_input_leds_pass and status_tok == "PASS":
                                saw_virtio_input_leds_pass = True
                            if not saw_virtio_input_leds_fail and status_tok == "FAIL":
                                saw_virtio_input_leds_fail = True
                            if not saw_virtio_input_leds_skip and status_tok == "SKIP":
                                saw_virtio_input_leds_skip = True

                        if virtio_input_events_marker_line is not None:
                            toks = virtio_input_events_marker_line.split("|")
                            status_tok = toks[3] if len(toks) >= 4 else ""
                            if not saw_virtio_input_events_ready and status_tok == "READY":
                                saw_virtio_input_events_ready = True
                            if not saw_virtio_input_events_pass and status_tok == "PASS":
                                saw_virtio_input_events_pass = True
                            if not saw_virtio_input_events_fail and status_tok == "FAIL":
                                saw_virtio_input_events_fail = True
                            if not saw_virtio_input_events_skip and status_tok == "SKIP":
                                saw_virtio_input_events_skip = True

                        if virtio_input_media_keys_marker_line is not None:
                            toks = virtio_input_media_keys_marker_line.split("|")
                            status_tok = toks[3] if len(toks) >= 4 else ""
                            if not saw_virtio_input_media_keys_ready and status_tok == "READY":
                                saw_virtio_input_media_keys_ready = True
                            if not saw_virtio_input_media_keys_pass and status_tok == "PASS":
                                saw_virtio_input_media_keys_pass = True
                            if not saw_virtio_input_media_keys_fail and status_tok == "FAIL":
                                saw_virtio_input_media_keys_fail = True
                            if not saw_virtio_input_media_keys_skip and status_tok == "SKIP":
                                saw_virtio_input_media_keys_skip = True

                        if virtio_input_led_marker_line is not None:
                            status_tok = _try_extract_marker_status(virtio_input_led_marker_line)
                            if not saw_virtio_input_led_pass and status_tok == "PASS":
                                saw_virtio_input_led_pass = True
                            if not saw_virtio_input_led_fail and status_tok == "FAIL":
                                saw_virtio_input_led_fail = True
                            if not saw_virtio_input_led_skip and status_tok == "SKIP":
                                saw_virtio_input_led_skip = True

                        if virtio_input_wheel_marker_line is not None:
                            status_tok = _try_extract_marker_status(virtio_input_wheel_marker_line)
                            if not saw_virtio_input_wheel_pass and status_tok == "PASS":
                                saw_virtio_input_wheel_pass = True
                            if not saw_virtio_input_wheel_fail and status_tok == "FAIL":
                                saw_virtio_input_wheel_fail = True
                            if not saw_virtio_input_wheel_skip and status_tok == "SKIP":
                                saw_virtio_input_wheel_skip = True

                        if virtio_input_events_modifiers_marker_line is not None:
                            status_tok = _try_extract_marker_status(virtio_input_events_modifiers_marker_line)
                            if not saw_virtio_input_events_modifiers_pass and status_tok == "PASS":
                                saw_virtio_input_events_modifiers_pass = True
                            if not saw_virtio_input_events_modifiers_fail and status_tok == "FAIL":
                                saw_virtio_input_events_modifiers_fail = True
                            if not saw_virtio_input_events_modifiers_skip and status_tok == "SKIP":
                                saw_virtio_input_events_modifiers_skip = True

                        if virtio_input_events_buttons_marker_line is not None:
                            status_tok = _try_extract_marker_status(virtio_input_events_buttons_marker_line)
                            if not saw_virtio_input_events_buttons_pass and status_tok == "PASS":
                                saw_virtio_input_events_buttons_pass = True
                            if not saw_virtio_input_events_buttons_fail and status_tok == "FAIL":
                                saw_virtio_input_events_buttons_fail = True
                            if not saw_virtio_input_events_buttons_skip and status_tok == "SKIP":
                                saw_virtio_input_events_buttons_skip = True

                        if virtio_input_events_wheel_marker_line is not None:
                            status_tok = _try_extract_marker_status(virtio_input_events_wheel_marker_line)
                            if not saw_virtio_input_events_wheel_pass and status_tok == "PASS":
                                saw_virtio_input_events_wheel_pass = True
                            if not saw_virtio_input_events_wheel_fail and status_tok == "FAIL":
                                saw_virtio_input_events_wheel_fail = True
                            if not saw_virtio_input_events_wheel_skip and status_tok == "SKIP":
                                saw_virtio_input_events_wheel_skip = True

                        if virtio_input_tablet_events_marker_line is not None:
                            toks = virtio_input_tablet_events_marker_line.split("|")
                            status_tok = toks[3] if len(toks) >= 4 else ""
                            if not saw_virtio_input_tablet_events_ready and status_tok == "READY":
                                saw_virtio_input_tablet_events_ready = True
                            if not saw_virtio_input_tablet_events_pass and status_tok == "PASS":
                                saw_virtio_input_tablet_events_pass = True
                            if not saw_virtio_input_tablet_events_fail and status_tok == "FAIL":
                                saw_virtio_input_tablet_events_fail = True
                            if not saw_virtio_input_tablet_events_skip and status_tok == "SKIP":
                                saw_virtio_input_tablet_events_skip = True
                        if (
                            not saw_virtio_input_leds_pass
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|PASS" in tail
                        ):
                            saw_virtio_input_leds_pass = True
                        if (
                            not saw_virtio_input_leds_fail
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|FAIL" in tail
                        ):
                            saw_virtio_input_leds_fail = True
                        if (
                            not saw_virtio_input_leds_skip
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|SKIP" in tail
                        ):
                            saw_virtio_input_leds_skip = True
                        if (
                            not saw_virtio_input_binding_pass
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|PASS" in tail
                        ):
                            saw_virtio_input_binding_pass = True
                        if (
                            not saw_virtio_input_binding_fail
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|FAIL" in tail
                        ):
                            saw_virtio_input_binding_fail = True
                        if (
                            not saw_virtio_input_binding_skip
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|SKIP" in tail
                        ):
                            saw_virtio_input_binding_skip = True
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
                            not saw_virtio_input_led_pass
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|PASS" in tail
                        ):
                            saw_virtio_input_led_pass = True
                        if (
                            not saw_virtio_input_led_fail
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|FAIL" in tail
                        ):
                            saw_virtio_input_led_fail = True
                        if (
                            not saw_virtio_input_led_skip
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|SKIP" in tail
                        ):
                            saw_virtio_input_led_skip = True
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

                        if virtio_snd_marker_line is not None:
                            status_tok = _try_extract_marker_status(virtio_snd_marker_line)
                            if not saw_virtio_snd_pass and status_tok == "PASS":
                                saw_virtio_snd_pass = True
                            if not saw_virtio_snd_skip and status_tok == "SKIP":
                                saw_virtio_snd_skip = True
                            if not saw_virtio_snd_fail and status_tok == "FAIL":
                                saw_virtio_snd_fail = True
                        if virtio_snd_capture_marker_line is not None:
                            status_tok = _try_extract_marker_status(virtio_snd_capture_marker_line)
                            if not saw_virtio_snd_capture_pass and status_tok == "PASS":
                                saw_virtio_snd_capture_pass = True
                            if not saw_virtio_snd_capture_skip and status_tok == "SKIP":
                                saw_virtio_snd_capture_skip = True
                            if not saw_virtio_snd_capture_fail and status_tok == "FAIL":
                                saw_virtio_snd_capture_fail = True
                        if virtio_snd_duplex_marker_line is not None:
                            status_tok = _try_extract_marker_status(virtio_snd_duplex_marker_line)
                            if not saw_virtio_snd_duplex_pass and status_tok == "PASS":
                                saw_virtio_snd_duplex_pass = True
                            if not saw_virtio_snd_duplex_skip and status_tok == "SKIP":
                                saw_virtio_snd_duplex_skip = True
                            if not saw_virtio_snd_duplex_fail and status_tok == "FAIL":
                                saw_virtio_snd_duplex_fail = True
                        if virtio_snd_buffer_limits_marker_line is not None:
                            status_tok = _try_extract_marker_status(virtio_snd_buffer_limits_marker_line)
                            if not saw_virtio_snd_buffer_limits_pass and status_tok == "PASS":
                                saw_virtio_snd_buffer_limits_pass = True
                            if not saw_virtio_snd_buffer_limits_skip and status_tok == "SKIP":
                                saw_virtio_snd_buffer_limits_skip = True
                            if not saw_virtio_snd_buffer_limits_fail and status_tok == "FAIL":
                                saw_virtio_snd_buffer_limits_fail = True
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

                        if virtio_net_marker_line is not None:
                            status_tok = _try_extract_marker_status(virtio_net_marker_line)
                            if not saw_virtio_net_pass and status_tok == "PASS":
                                saw_virtio_net_pass = True
                            if not saw_virtio_net_fail and status_tok == "FAIL":
                                saw_virtio_net_fail = True

                        if virtio_net_udp_marker_line is not None:
                            status_tok = _try_extract_marker_status(virtio_net_udp_marker_line)
                            if not saw_virtio_net_udp_pass and status_tok == "PASS":
                                saw_virtio_net_udp_pass = True
                            if not saw_virtio_net_udp_fail and status_tok == "FAIL":
                                saw_virtio_net_udp_fail = True
                            if not saw_virtio_net_udp_skip and status_tok == "SKIP":
                                saw_virtio_net_udp_skip = True

                        if virtio_net_link_flap_marker_line is not None:
                            toks = virtio_net_link_flap_marker_line.split("|")
                            status_tok = toks[3] if len(toks) >= 4 else ""
                            if not saw_virtio_net_link_flap_ready and status_tok == "READY":
                                saw_virtio_net_link_flap_ready = True
                            if not saw_virtio_net_link_flap_pass and status_tok == "PASS":
                                saw_virtio_net_link_flap_pass = True
                            if not saw_virtio_net_link_flap_fail and status_tok == "FAIL":
                                saw_virtio_net_link_flap_fail = True
                            if not saw_virtio_net_link_flap_skip and status_tok == "SKIP":
                                saw_virtio_net_link_flap_skip = True
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
                        if (
                            not saw_virtio_net_link_flap_ready
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|READY" in tail
                        ):
                            saw_virtio_net_link_flap_ready = True
                        if (
                            not saw_virtio_net_link_flap_pass
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|PASS" in tail
                        ):
                            saw_virtio_net_link_flap_pass = True
                        if (
                            not saw_virtio_net_link_flap_fail
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|FAIL" in tail
                        ):
                            saw_virtio_net_link_flap_fail = True
                        if (
                            not saw_virtio_net_link_flap_skip
                            and b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|SKIP" in tail
                        ):
                            saw_virtio_net_link_flap_skip = True
                        result_status: Optional[str] = None
                        if selftest_result_marker_line is not None:
                            result_status = _try_extract_marker_status(selftest_result_marker_line)
                        if result_status is None:
                            if b"AERO_VIRTIO_SELFTEST|RESULT|PASS" in tail:
                                result_status = "PASS"
                            elif b"AERO_VIRTIO_SELFTEST|RESULT|FAIL" in tail:
                                result_status = "FAIL"

                        if result_status == "PASS":
                            if args.require_expect_blk_msi:
                                expect_blk_msi_config, msg = _require_expect_blk_msi_config(
                                    tail, expect_blk_msi_config=expect_blk_msi_config
                                )
                                if msg is not None:
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if not require_per_test_markers and getattr(
                                args, "require_virtio_input_binding", False
                            ):
                                bind_fail = _check_required_virtio_input_bind_marker(
                                    require_per_test_markers=True,
                                    saw_pass=saw_virtio_input_bind_pass,
                                    saw_fail=saw_virtio_input_bind_fail,
                                )
                                if bind_fail == "VIRTIO_INPUT_BIND_FAILED":
                                    print(
                                        _virtio_input_bind_fail_failure_message(
                                            tail,
                                            marker_line=virtio_input_bind_marker_line,
                                        ),
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if bind_fail == "MISSING_VIRTIO_INPUT_BIND":
                                    print(
                                        "FAIL: MISSING_VIRTIO_INPUT_BIND: selftest RESULT=PASS but did not emit virtio-input-bind test marker",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if require_per_test_markers:
                                if saw_virtio_blk_fail:
                                    print(
                                        _virtio_blk_fail_failure_message(
                                            tail,
                                            marker_line=virtio_blk_marker_line,
                                        ),
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
                                            _virtio_blk_resize_fail_failure_message(
                                                tail,
                                                marker_line=virtio_blk_resize_marker_line,
                                            ),
                                            file=sys.stderr,
                                        )
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                                    if not saw_virtio_blk_resize_pass:
                                        if saw_virtio_blk_resize_skip:
                                            print(
                                                _virtio_blk_resize_skip_failure_message(
                                                    tail,
                                                    marker_line=virtio_blk_resize_marker_line,
                                                ),
                                                file=sys.stderr,
                                            )
                                        else:
                                            print(
                                                "FAIL: MISSING_VIRTIO_BLK_RESIZE: selftest RESULT=PASS but did not emit virtio-blk-resize test marker "
                                                "while --with-blk-resize/--with-virtio-blk-resize/--require-virtio-blk-resize/--enable-virtio-blk-resize was enabled",
                                                file=sys.stderr,
                                            )
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                                if saw_virtio_input_fail:
                                    print(
                                        _virtio_input_fail_failure_message(
                                            tail,
                                            marker_line=virtio_input_marker_line,
                                        ),
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
                                bind_fail = _check_required_virtio_input_bind_marker(
                                    require_per_test_markers=require_per_test_markers,
                                    saw_pass=saw_virtio_input_bind_pass,
                                    saw_fail=saw_virtio_input_bind_fail,
                                )
                                if bind_fail == "VIRTIO_INPUT_BIND_FAILED":
                                    print(
                                        _virtio_input_bind_fail_failure_message(
                                            tail,
                                            marker_line=virtio_input_bind_marker_line,
                                        )
                                        + " (see serial log for bound service name / ConfigManager error details)",
                                        file=sys.stderr,
                                    )
                                    _print_virtio_input_bind_diagnostics(serial_log)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if bind_fail == "MISSING_VIRTIO_INPUT_BIND":
                                    print(
                                        "FAIL: MISSING_VIRTIO_INPUT_BIND: selftest RESULT=PASS but did not emit virtio-input-bind test marker "
                                        "(guest selftest too old; update the image/selftest binary)",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if saw_virtio_snd_fail:
                                    print(
                                        _virtio_snd_fail_failure_message(
                                            tail,
                                            marker_line=virtio_snd_marker_line,
                                        ),
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break

                                if args.enable_virtio_snd:
                                    if not saw_virtio_snd_pass:
                                        msg = "FAIL: MISSING_VIRTIO_SND: virtio-snd test did not PASS while --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"
                                        if saw_virtio_snd_skip:
                                            msg = _virtio_snd_skip_failure_message(
                                                tail,
                                                marker_line=virtio_snd_marker_line,
                                                skip_reason=virtio_snd_skip_reason,
                                            )
                                        print(msg, file=sys.stderr)
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                                    if saw_virtio_snd_capture_fail:
                                        print(
                                            _virtio_snd_capture_fail_failure_message(
                                                tail,
                                                marker_line=virtio_snd_capture_marker_line,
                                            ),
                                            file=sys.stderr,
                                        )
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                                    if not saw_virtio_snd_capture_pass:
                                        msg = (
                                            "FAIL: MISSING_VIRTIO_SND_CAPTURE: virtio-snd capture test did not PASS while --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"
                                        )
                                        if saw_virtio_snd_capture_skip:
                                            msg = _virtio_snd_capture_skip_failure_message(
                                                tail,
                                                marker_line=virtio_snd_capture_marker_line,
                                            )
                                        print(msg, file=sys.stderr)
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                                    if saw_virtio_snd_duplex_fail:
                                        print(
                                            _virtio_snd_duplex_fail_failure_message(
                                                tail,
                                                marker_line=virtio_snd_duplex_marker_line,
                                            ),
                                            file=sys.stderr,
                                        )
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                                    if not saw_virtio_snd_duplex_pass:
                                        if saw_virtio_snd_duplex_skip:
                                            msg = _virtio_snd_duplex_skip_failure_message(
                                                tail,
                                                marker_line=virtio_snd_duplex_marker_line,
                                            )
                                        else:
                                                msg = (
                                                    "FAIL: MISSING_VIRTIO_SND_DUPLEX: selftest RESULT=PASS but did not emit virtio-snd-duplex test marker "
                                                    "while --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"
                                                )
                                        print(msg, file=sys.stderr)
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break

                                    if args.with_snd_buffer_limits:
                                        msg = _virtio_snd_buffer_limits_required_failure_message(
                                            tail,
                                            saw_pass=saw_virtio_snd_buffer_limits_pass,
                                            saw_fail=saw_virtio_snd_buffer_limits_fail,
                                            saw_skip=saw_virtio_snd_buffer_limits_skip,
                                            marker_line=virtio_snd_buffer_limits_marker_line,
                                        )
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
                                            _virtio_snd_capture_fail_failure_message(
                                                tail,
                                                marker_line=virtio_snd_capture_marker_line,
                                            ),
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
                                            _virtio_snd_duplex_fail_failure_message(
                                                tail,
                                                marker_line=virtio_snd_duplex_marker_line,
                                            ),
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
                                        _virtio_net_fail_failure_message(
                                            tail,
                                            marker_line=virtio_net_marker_line,
                                        ),
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
                                            _virtio_net_udp_fail_failure_message(
                                                tail,
                                                marker_line=virtio_net_udp_marker_line,
                                            ),
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

                                if need_net_link_flap:
                                    msg = _virtio_net_link_flap_required_failure_message(
                                        tail,
                                        saw_pass=saw_virtio_net_link_flap_pass,
                                        saw_fail=saw_virtio_net_link_flap_fail,
                                        saw_skip=saw_virtio_net_link_flap_skip,
                                        marker_line=virtio_net_link_flap_marker_line,
                                    )
                                    if msg is not None:
                                        print(msg, file=sys.stderr)
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                            elif args.enable_virtio_snd:
                                if saw_virtio_snd_fail:
                                    print(
                                        _virtio_snd_fail_failure_message(
                                            tail,
                                            marker_line=virtio_snd_marker_line,
                                        ),
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break

                                if not saw_virtio_snd_pass:
                                    msg = "FAIL: MISSING_VIRTIO_SND: virtio-snd test did not PASS while --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"
                                    if saw_virtio_snd_skip:
                                        msg = _virtio_snd_skip_failure_message(
                                            tail,
                                            marker_line=virtio_snd_marker_line,
                                            skip_reason=virtio_snd_skip_reason,
                                        )
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if saw_virtio_snd_capture_fail:
                                    print(
                                        _virtio_snd_capture_fail_failure_message(
                                            tail,
                                            marker_line=virtio_snd_capture_marker_line,
                                        ),
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_snd_capture_pass:
                                    msg = "FAIL: MISSING_VIRTIO_SND_CAPTURE: virtio-snd capture test did not PASS while --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"
                                    if saw_virtio_snd_capture_skip:
                                        msg = _virtio_snd_capture_skip_failure_message(
                                            tail,
                                            marker_line=virtio_snd_capture_marker_line,
                                        )
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if saw_virtio_snd_duplex_fail:
                                    print(
                                        _virtio_snd_duplex_fail_failure_message(
                                            tail,
                                            marker_line=virtio_snd_duplex_marker_line,
                                        ),
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_snd_duplex_pass:
                                    if saw_virtio_snd_duplex_skip:
                                        msg = _virtio_snd_duplex_skip_failure_message(
                                            tail,
                                            marker_line=virtio_snd_duplex_marker_line,
                                        )
                                    else:
                                        msg = (
                                            "FAIL: MISSING_VIRTIO_SND_DUPLEX: selftest RESULT=PASS but did not emit virtio-snd-duplex test marker "
                                            "while --with-virtio-snd/--require-virtio-snd/--enable-virtio-snd was enabled"
                                        )
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break

                                if args.with_snd_buffer_limits:
                                    msg = _virtio_snd_buffer_limits_required_failure_message(
                                        tail,
                                        saw_pass=saw_virtio_snd_buffer_limits_pass,
                                        saw_fail=saw_virtio_snd_buffer_limits_fail,
                                        saw_skip=saw_virtio_snd_buffer_limits_skip,
                                        marker_line=virtio_snd_buffer_limits_marker_line,
                                    )
                                    if msg is not None:
                                        print(msg, file=sys.stderr)
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                            if need_blk_reset:
                                if saw_virtio_blk_reset_fail:
                                    print(
                                        _virtio_blk_reset_fail_failure_message(
                                            tail,
                                            marker_line=virtio_blk_reset_marker_line,
                                        ),
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_blk_reset_pass:
                                    if saw_virtio_blk_reset_skip:
                                        print(
                                            _virtio_blk_reset_skip_failure_message(
                                                tail,
                                                marker_line=virtio_blk_reset_marker_line,
                                            ),
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(_virtio_blk_reset_missing_failure_message(), file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if need_input_leds:
                                msg = _virtio_input_leds_required_failure_message(
                                    tail,
                                    saw_pass=saw_virtio_input_leds_pass,
                                    saw_fail=saw_virtio_input_leds_fail,
                                    saw_skip=saw_virtio_input_leds_skip,
                                    marker_line=virtio_input_leds_marker_line,
                                )
                                if msg is not None:
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if need_input_events:
                                if saw_virtio_input_events_fail:
                                    print(
                                        _virtio_input_events_fail_failure_message(
                                            tail,
                                            marker_line=virtio_input_events_marker_line,
                                            req_flags_desc=input_events_req_flags_desc,
                                        ),
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
                                        _virtio_input_media_keys_fail_failure_message(
                                            tail,
                                            marker_line=virtio_input_media_keys_marker_line,
                                        ),
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_input_media_keys_pass:
                                    if saw_virtio_input_media_keys_skip:
                                        print(
                                            "FAIL: VIRTIO_INPUT_MEDIA_KEYS_SKIPPED: virtio-input-media-keys test was skipped (flag_not_set) but "
                                            "--with-input-media-keys/--with-virtio-input-media-keys/--require-virtio-input-media-keys/--enable-virtio-input-media-keys was enabled (provision the guest with --test-input-media-keys)",
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(
                                            "FAIL: MISSING_VIRTIO_INPUT_MEDIA_KEYS: did not observe virtio-input-media-keys PASS marker while "
                                            "--with-input-media-keys/--with-virtio-input-media-keys/--require-virtio-input-media-keys/--enable-virtio-input-media-keys was enabled",
                                            file=sys.stderr,
                                        )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break

                            if need_blk_resize:
                                if saw_virtio_blk_resize_fail:
                                    print(
                                        _virtio_blk_resize_fail_failure_message(
                                            tail,
                                            marker_line=virtio_blk_resize_marker_line,
                                        ),
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_blk_resize_pass:
                                    if saw_virtio_blk_resize_skip:
                                        print(
                                            _virtio_blk_resize_skip_failure_message(
                                                tail,
                                                marker_line=virtio_blk_resize_marker_line,
                                            ),
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(
                                            "FAIL: MISSING_VIRTIO_BLK_RESIZE: did not observe virtio-blk-resize PASS marker while --with-blk-resize/--with-virtio-blk-resize/--require-virtio-blk-resize/--enable-virtio-blk-resize was enabled",
                                            file=sys.stderr,
                                        )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if need_net_link_flap:
                                msg = _virtio_net_link_flap_required_failure_message(
                                    tail,
                                    saw_pass=saw_virtio_net_link_flap_pass,
                                    saw_fail=saw_virtio_net_link_flap_fail,
                                    saw_skip=saw_virtio_net_link_flap_skip,
                                    marker_line=virtio_net_link_flap_marker_line,
                                )
                                if msg is not None:
                                    print(msg, file=sys.stderr)
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
                                        _virtio_input_events_extended_fail_failure_message(
                                            tail,
                                            modifiers_marker_line=virtio_input_events_modifiers_marker_line,
                                            buttons_marker_line=virtio_input_events_buttons_marker_line,
                                            wheel_marker_line=virtio_input_events_wheel_marker_line,
                                        ),
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
                                            "--with-input-events-extended/--with-input-events-extra was enabled (provision the guest with --test-input-events-extended)",
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(
                                            f"FAIL: MISSING_VIRTIO_INPUT_EVENTS_EXTENDED: did not observe {name} PASS marker while "
                                            "--with-input-events-extended/--with-input-events-extra was enabled",
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
                                        _virtio_input_tablet_events_fail_failure_message(
                                            tail,
                                            marker_line=virtio_input_tablet_events_marker_line,
                                        ),
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_input_tablet_events_pass:
                                    if saw_virtio_input_tablet_events_skip:
                                        print(
                                            _virtio_input_tablet_events_skip_failure_message(
                                                tail,
                                                marker_line=virtio_input_tablet_events_marker_line,
                                            ),
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(
                                            "FAIL: MISSING_VIRTIO_INPUT_TABLET_EVENTS: did not observe virtio-input-tablet-events PASS marker while "
                                            "--with-input-tablet-events/--with-tablet-events/--with-virtio-input-tablet-events/--require-virtio-input-tablet-events/--enable-virtio-input-tablet-events was enabled",
                                            file=sys.stderr,
                                        )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if need_input_wheel:
                                if saw_virtio_input_wheel_fail:
                                    print(
                                        _virtio_input_wheel_fail_failure_message(
                                            tail,
                                            marker_line=virtio_input_wheel_marker_line,
                                        ),
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                                if not saw_virtio_input_wheel_pass:
                                    if saw_virtio_input_wheel_skip:
                                        print(
                                            _virtio_input_wheel_skip_failure_message(
                                                tail,
                                                marker_line=virtio_input_wheel_marker_line,
                                            ),
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(
                                            "FAIL: MISSING_VIRTIO_INPUT_WHEEL: did not observe virtio-input-wheel PASS marker while "
                                            "--with-input-wheel/--with-virtio-input-wheel/--require-virtio-input-wheel/--enable-virtio-input-wheel was enabled",
                                            file=sys.stderr,
                                        )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if need_blk_reset:
                                try:
                                    reset_tail = serial_log.read_bytes()
                                except Exception:
                                    reset_tail = tail
                                msg = _virtio_blk_reset_required_failure_message(
                                    reset_tail,
                                    saw_pass=saw_virtio_blk_reset_pass,
                                    saw_fail=saw_virtio_blk_reset_fail,
                                    saw_skip=saw_virtio_blk_reset_skip,
                                    marker_line=virtio_blk_reset_marker_line,
                                )
                                if msg is not None:
                                    print(msg, file=sys.stderr)
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
                            if args.require_virtio_net_msix:
                                msix_tail = (
                                    virtio_net_msix_marker_line.encode("utf-8")
                                    if virtio_net_msix_marker_line is not None
                                    else tail
                                )
                                ok, reason = _require_virtio_net_msix_marker(msix_tail)
                                if not ok:
                                    if reason.startswith("missing virtio-net-msix marker"):
                                                print(
                                                    "FAIL: MISSING_VIRTIO_NET_MSIX: did not observe virtio-net-msix marker while "
                                                    "--require-virtio-net-msix/--require-net-msix was enabled (guest selftest too old?)",
                                                    file=sys.stderr,
                                                )
                                    else:
                                        print(
                                            "FAIL: VIRTIO_NET_MSIX_REQUIRED: "
                                            f"{reason} (while --require-virtio-net-msix/--require-net-msix was enabled)",
                                            file=sys.stderr,
                                        )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if args.require_virtio_blk_msix:
                                msix_tail = (
                                    virtio_blk_msix_marker_line.encode("utf-8")
                                    if virtio_blk_msix_marker_line is not None
                                    else tail
                                )
                                ok, reason = _require_virtio_blk_msix_marker(msix_tail)
                                if not ok:
                                    print(
                                        "FAIL: VIRTIO_BLK_MSIX_REQUIRED: "
                                        f"{reason} (while --require-virtio-blk-msix/--require-blk-msix was enabled)",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if args.require_virtio_snd_msix:
                                msix_tail = (
                                    virtio_snd_msix_marker_line.encode("utf-8")
                                    if virtio_snd_msix_marker_line is not None
                                    else tail
                                )
                                ok, reason = _require_virtio_snd_msix_marker(msix_tail)
                                if not ok:
                                    print(
                                        "FAIL: VIRTIO_SND_MSIX_REQUIRED: "
                                        f"{reason} (while --require-virtio-snd-msix/--require-snd-msix was enabled)",
                                        file=sys.stderr,
                                    )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break

                            if need_input_led:
                                msg = _virtio_input_led_required_failure_message(
                                    tail,
                                    saw_pass=saw_virtio_input_led_pass,
                                    saw_fail=saw_virtio_input_led_fail,
                                    saw_skip=saw_virtio_input_led_skip,
                                    marker_line=virtio_input_led_marker_line,
                                )
                                if msg is not None:
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break

                            if bool(args.require_virtio_input_msix):
                                msix_tail = (
                                    virtio_input_msix_marker.line.encode("utf-8")
                                    if virtio_input_msix_marker is not None
                                    else (
                                        virtio_input_msix_marker_line.encode("utf-8")
                                        if virtio_input_msix_marker_line is not None
                                        else tail
                                    )
                                )
                                ok, reason = _require_virtio_input_msix_marker(msix_tail)
                                if not ok:
                                    if reason.startswith("missing virtio-input-msix marker"):
                                                print(
                                                    "FAIL: MISSING_VIRTIO_INPUT_MSIX: did not observe virtio-input-msix marker while "
                                                    "--require-virtio-input-msix/--require-input-msix was enabled (guest selftest too old?)",
                                                    file=sys.stderr,
                                                )
                                    else:
                                        print(
                                            "FAIL: VIRTIO_INPUT_MSIX_REQUIRED: "
                                            f"{reason} (while --require-virtio-input-msix/--require-input-msix was enabled)",
                                            file=sys.stderr,
                                        )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if bool(getattr(args, "require_virtio_input_binding", False)):
                                msg = _virtio_input_binding_required_failure_message(
                                    tail,
                                    saw_pass=saw_virtio_input_binding_pass,
                                    saw_fail=saw_virtio_input_binding_fail,
                                    saw_skip=saw_virtio_input_binding_skip,
                                    marker_line=virtio_input_binding_marker_line,
                                )
                                if msg is not None:
                                    print(msg, file=sys.stderr)
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
                            if args.require_no_blk_recovery:
                                msg = _check_no_blk_recovery_requirement(
                                    tail,
                                    blk_test_line=virtio_blk_marker_line,
                                    blk_counters_line=virtio_blk_counters_marker_line,
                                )
                                if msg is not None:
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if args.fail_on_blk_recovery:
                                msg = _check_fail_on_blk_recovery_requirement(
                                    tail,
                                    blk_test_line=virtio_blk_marker_line,
                                    blk_counters_line=virtio_blk_counters_marker_line,
                                )
                                if msg is not None:
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if args.require_no_blk_reset_recovery:
                                msg = _check_no_blk_reset_recovery_requirement(
                                    tail,
                                    blk_reset_recovery_line=(
                                        virtio_blk_reset_recovery_marker_line
                                        or virtio_blk_miniport_reset_recovery_marker_line
                                    ),
                                )
                                if msg is not None:
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if args.fail_on_blk_reset_recovery:
                                msg = _check_fail_on_blk_reset_recovery_requirement(
                                    tail,
                                    blk_reset_recovery_line=(
                                        virtio_blk_reset_recovery_marker_line
                                        or virtio_blk_miniport_reset_recovery_marker_line
                                    ),
                                )
                                if msg is not None:
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if args.require_no_blk_miniport_flags:
                                msg = _check_no_blk_miniport_flags_requirement(
                                    tail,
                                    blk_miniport_flags_line=virtio_blk_miniport_flags_marker_line,
                                )
                                if msg is not None:
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            if args.fail_on_blk_miniport_flags:
                                msg = _check_fail_on_blk_miniport_flags_requirement(
                                    tail,
                                    blk_miniport_flags_line=virtio_blk_miniport_flags_marker_line,
                                )
                                if msg is not None:
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break

                            if args.require_net_csum_offload or getattr(args, "require_net_udp_csum_offload", False):
                                csum_tail = (
                                    virtio_net_offload_csum_marker_line.encode("utf-8")
                                    if virtio_net_offload_csum_marker_line is not None
                                    else tail
                                )
                                stats = _extract_virtio_net_offload_csum_stats(csum_tail)
                                if stats is None:
                                    if args.require_net_csum_offload:
                                        print(
                                            "FAIL: MISSING_VIRTIO_NET_CSUM_OFFLOAD: missing virtio-net-offload-csum marker while "
                                            "--require-net-csum-offload/--require-virtio-net-csum-offload was enabled",
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(
                                            "FAIL: MISSING_VIRTIO_NET_UDP_CSUM_OFFLOAD: missing virtio-net-offload-csum marker while "
                                            "--require-net-udp-csum-offload/--require-virtio-net-udp-csum-offload was enabled",
                                            file=sys.stderr,
                                        )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break

                                if stats.get("status") != "PASS":
                                    if args.require_net_csum_offload:
                                        print(
                                            "FAIL: VIRTIO_NET_CSUM_OFFLOAD_FAILED: virtio-net-offload-csum marker did not PASS "
                                            f"(status={stats.get('status')})",
                                            file=sys.stderr,
                                        )
                                    else:
                                        print(
                                            "FAIL: VIRTIO_NET_UDP_CSUM_OFFLOAD_FAILED: virtio-net-offload-csum marker did not PASS "
                                            f"(status={stats.get('status')})",
                                            file=sys.stderr,
                                        )
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break

                                if args.require_net_csum_offload:
                                    tx_csum = stats.get("tx_csum")
                                    if tx_csum is None:
                                        print(
                                            "FAIL: VIRTIO_NET_CSUM_OFFLOAD_MISSING_FIELDS: virtio-net-offload-csum marker missing tx_csum field",
                                            file=sys.stderr,
                                        )
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break

                                    if int(tx_csum) <= 0:
                                        rx_csum = stats.get("rx_csum")
                                        fallback = stats.get("fallback")
                                        print(
                                            "FAIL: VIRTIO_NET_CSUM_OFFLOAD_ZERO: checksum offload requirement not met "
                                            f"(tx_csum={tx_csum} rx_csum={rx_csum} fallback={fallback})",
                                            file=sys.stderr,
                                        )
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break

                                if getattr(args, "require_net_udp_csum_offload", False):
                                    tx_udp = stats.get("tx_udp")
                                    tx_udp4 = stats.get("tx_udp4")
                                    tx_udp6 = stats.get("tx_udp6")
                                    if tx_udp is None:
                                        if tx_udp4 is None and tx_udp6 is None:
                                            print(
                                                "FAIL: VIRTIO_NET_UDP_CSUM_OFFLOAD_MISSING_FIELDS: virtio-net-offload-csum marker missing tx_udp/tx_udp4/tx_udp6 fields",
                                                file=sys.stderr,
                                            )
                                            _print_tail(serial_log)
                                            result_code = 1
                                            break
                                        tx_udp = int(tx_udp4 or 0) + int(tx_udp6 or 0)

                                    if int(tx_udp) <= 0:
                                        fallback = stats.get("fallback")
                                        print(
                                            "FAIL: VIRTIO_NET_UDP_CSUM_OFFLOAD_ZERO: UDP checksum offload requirement not met "
                                            f"(tx_udp={tx_udp} tx_udp4={tx_udp4} tx_udp6={tx_udp6} fallback={fallback})",
                                            file=sys.stderr,
                                        )
                                        _print_tail(serial_log)
                                        result_code = 1
                                        break
                            print("PASS: AERO_VIRTIO_SELFTEST|RESULT|PASS")
                            result_code = 0
                            break
                        if result_status == "FAIL":
                            if args.require_expect_blk_msi:
                                expect_blk_msi_config, msg = _require_expect_blk_msi_config(
                                    tail, expect_blk_msi_config=expect_blk_msi_config
                                )
                                if msg is not None:
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            msg = _try_virtio_snd_force_null_backend_failure_message(tail)
                            if msg is not None:
                                print(msg)
                            else:
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
        if virtio_blk_counters_marker_carry:
            raw = virtio_blk_counters_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|"):
                try:
                    virtio_blk_counters_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_blk_reset_recovery_marker_carry:
            raw = virtio_blk_reset_recovery_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|"):
                try:
                    virtio_blk_reset_recovery_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_blk_miniport_flags_marker_carry:
            raw = virtio_blk_miniport_flags_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"virtio-blk-miniport-flags|"):
                try:
                    virtio_blk_miniport_flags_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_blk_miniport_reset_recovery_marker_carry:
            raw = virtio_blk_miniport_reset_recovery_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"virtio-blk-miniport-reset-recovery|"):
                try:
                    virtio_blk_miniport_reset_recovery_marker_line = raw2.decode(
                        "utf-8", errors="replace"
                    ).strip()
                except Exception:
                    pass
        if virtio_blk_resize_marker_carry:
            raw = virtio_blk_resize_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|"):
                try:
                    virtio_blk_resize_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_blk_reset_marker_carry:
            raw = virtio_blk_reset_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|"):
                try:
                    virtio_blk_reset_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_blk_msix_marker_carry:
            raw = virtio_blk_msix_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-msix|"):
                try:
                    virtio_blk_msix_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_net_msix_marker_carry:
            raw = virtio_net_msix_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|"):
                try:
                    virtio_net_msix_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_net_marker_carry:
            raw = virtio_net_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|"):
                try:
                    virtio_net_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_net_udp_marker_carry:
            raw = virtio_net_udp_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|"):
                try:
                    virtio_net_udp_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_net_udp_dns_marker_carry:
            raw = virtio_net_udp_dns_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp-dns|"):
                try:
                    virtio_net_udp_dns_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_net_link_flap_marker_carry:
            raw = virtio_net_link_flap_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|"):
                try:
                    virtio_net_link_flap_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_net_offload_csum_marker_carry:
            raw = virtio_net_offload_csum_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-offload-csum|"):
                try:
                    virtio_net_offload_csum_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_net_diag_marker_carry:
            raw = virtio_net_diag_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"virtio-net-diag|"):
                try:
                    virtio_net_diag_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_snd_marker_carry:
            raw = virtio_snd_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd|"):
                try:
                    virtio_snd_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_snd_capture_marker_carry:
            raw = virtio_snd_capture_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|"):
                try:
                    virtio_snd_capture_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_snd_duplex_marker_carry:
            raw = virtio_snd_duplex_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|"):
                try:
                    virtio_snd_duplex_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_snd_buffer_limits_marker_carry:
            raw = virtio_snd_buffer_limits_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|"):
                try:
                    virtio_snd_buffer_limits_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_snd_msix_marker_carry:
            raw = virtio_snd_msix_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|"):
                try:
                    virtio_snd_msix_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_input_msix_marker_carry:
            raw = virtio_input_msix_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|"):
                try:
                    virtio_input_msix_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_input_marker_carry:
            raw = virtio_input_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-input|"):
                try:
                    virtio_input_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_input_bind_marker_carry:
            raw = virtio_input_bind_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|"):
                try:
                    virtio_input_bind_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_input_leds_marker_carry:
            raw = virtio_input_leds_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|"):
                try:
                    virtio_input_leds_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_input_events_marker_carry:
            raw = virtio_input_events_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|"):
                try:
                    virtio_input_events_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_input_media_keys_marker_carry:
            raw = virtio_input_media_keys_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|"):
                try:
                    virtio_input_media_keys_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_input_led_marker_carry:
            raw = virtio_input_led_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|"):
                try:
                    virtio_input_led_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_input_wheel_marker_carry:
            raw = virtio_input_wheel_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|"):
                try:
                    virtio_input_wheel_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_input_events_modifiers_marker_carry:
            raw = virtio_input_events_modifiers_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-modifiers|"):
                try:
                    virtio_input_events_modifiers_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_input_events_buttons_marker_carry:
            raw = virtio_input_events_buttons_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-buttons|"):
                try:
                    virtio_input_events_buttons_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_input_events_wheel_marker_carry:
            raw = virtio_input_events_wheel_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-wheel|"):
                try:
                    virtio_input_events_wheel_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_input_tablet_events_marker_carry:
            raw = virtio_input_tablet_events_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|"):
                try:
                    virtio_input_tablet_events_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if virtio_input_binding_marker_carry:
            raw = virtio_input_binding_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|"):
                try:
                    virtio_input_binding_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if selftest_config_marker_carry:
            raw = selftest_config_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|CONFIG|"):
                try:
                    selftest_config_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass
        if selftest_result_marker_carry:
            raw = selftest_result_marker_carry.rstrip(b"\r")
            raw2 = raw.lstrip()
            if raw2.startswith(b"AERO_VIRTIO_SELFTEST|RESULT|"):
                try:
                    selftest_result_marker_line = raw2.decode("utf-8", errors="replace").strip()
                except Exception:
                    pass

        _emit_virtio_blk_irq_host_marker(tail, blk_test_line=virtio_blk_marker_line, irq_diag_markers=irq_diag_markers)
        blk_msix_tail = (
            virtio_blk_msix_marker_line.encode("utf-8") if virtio_blk_msix_marker_line is not None else tail
        )
        _emit_virtio_blk_msix_host_marker(blk_msix_tail)
        _emit_virtio_blk_io_host_marker(tail, blk_test_line=virtio_blk_marker_line)
        _emit_virtio_blk_recovery_host_marker(
            tail,
            blk_test_line=virtio_blk_marker_line,
            blk_counters_line=virtio_blk_counters_marker_line,
        )
        _emit_virtio_blk_counters_host_marker(
            tail,
            blk_counters_line=virtio_blk_counters_marker_line,
            blk_test_line=virtio_blk_marker_line,
        )
        _emit_virtio_blk_miniport_flags_host_marker(
            tail, marker_line=virtio_blk_miniport_flags_marker_line
        )
        _emit_virtio_blk_miniport_reset_recovery_host_marker(
            tail, marker_line=virtio_blk_miniport_reset_recovery_marker_line
        )
        _emit_virtio_blk_reset_recovery_host_marker(
            tail,
            blk_reset_recovery_line=(
                virtio_blk_reset_recovery_marker_line
                or virtio_blk_miniport_reset_recovery_marker_line
            ),
        )
        _emit_virtio_blk_resize_host_marker(tail, blk_resize_line=virtio_blk_resize_marker_line)
        _emit_virtio_blk_reset_host_marker(tail, blk_reset_line=virtio_blk_reset_marker_line)
        net_large_tail = virtio_net_marker_line.encode("utf-8") if virtio_net_marker_line is not None else tail
        _emit_virtio_net_large_host_marker(net_large_tail)
        net_udp_tail = virtio_net_udp_marker_line.encode("utf-8") if virtio_net_udp_marker_line is not None else tail
        _emit_virtio_net_udp_host_marker(net_udp_tail)
        net_udp_dns_tail = (
            virtio_net_udp_dns_marker_line.encode("utf-8") if virtio_net_udp_dns_marker_line is not None else tail
        )
        _emit_virtio_net_udp_dns_host_marker(net_udp_dns_tail)
        net_csum_tail = (
            virtio_net_offload_csum_marker_line.encode("utf-8")
            if virtio_net_offload_csum_marker_line is not None
            else tail
        )
        _emit_virtio_net_offload_csum_host_marker(net_csum_tail)
        net_diag_tail = (
            virtio_net_diag_marker_line.encode("utf-8") if virtio_net_diag_marker_line is not None else tail
        )
        _emit_virtio_net_diag_host_marker(net_diag_tail)
        net_msix_tail = (
            virtio_net_msix_marker_line.encode("utf-8") if virtio_net_msix_marker_line is not None else tail
        )
        _emit_virtio_net_msix_host_marker(net_msix_tail)
        _emit_virtio_input_binding_host_marker(tail, marker_line=virtio_input_binding_marker_line)
        _emit_virtio_net_irq_host_marker(net_large_tail)
        snd_irq_tail = virtio_snd_marker_line.encode("utf-8") if virtio_snd_marker_line is not None else tail
        _emit_virtio_snd_irq_host_marker(snd_irq_tail)
        snd_msix_tail = (
            virtio_snd_msix_marker_line.encode("utf-8") if virtio_snd_msix_marker_line is not None else tail
        )
        _emit_virtio_snd_msix_host_marker(snd_msix_tail)
        input_irq_tail = (
            virtio_input_marker_line.encode("utf-8") if virtio_input_marker_line is not None else tail
        )
        _emit_virtio_input_irq_host_marker(input_irq_tail)
        input_bind_tail = (
            virtio_input_bind_marker_line.encode("utf-8")
            if virtio_input_bind_marker_line is not None
            else tail
        )
        _emit_virtio_input_bind_host_marker(input_bind_tail)
        input_msix_tail = (
            virtio_input_msix_marker.line.encode("utf-8")
            if virtio_input_msix_marker is not None
            else (
                virtio_input_msix_marker_line.encode("utf-8")
                if virtio_input_msix_marker_line is not None
                else tail
            )
        )
        _emit_virtio_input_msix_host_marker(input_msix_tail)
        _emit_virtio_irq_host_markers(tail, markers=irq_diag_markers)
        snd_play_tail = virtio_snd_marker_line.encode("utf-8") if virtio_snd_marker_line is not None else tail
        _emit_virtio_snd_playback_host_marker(snd_play_tail)
        snd_capture_tail = (
            virtio_snd_capture_marker_line.encode("utf-8")
            if virtio_snd_capture_marker_line is not None
            else tail
        )
        _emit_virtio_snd_capture_host_marker(snd_capture_tail)
        _emit_virtio_snd_format_host_marker(tail)
        snd_duplex_tail = (
            virtio_snd_duplex_marker_line.encode("utf-8")
            if virtio_snd_duplex_marker_line is not None
            else tail
        )
        _emit_virtio_snd_duplex_host_marker(snd_duplex_tail)
        snd_buffer_limits_tail = (
            virtio_snd_buffer_limits_marker_line.encode("utf-8")
            if virtio_snd_buffer_limits_marker_line is not None
            else tail
        )
        _emit_virtio_snd_buffer_limits_host_marker(snd_buffer_limits_tail)
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


def _emit_virtio_input_events_inject_host_marker(
    *,
    ok: bool,
    attempt: int,
    backend: Optional[str] = None,
    kbd_mode: Optional[str] = None,
    mouse_mode: Optional[str] = None,
    reason: Optional[str] = None,
) -> None:
    if ok:
        assert backend is not None and kbd_mode is not None and mouse_mode is not None
        print(
            f"AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_EVENTS_INJECT|PASS|attempt={attempt}|"
            f"backend={backend}|kbd_mode={kbd_mode}|mouse_mode={mouse_mode}"
        )
        return
    reason_tok = _sanitize_marker_value(reason or "unknown")
    backend_tok = _sanitize_marker_value(backend or "unknown")
    print(
        f"AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_EVENTS_INJECT|FAIL|attempt={attempt}|backend={backend_tok}|reason={reason_tok}",
        file=sys.stderr,
    )


def _emit_virtio_input_media_keys_inject_host_marker(
    *,
    ok: bool,
    attempt: int,
    backend: Optional[str] = None,
    kbd_mode: Optional[str] = None,
    reason: Optional[str] = None,
) -> None:
    if ok:
        assert backend is not None and kbd_mode is not None
        print(
            f"AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_MEDIA_KEYS_INJECT|PASS|attempt={attempt}|backend={backend}|kbd_mode={kbd_mode}"
        )
        return
    reason_tok = _sanitize_marker_value(reason or "unknown")
    backend_tok = _sanitize_marker_value(backend or "unknown")
    print(
        f"AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_MEDIA_KEYS_INJECT|FAIL|attempt={attempt}|backend={backend_tok}|reason={reason_tok}",
        file=sys.stderr,
    )


def _emit_virtio_input_tablet_events_inject_host_marker(
    *,
    ok: bool,
    attempt: int,
    backend: Optional[str] = None,
    tablet_mode: Optional[str] = None,
    reason: Optional[str] = None,
) -> None:
    if ok:
        assert backend is not None and tablet_mode is not None
        print(
            f"AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_TABLET_EVENTS_INJECT|PASS|attempt={attempt}|backend={backend}|tablet_mode={tablet_mode}"
        )
        return
    reason_tok = _sanitize_marker_value(reason or "unknown")
    backend_tok = _sanitize_marker_value(backend or "unknown")
    print(
        f"AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_TABLET_EVENTS_INJECT|FAIL|attempt={attempt}|backend={backend_tok}|reason={reason_tok}",
        file=sys.stderr,
    )


def _check_required_virtio_input_bind_marker(
    *,
    require_per_test_markers: bool,
    saw_pass: bool,
    saw_fail: bool,
) -> Optional[str]:
    """
    Return a failure token when `virtio-input-bind` is required but missing/failed.

    This exists so unit tests can validate strict-marker behavior without invoking QEMU.
    """
    if not require_per_test_markers:
        return None
    if saw_fail:
        return "VIRTIO_INPUT_BIND_FAILED"
    if not saw_pass:
        return "MISSING_VIRTIO_INPUT_BIND"
    return None


def _extract_serial_log_lines_matching(
    path: Path,
    *,
    needles: list[bytes],
    max_lines: int = 50,
) -> list[str]:
    """
    Extract (up to) the last `max_lines` lines from `path` that contain any of `needles`.

    This is used for actionable diagnostics when strict marker requirements fail. We avoid
    relying on a small tail slice because the guest selftest can emit many lines after a
    specific test, pushing earlier diagnostics out of the tail buffer.
    """
    if not needles or max_lines <= 0:
        return []
    dq: deque[str] = deque(maxlen=max_lines)
    try:
        with path.open("rb") as f:
            for raw in f:
                if any(n in raw for n in needles):
                    try:
                        dq.append(raw.rstrip(b"\r\n").decode("utf-8", errors="replace"))
                    except Exception:
                        # Best-effort: never fail the harness due to an encoding error.
                        continue
    except FileNotFoundError:
        return []
    except OSError:
        return []
    return list(dq)


def _print_virtio_input_bind_diagnostics(serial_log: Path) -> None:
    """
    Print virtio-input PCI binding diagnostics from the guest serial log (best-effort).

    The guest selftest emits detailed lines like:
      virtio-input-bind: pci device ... service=<...>
      virtio-input-bind: pci device ... bound_service=<...> (expected aero_virtio_input)
      virtio-input-bind: pci device ... ConfigManagerErrorCode=<n>
    """
    lines = _extract_serial_log_lines_matching(
        serial_log,
        needles=[b"virtio-input-bind:"],
        max_lines=50,
    )
    if not lines:
        return
    sys.stderr.write("\n--- virtio-input-bind diagnostics ---\n")
    for line in lines:
        sys.stderr.write(line)
        sys.stderr.write("\n")


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

def _try_parse_selftest_config_marker(tail: bytes) -> Optional[dict[str, str]]:
    """
    Parse the guest selftest CONFIG marker into a dict of key/value fields.

    Example line emitted by the guest:
      AERO_VIRTIO_SELFTEST|CONFIG|http_url=...|...|expect_blk_msi=1
    """
    line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|CONFIG|")
    if not line:
        return None
    return _parse_marker_kv_fields(line)


def _try_get_selftest_config_expect_blk_msi(tail: bytes) -> Optional[str]:
    """
    Return the value of `expect_blk_msi` from the guest CONFIG marker if present.

    Returns:
      - "1" or "0" when present
      - None when the CONFIG marker (or field) is not present
    """
    cfg = _try_parse_selftest_config_marker(tail)
    if cfg is None:
        return None
    return cfg.get("expect_blk_msi")


def _require_expect_blk_msi_config(
    tail: bytes, *, expect_blk_msi_config: Optional[str]
) -> tuple[Optional[str], Optional[str]]:
    """
    Enforce `--require-expect-blk-msi` against the guest selftest CONFIG marker.

    Returns a tuple of:
      - `expect_blk_msi_config` (possibly updated by parsing `tail`)
      - `failure_message` (None when the requirement is satisfied)
    """

    if expect_blk_msi_config is None:
        expect_blk_msi_config = _try_get_selftest_config_expect_blk_msi(tail)

    if expect_blk_msi_config != "1":
        return (
            expect_blk_msi_config,
            "FAIL: EXPECT_BLK_MSI_NOT_SET: guest selftest was not provisioned with --expect-blk-msi "
            "(expect_blk_msi=1 in CONFIG marker). Re-provision the image or omit --require-expect-blk-msi.",
        )

    return expect_blk_msi_config, None


def _try_get_selftest_config_udp_port(tail: bytes) -> Optional[str]:
    """
    Return the value of `udp_port` from the guest CONFIG marker if present.

    Returns:
      - decimal string when present (e.g. "18081")
      - None when the CONFIG marker (or field) is not present
    """
    cfg = _try_parse_selftest_config_marker(tail)
    if cfg is None:
        return None
    return cfg.get("udp_port")


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

      virtio-<dev>-irq|INFO|mode=intx
      virtio-<dev>-irq|INFO|mode=msi|messages=<n>             # message interrupts (MSI or MSI-X)
      virtio-<dev>-irq|INFO|mode=msix|messages=<n>|...         # richer MSI-X diagnostics when available (e.g. virtio-snd)
      virtio-<dev>-irq|INFO|mode=none|...                      # polling-only (virtio-snd)
      virtio-<dev>-irq|WARN|reason=...|...

    Returns a mapping from device name (e.g. "virtio-net") to a dict of parsed fields.
    The dict always includes a "level" key ("INFO" or "WARN") and may include additional
    key/value fields (e.g. "mode", "messages", ...).

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

    # Bound the carry buffer to avoid unbounded growth if the guest prints extremely long lines without
    # newlines (and to avoid constructing an excessively large `carry + chunk` temporary buffer).
    # The harness rolling tail is capped to 128 KiB; use the same cap here since we can only reliably
    # parse marker lines within that window anyway.
    if len(carry) > _SERIAL_TAIL_CAP_BYTES:
        carry = carry[-_SERIAL_TAIL_CAP_BYTES:]

    data = carry + chunk

    # Use splitlines(keepends=True) so we correctly handle any of: LF, CRLF, or CR.
    parts = data.splitlines(keepends=True)
    new_carry = b""
    if parts and not parts[-1].endswith((b"\n", b"\r")):
        new_carry = parts.pop()
    if len(new_carry) > _SERIAL_TAIL_CAP_BYTES:
        new_carry = new_carry[-_SERIAL_TAIL_CAP_BYTES:]

    for raw in parts:
        raw = raw.rstrip(b"\r\n")
        raw2 = raw.lstrip()
        if not raw2.startswith(prefix):
            continue
        try:
            last = raw2.decode("utf-8", errors="replace").strip()
        except Exception:
            continue

    return last, new_carry


def _update_virtio_snd_skip_reason_from_chunk(
    reason: Optional[str], chunk: bytes, *, carry: bytes = b""
) -> tuple[Optional[str], bytes]:
    """
    Incrementally capture the virtio-snd SKIP reason from guest log text.

    The virtio-snd selftest's SKIP marker is intentionally strict (machine-friendly), and does not
    include a reason token. Instead, the guest logs a human-readable line, e.g.:

      virtio-snd: skipped (enable with --test-snd)
      virtio-snd: disabled by --disable-snd
      virtio-snd: pci device not detected

    Capture these lines incrementally so we can still produce a specific failure token even if the
    rolling serial tail buffer is truncated.

    Returns `(reason, carry)` where `carry` is any potentially incomplete last line (i.e. bytes
    after the last newline).
    """
    if not chunk and not carry:
        return reason, b""

    # Bound the carry buffer to avoid unbounded growth if the guest prints extremely long lines
    # without newlines.
    if len(carry) > _SERIAL_TAIL_CAP_BYTES:
        carry = carry[-_SERIAL_TAIL_CAP_BYTES:]

    data = carry + chunk
    parts = data.splitlines(keepends=True)
    new_carry = b""
    if parts and not parts[-1].endswith((b"\n", b"\r")):
        new_carry = parts.pop()
    if len(new_carry) > _SERIAL_TAIL_CAP_BYTES:
        new_carry = new_carry[-_SERIAL_TAIL_CAP_BYTES:]

    for raw in parts:
        raw = raw.rstrip(b"\r\n")
        raw2 = raw.lstrip()
        if not raw2:
            continue
        if b"virtio-snd: skipped (enable with --test-snd)" in raw2:
            reason = "guest_not_configured_with_--test-snd"
            continue
        if b"virtio-snd: disabled by --disable-snd" in raw2:
            reason = "--disable-snd"
            continue
        if b"virtio-snd:" in raw2 and b"device not detected" in raw2:
            reason = "device_missing"
            continue

    return reason, new_carry


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

def _parse_virtio_net_msix_marker(tail: bytes) -> Optional[tuple[str, dict[str, str]]]:
    """
    Parse the guest virtio-net MSI-X diagnostic marker emitted by aero-virtio-selftest.exe.

    Example:
      AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|PASS|mode=msix|messages=3|...

    Returns:
      (status_token, kv_fields) for the *last* matching line, or None if not found/invalid.
    """
    marker_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|")
    if marker_line is None:
        return None

    parts = marker_line.split("|")
    if len(parts) < 4:
        return None

    status = parts[3].strip()
    fields = _parse_marker_kv_fields(marker_line)
    return status, fields


def _extract_virtio_net_offload_csum_stats(tail: bytes) -> Optional[dict[str, object]]:
    """
    Extract virtio-net checksum offload counters from the guest selftest marker.

    Marker format:
      AERO_VIRTIO_SELFTEST|TEST|virtio-net-offload-csum|PASS|tx_csum=...|rx_csum=...|fallback=...
    """

    marker_line = _try_extract_last_marker_line(
        tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-offload-csum|"
    )
    if marker_line is None:
        return None

    fields = _parse_marker_kv_fields(marker_line)
    toks = marker_line.split("|")
    status = "INFO"
    if "FAIL" in toks:
        status = "FAIL"
    elif "PASS" in toks:
        status = "PASS"

    def parse_u64(k: str) -> Optional[int]:
        v = fields.get(k)
        if v is None:
            return None
        try:
            # base=0 accepts either decimal or 0x-prefixed values.
            x = int(v, 0)
        except Exception:
            return None
        if x < 0:
            return None
        return x

    return {
        "status": status,
        "tx_csum": parse_u64("tx_csum"),
        "rx_csum": parse_u64("rx_csum"),
        "fallback": parse_u64("fallback"),
        "tx_tcp": parse_u64("tx_tcp"),
        "tx_udp": parse_u64("tx_udp"),
        "rx_tcp": parse_u64("rx_tcp"),
        "rx_udp": parse_u64("rx_udp"),
        "tx_tcp4": parse_u64("tx_tcp4"),
        "tx_tcp6": parse_u64("tx_tcp6"),
        "tx_udp4": parse_u64("tx_udp4"),
        "tx_udp6": parse_u64("tx_udp6"),
        "rx_tcp4": parse_u64("rx_tcp4"),
        "rx_tcp6": parse_u64("rx_tcp6"),
        "rx_udp4": parse_u64("rx_udp4"),
        "rx_udp6": parse_u64("rx_udp6"),
        "fields": fields,
        "line": marker_line,
    }


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


@dataclass(frozen=True)
class _VirtioNetLinkFlapReadyInfo:
    adapter: Optional[str]
    guid: Optional[str]


def _try_extract_virtio_net_link_flap_ready(tail: bytes) -> Optional[_VirtioNetLinkFlapReadyInfo]:
    """
    Extract the guest virtio-net-link-flap READY marker from the serial tail.

    Marker format:
      AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|READY|adapter=...|guid=...
    """
    marker_line = _try_extract_last_marker_line(
        tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|READY"
    )
    if marker_line is None:
        return None
    fields = _parse_marker_kv_fields(marker_line)
    adapter = fields.get("adapter")
    guid = fields.get("guid")
    return _VirtioNetLinkFlapReadyInfo(adapter=adapter, guid=guid)


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


def _emit_virtio_net_udp_host_marker(tail: bytes) -> None:
    """
    Best-effort: emit a host-side marker describing the guest's UDP echo test.

    This does not affect harness PASS/FAIL; it's only for log scraping/diagnostics.
    """
    marker_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|")
    if marker_line is None:
        return

    status = _try_extract_marker_status(marker_line)
    if status is None:
        return

    fields = _parse_marker_kv_fields(marker_line)
    parts = [f"AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_UDP|{status}"]
    for k in ("bytes", "small_bytes", "mtu_bytes", "reason", "wsa"):
        if k in fields:
            parts.append(f"{k}={_sanitize_marker_value(fields[k])}")
    print("|".join(parts))


def _emit_virtio_net_udp_dns_host_marker(tail: bytes) -> None:
    """
    Best-effort: emit a host-side marker for the guest's UDP DNS smoke test.

    This does not affect harness PASS/FAIL; it's only for log scraping/diagnostics.

    Example guest marker:
      AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp-dns|PASS|server=10.0.2.3|query=example.com|sent=40|recv=128|rcode=0

    Example host marker:
      AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_UDP_DNS|PASS|server=10.0.2.3|query=example.com|sent=40|recv=128|rcode=0
    """
    marker_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp-dns|")
    if marker_line is None:
        return

    parts = marker_line.split("|")
    status = parts[3] if len(parts) >= 4 else "INFO"
    if status not in ("PASS", "FAIL", "SKIP", "INFO"):
        status = "INFO"

    fields = _parse_marker_kv_fields(marker_line)

    out = [f"AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_UDP_DNS|{status}"]
    for k in ("server", "query", "sent", "recv", "rcode"):
        if k in fields:
            out.append(f"{k}={_sanitize_marker_value(fields[k])}")

    # If the guest marker included a trailing reason token (no '='), preserve it for SKIP/FAIL.
    if status in ("SKIP", "FAIL") and "reason" not in fields and len(parts) >= 5:
        reason = parts[4].strip()
        if reason and "=" not in reason:
            out.append(f"reason={_sanitize_marker_value(reason)}")

    print("|".join(out))


def _emit_virtio_net_offload_csum_host_marker(tail: bytes) -> None:
    """
    Best-effort: emit a host-side marker describing the guest's checksum offload counters.

    This does not affect harness PASS/FAIL; it's only for log scraping/diagnostics.

    Example guest marker:
      AERO_VIRTIO_SELFTEST|TEST|virtio-net-offload-csum|PASS|tx_csum=...|rx_csum=...|fallback=...

    Example host marker:
      AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_OFFLOAD_CSUM|PASS|tx_csum=...|rx_csum=...|fallback=...
    """
    marker_line = _try_extract_last_marker_line(
        tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-offload-csum|"
    )
    if marker_line is None:
        return

    status = _try_extract_marker_status(marker_line)
    if status is None:
        status = "INFO"

    fields = _parse_marker_kv_fields(marker_line)

    parts = [f"AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_OFFLOAD_CSUM|{status}"]
    ordered = (
        "tx_csum",
        "rx_csum",
        "fallback",
        "tx_tcp",
        "tx_udp",
        "rx_tcp",
        "rx_udp",
        "tx_tcp4",
        "tx_tcp6",
        "tx_udp4",
        "tx_udp6",
        "rx_tcp4",
        "rx_tcp6",
        "rx_udp4",
        "rx_udp6",
    )
    for k in ordered:
        if k in fields:
            parts.append(f"{k}={_sanitize_marker_value(fields[k])}")
    extra = sorted(k for k in fields if k not in ordered)
    for k in extra:
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
        "tx_udp_csum_v4",
        "tx_udp_csum_v6",
        "tx_tcp_csum_offload_pkts",
        "tx_tcp_csum_fallback_pkts",
        "tx_udp_csum_offload_pkts",
        "tx_udp_csum_fallback_pkts",
        "tx_tso_v4",
        "tx_tso_v6",
        "tx_tso_max_size",
        "ctrl_vq",
        "ctrl_rx",
        "ctrl_vlan",
        "ctrl_mac_addr",
        "ctrl_queue_index",
        "ctrl_queue_size",
        "ctrl_error_flags",
        "ctrl_cmd_sent",
        "ctrl_cmd_ok",
        "ctrl_cmd_err",
        "ctrl_cmd_timeout",
        "perm_mac",
        "cur_mac",
        "link_up",
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


def _emit_virtio_blk_miniport_flags_host_marker(
    tail: bytes, *, marker_line: Optional[str] = None
) -> None:
    """
    Best-effort: emit a host-side marker mirroring the guest's `virtio-blk-miniport-flags|...` diagnostics.

    Guest markers (selftest diagnostic lines):
      virtio-blk-miniport-flags|INFO|raw=0x...|removed=...|surprise_removed=...|reset_in_progress=...|reset_pending=...
      virtio-blk-miniport-flags|WARN|reason=...|returned_len=...|expected_min=...

    Host marker:
      AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_MINIPORT_FLAGS|INFO/WARN|...

    This does not affect harness PASS/FAIL; it's only for log scraping/diagnostics.
    """
    if marker_line is None:
        marker_line = _try_extract_last_marker_line(tail, b"virtio-blk-miniport-flags|")
    if marker_line is None:
        return

    toks = marker_line.split("|")
    raw_status = toks[1] if len(toks) >= 2 else "INFO"
    raw_status = raw_status.strip().upper()
    status = "WARN" if raw_status == "WARN" else "INFO"

    fields = _parse_marker_kv_fields(marker_line)
    parts = [f"AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_MINIPORT_FLAGS|{status}"]

    ordered = (
        "raw",
        "removed",
        "surprise_removed",
        "reset_in_progress",
        "reset_pending",
        "reason",
        "returned_len",
        "expected_min",
    )
    for k in ordered:
        if k in fields:
            parts.append(f"{k}={_sanitize_marker_value(fields[k])}")

    extra = sorted(k for k in fields if k not in ordered)
    for k in extra:
        parts.append(f"{k}={_sanitize_marker_value(fields[k])}")

    print("|".join(parts))


def _emit_virtio_blk_miniport_reset_recovery_host_marker(
    tail: bytes, *, marker_line: Optional[str] = None
) -> None:
    """
    Best-effort: emit a host-side marker mirroring the guest's `virtio-blk-miniport-reset-recovery|...` diagnostics.

    Guest markers (selftest diagnostic lines):
      virtio-blk-miniport-reset-recovery|INFO|reset_detected=...|hw_reset_bus=...
      virtio-blk-miniport-reset-recovery|WARN|reason=...|returned_len=...|expected_min=...

    Host marker:
      AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_MINIPORT_RESET_RECOVERY|INFO/WARN|...

    This does not affect harness PASS/FAIL; it's only for log scraping/diagnostics.
    """
    if marker_line is None:
        marker_line = _try_extract_last_marker_line(tail, b"virtio-blk-miniport-reset-recovery|")
    if marker_line is None:
        return

    toks = marker_line.split("|")
    raw_status = toks[1] if len(toks) >= 2 else "INFO"
    raw_status = raw_status.strip().upper()
    status = "WARN" if raw_status == "WARN" else "INFO"

    fields = _parse_marker_kv_fields(marker_line)
    parts = [f"AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_MINIPORT_RESET_RECOVERY|{status}"]

    ordered = (
        "reset_detected",
        "hw_reset_bus",
        "reason",
        "returned_len",
        "expected_min",
    )
    for k in ordered:
        if k in fields:
            parts.append(f"{k}={_sanitize_marker_value(fields[k])}")

    extra = sorted(k for k in fields if k not in ordered)
    for k in extra:
        parts.append(f"{k}={_sanitize_marker_value(fields[k])}")

    print("|".join(parts))


_VIRTIO_BLK_RECOVERY_KEYS = (
    "abort_srb",
    "reset_device_srb",
    "reset_bus_srb",
    "pnp_srb",
    "ioctl_reset",
)

_VIRTIO_BLK_COUNTERS_KEYS = (
    "abort",
    "reset_device",
    "reset_bus",
    "pnp",
    "ioctl_reset",
)

_VIRTIO_BLK_RESET_RECOVERY_KEYS = (
    "reset_detected",
    "hw_reset_bus",
)

_VIRTIO_BLK_MINIPORT_FLAGS_KEYS = (
    "raw",
    "removed",
    "surprise_removed",
    "reset_in_progress",
    "reset_pending",
)


def _try_parse_int_base0(s: str) -> Optional[int]:
    try:
        return int(s, 0)
    except Exception:
        return None


def _try_parse_virtio_blk_recovery_counters_from_blk_counters_marker(
    tail: bytes, *, blk_counters_line: Optional[str] = None
) -> Optional[dict[str, int]]:
    """
    Best-effort: extract virtio-blk recovery counters from the dedicated `virtio-blk-counters` guest marker.

    Guest marker:
      AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|INFO|abort=...|reset_device=...|reset_bus=...|pnp=...|ioctl_reset=...|capacity_change_events=<n|not_supported>
    """
    marker_line = blk_counters_line
    if marker_line is None:
        marker_line = _try_extract_last_marker_line(
            tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|"
        )
    if marker_line is None:
        return None

    toks = marker_line.split("|")
    status = toks[3] if len(toks) >= 4 else ""
    if status == "SKIP":
        return None

    fields = _parse_marker_kv_fields(marker_line)
    if not any(k in fields for k in _VIRTIO_BLK_COUNTERS_KEYS):
        return None

    mapped: dict[str, int] = {}
    mapping = {
        "abort": "abort_srb",
        "reset_device": "reset_device_srb",
        "reset_bus": "reset_bus_srb",
        "pnp": "pnp_srb",
        "ioctl_reset": "ioctl_reset",
    }

    for src, dst in mapping.items():
        if src not in fields:
            return None
        v = _try_parse_int_base0(fields[src])
        if v is None:
            return None
        mapped[dst] = v
    return mapped


def _try_parse_virtio_blk_recovery_counters(
    tail: bytes,
    *,
    blk_test_line: Optional[str] = None,
    blk_counters_line: Optional[str] = None,
) -> Optional[dict[str, int]]:
    """
    Best-effort: extract virtio-blk StorPort recovery counters from the guest virtio-blk test marker.

    The guest selftest may append the following fields when the virtio-blk miniport IOCTL contract
    includes the recovery counters region:
      abort_srb, reset_device_srb, reset_bus_srb, pnp_srb, ioctl_reset
    """
    if blk_test_line is None:
        blk_test_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|")
    if blk_test_line is None:
        return None

    fields = _parse_marker_kv_fields(blk_test_line)
    if any(k in fields for k in _VIRTIO_BLK_RECOVERY_KEYS):
        counters: dict[str, int] = {}
        for k in _VIRTIO_BLK_RECOVERY_KEYS:
            if k not in fields:
                break
            v = _try_parse_int_base0(fields[k])
            if v is None:
                break
            counters[k] = v
        else:
            return counters

    # Backward/robustness: if the virtio-blk per-test marker does not include the counters fields (or is truncated),
    # fall back to the dedicated virtio-blk-counters marker.
    return _try_parse_virtio_blk_recovery_counters_from_blk_counters_marker(
        tail, blk_counters_line=blk_counters_line
    )


def _virtio_blk_recovery_is_nonzero(counters: dict[str, int], *, threshold: int = 0) -> bool:
    return any(v > threshold for v in counters.values())


def _virtio_blk_recovery_failure_message(counters: dict[str, int]) -> str:
    parts = ["FAIL: VIRTIO_BLK_RECOVERY_NONZERO:"]
    for k in _VIRTIO_BLK_RECOVERY_KEYS:
        if k in counters:
            parts.append(f"{k}={counters[k]}")
    return " ".join(parts)


def _check_no_blk_recovery_requirement(
    tail: bytes,
    *,
    threshold: int = 0,
    blk_test_line: Optional[str] = None,
    blk_counters_line: Optional[str] = None,
) -> Optional[str]:
    counters = _try_parse_virtio_blk_recovery_counters(
        tail, blk_test_line=blk_test_line, blk_counters_line=blk_counters_line
    )
    if counters is None:
        return None
    if _virtio_blk_recovery_is_nonzero(counters, threshold=threshold):
        return _virtio_blk_recovery_failure_message(counters)
    return None


def _check_fail_on_blk_recovery_requirement(
    tail: bytes,
    *,
    threshold: int = 0,
    blk_test_line: Optional[str] = None,
    blk_counters_line: Optional[str] = None,
) -> Optional[str]:
    """
    Enforce that virtio-blk did not trigger StorPort recovery activity.

    This uses the dedicated guest marker:
      AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|INFO|abort=...|reset_device=...|reset_bus=...|pnp=...|ioctl_reset=...|capacity_change_events=<n|not_supported>
      AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|SKIP|reason=...|returned_len=...

    Behavior:
    - If the marker is present but SKIP: treat counters as unavailable (no failure; does not fall back).
    - If any of abort/reset_device/reset_bus exceed `threshold`: return a FAIL message.
    - If the marker is missing entirely: fall back to legacy abort_srb/reset_*_srb fields on the virtio-blk test marker.
    """
    if blk_counters_line is None:
        blk_counters_line = _try_extract_last_marker_line(
            tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|"
        )
    if blk_counters_line:
        toks = blk_counters_line.split("|")
        status = toks[3] if len(toks) >= 4 else ""
        if status == "SKIP":
            return None

        fields = _parse_marker_kv_fields(blk_counters_line)
        want = ("abort", "reset_device", "reset_bus")
        counters: dict[str, int] = {}
        for k in want:
            if k not in fields:
                return None
            v = _try_parse_int_base0(fields[k])
            if v is None:
                return None
            counters[k] = v

        if (
            counters["abort"] > threshold
            or counters["reset_device"] > threshold
            or counters["reset_bus"] > threshold
        ):
            return (
                "FAIL: VIRTIO_BLK_RECOVERY_DETECTED:"
                f" abort={counters['abort']} reset_device={counters['reset_device']} reset_bus={counters['reset_bus']}"
            )
        return None

    # Backward compatible fallback: older guest selftests emitted the counters on the virtio-blk
    # per-test marker rather than the dedicated virtio-blk-counters marker.
    if blk_test_line is None:
        blk_test_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|")
    if not blk_test_line:
        return None

    fields = _parse_marker_kv_fields(blk_test_line)
    mapping = {
        "abort_srb": "abort",
        "reset_device_srb": "reset_device",
        "reset_bus_srb": "reset_bus",
    }
    counters2: dict[str, int] = {}
    for src, dst in mapping.items():
        if src not in fields:
            return None
        v = _try_parse_int_base0(fields[src])
        if v is None:
            return None
        counters2[dst] = v

    if (
        counters2["abort"] > threshold
        or counters2["reset_device"] > threshold
        or counters2["reset_bus"] > threshold
    ):
        return (
            "FAIL: VIRTIO_BLK_RECOVERY_DETECTED:"
            f" abort={counters2['abort']} reset_device={counters2['reset_device']} reset_bus={counters2['reset_bus']}"
        )
    return None


def _try_parse_virtio_blk_reset_recovery_counters(
    tail: bytes, *, blk_reset_recovery_line: Optional[str] = None
) -> Optional[dict[str, int]]:
    """
    Best-effort: extract virtio-blk timeout/error recovery counters.

    Preferred source (newer guest selftests):
      AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|INFO|reset_detected=...|hw_reset_bus=...
      AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|SKIP|reason=...|returned_len=...

    Backward compatible fallback (older guest selftests):
      virtio-blk-miniport-reset-recovery|INFO|reset_detected=...|hw_reset_bus=...
      virtio-blk-miniport-reset-recovery|WARN|reason=...|returned_len=...|expected_min=...
    """
    marker_line = blk_reset_recovery_line
    if marker_line is None:
        marker_line = _try_extract_last_marker_line(
            tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|"
        )
        if marker_line is None:
            marker_line = _try_extract_last_marker_line(
                tail, b"virtio-blk-miniport-reset-recovery|"
            )
    if not marker_line:
        return None

    if marker_line.startswith("AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|"):
        toks = marker_line.split("|")
        status = toks[3] if len(toks) >= 4 else ""
        if status == "SKIP":
            return None
    elif marker_line.startswith("virtio-blk-miniport-reset-recovery|"):
        toks = marker_line.split("|")
        level = toks[1].strip().upper() if len(toks) >= 2 else ""
        if level != "INFO":
            return None
    else:
        return None

    fields = _parse_marker_kv_fields(marker_line)
    counters: dict[str, int] = {}
    for k in _VIRTIO_BLK_RESET_RECOVERY_KEYS:
        if k not in fields:
            return None
        v = _try_parse_int_base0(fields[k])
        if v is None:
            return None
        counters[k] = v
    return counters


def _check_no_blk_reset_recovery_requirement(
    tail: bytes,
    *,
    threshold: int = 0,
    blk_reset_recovery_line: Optional[str] = None,
) -> Optional[str]:
    counters = _try_parse_virtio_blk_reset_recovery_counters(
        tail, blk_reset_recovery_line=blk_reset_recovery_line
    )
    if counters is None:
        return None
    if counters["reset_detected"] > threshold or counters["hw_reset_bus"] > threshold:
        return (
            "FAIL: VIRTIO_BLK_RESET_RECOVERY_NONZERO:"
            f" reset_detected={counters['reset_detected']} hw_reset_bus={counters['hw_reset_bus']}"
        )
    return None


def _check_fail_on_blk_reset_recovery_requirement(
    tail: bytes,
    *,
    threshold: int = 0,
    blk_reset_recovery_line: Optional[str] = None,
) -> Optional[str]:
    counters = _try_parse_virtio_blk_reset_recovery_counters(
        tail, blk_reset_recovery_line=blk_reset_recovery_line
    )
    if counters is None:
        return None
    # Looser mode: only treat an actual StorPort HwResetBus invocation as a failure.
    if counters["hw_reset_bus"] > threshold:
        return (
            "FAIL: VIRTIO_BLK_RESET_RECOVERY_DETECTED:"
            f" hw_reset_bus={counters['hw_reset_bus']} reset_detected={counters['reset_detected']}"
        )
    return None


def _try_parse_virtio_blk_miniport_flags(
    tail: bytes, *, blk_miniport_flags_line: Optional[str] = None
) -> Optional[dict[str, int]]:
    """
    Best-effort: parse the guest virtio-blk miniport flags diagnostic line.

    Guest diagnostic line format (not an AERO marker):
      virtio-blk-miniport-flags|INFO|raw=0x...|removed=<0|1>|surprise_removed=<0|1>|reset_in_progress=<0|1>|reset_pending=<0|1>
      virtio-blk-miniport-flags|WARN|reason=...|returned_len=...|expected_min=...

    Returns a dict of parsed integer fields on INFO, otherwise None.
    """
    marker_line = blk_miniport_flags_line
    if marker_line is None:
        marker_line = _try_extract_last_marker_line(tail, b"virtio-blk-miniport-flags|")
    if not marker_line:
        return None

    toks = marker_line.split("|")
    level = toks[1].strip().upper() if len(toks) >= 2 else ""
    if level != "INFO":
        return None

    fields = _parse_marker_kv_fields(marker_line)
    out: dict[str, int] = {}
    for k in _VIRTIO_BLK_MINIPORT_FLAGS_KEYS:
        if k not in fields:
            return None
        v = _try_parse_int_base0(fields[k])
        if v is None:
            return None
        out[k] = v
    return out


def _check_no_blk_miniport_flags_requirement(
    tail: bytes,
    *,
    threshold: int = 0,
    blk_miniport_flags_line: Optional[str] = None,
) -> Optional[str]:
    flags = _try_parse_virtio_blk_miniport_flags(
        tail, blk_miniport_flags_line=blk_miniport_flags_line
    )
    if flags is None:
        return None
    if (
        flags["removed"] > threshold
        or flags["surprise_removed"] > threshold
        or flags["reset_in_progress"] > threshold
        or flags["reset_pending"] > threshold
    ):
        raw = f"0x{flags['raw']:08x}"
        return (
            "FAIL: VIRTIO_BLK_MINIPORT_FLAGS_NONZERO:"
            f" raw={raw} removed={flags['removed']} surprise_removed={flags['surprise_removed']}"
            f" reset_in_progress={flags['reset_in_progress']} reset_pending={flags['reset_pending']}"
        )
    return None


def _check_fail_on_blk_miniport_flags_requirement(
    tail: bytes,
    *,
    threshold: int = 0,
    blk_miniport_flags_line: Optional[str] = None,
) -> Optional[str]:
    flags = _try_parse_virtio_blk_miniport_flags(
        tail, blk_miniport_flags_line=blk_miniport_flags_line
    )
    if flags is None:
        return None
    if flags["removed"] > threshold or flags["surprise_removed"] > threshold:
        raw = f"0x{flags['raw']:08x}"
        return (
            "FAIL: VIRTIO_BLK_MINIPORT_FLAGS_REMOVED:"
            f" raw={raw} removed={flags['removed']} surprise_removed={flags['surprise_removed']}"
        )
    return None


def _emit_virtio_blk_recovery_host_marker(
    tail: bytes,
    *,
    blk_test_line: Optional[str] = None,
    blk_counters_line: Optional[str] = None,
) -> None:
    """
    Best-effort: emit a host-side marker describing the guest virtio-blk StorPort recovery counters.

    This does not affect harness PASS/FAIL by itself; gating is controlled by --require-no-blk-recovery.
    """
    counters = _try_parse_virtio_blk_recovery_counters(
        tail, blk_test_line=blk_test_line, blk_counters_line=blk_counters_line
    )
    if counters is None:
        return

    parts = ["AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RECOVERY|INFO"]
    for k in _VIRTIO_BLK_RECOVERY_KEYS:
        if k in counters:
            parts.append(f"{k}={_sanitize_marker_value(str(counters[k]))}")

    print("|".join(parts))


def _emit_virtio_input_binding_host_marker(tail: bytes, *, marker_line: Optional[str] = None) -> None:
    """
    Best-effort: emit a host-side marker mirroring the guest's virtio-input PCI binding validation.

    Guest marker format:
      AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|PASS|service=...|pnp_id=...
      AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|FAIL|reason=...|expected=...|actual=...|pnp_id=...

    This does not affect harness PASS/FAIL unless --require-virtio-input-binding is enabled; it's also useful for
    log scraping/CI diagnostics.
    """
    if marker_line is None:
        marker_line = _try_extract_last_marker_line(
            tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|"
        )
    if marker_line is None:
        return

    status = _try_extract_marker_status(marker_line)
    if status is None:
        return

    fields = _parse_marker_kv_fields(marker_line)
    parts = [f"AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_BINDING|{status}"]

    ordered = (
        "reason",
        "expected",
        "actual",
        "service",
        "pnp_id",
        "hwid0",
        "cm_problem",
        "cm_status",
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
    #     - `virtio-blk-miniport-irq|...` (miniport IOCTL-derived mode/messages/message_count/MSI-X vectors)
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
            blk_test_fields.get("messages")
            or blk_test_fields.get("message_count")
            or blk_test_fields.get("irq_messages")
            or blk_test_fields.get("msi_messages"),
        )

    def _apply_irq_diag(diag: dict[str, str]) -> None:
        diag_mode = diag.get("irq_mode") or diag.get("mode") or diag.get("interrupt_mode")
        # Some guest diagnostics (notably virtio-blk miniport IOCTL output) intentionally conflate
        # MSI and MSI-X in the reported `mode` field (e.g. `mode=msi` even when MSI-X vectors are
        # assigned). If the diagnostic includes MSI-X vector indices, treat this as MSI-X so the
        # stable host marker matches the per-test marker semantics (`irq_mode=msix`).
        if diag_mode == "msi":
            for vec_key in ("msix_config_vector", "msix_queue_vector", "msix_queue0_vector"):
                vec = diag.get(vec_key)
                if vec is None:
                    continue
                v = vec.strip().lower()
                if not v or v in ("none", "0xffff", "65535"):
                    continue
                diag_mode = "msix"
                break

        _set_if_missing("irq_mode", diag_mode)
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

    fields = _parse_marker_kv_fields(marker_line)
    parts = marker_line.split("|")
    if "FAIL" in parts:
        reason = "virtio-blk-msix marker reported FAIL"
        if "reason" in fields:
            reason += f" reason={fields['reason']}"
        if "err" in fields:
            reason += f" err={fields['err']}"
        return False, reason
    if "SKIP" in parts:
        reason = "virtio-blk-msix marker reported SKIP"
        if "reason" in fields:
            reason += f" reason={fields['reason']}"
        if "err" in fields:
            reason += f" err={fields['err']}"
        return False, reason

    mode = fields.get("mode")
    if mode is None:
        return False, "virtio-blk-msix marker missing mode=... field"
    if mode != "msix":
        msgs = fields.get("messages", "?")
        return False, f"mode={mode} (expected msix) messages={msgs}"
    return True, "ok"


def _require_virtio_net_msix_marker(tail: bytes) -> tuple[bool, str]:
    """
    Return (ok, reason). `ok` is True iff the guest reported virtio-net running in MSI-X mode
    via the marker: AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|PASS|mode=msix|...
    """
    marker_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|")
    if marker_line is None:
        return False, "missing virtio-net-msix marker (guest selftest too old?)"

    fields = _parse_marker_kv_fields(marker_line)
    parts = marker_line.split("|")
    if "FAIL" in parts:
        reason = "virtio-net-msix marker reported FAIL"
        if "reason" in fields:
            reason += f" reason={fields['reason']}"
        if "err" in fields:
            reason += f" err={fields['err']}"
        return False, reason
    if "SKIP" in parts:
        reason = "virtio-net-msix marker reported SKIP"
        if "reason" in fields:
            reason += f" reason={fields['reason']}"
        if "err" in fields:
            reason += f" err={fields['err']}"
        return False, reason

    mode = fields.get("mode")
    if mode is None:
        return False, "virtio-net-msix marker missing mode=... field"
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

    fields = _parse_marker_kv_fields(marker_line)
    parts = marker_line.split("|")
    if "FAIL" in parts:
        reason = "virtio-snd-msix marker reported FAIL"
        if "reason" in fields:
            reason += f" reason={fields['reason']}"
        if "err" in fields:
            reason += f" err={fields['err']}"
        return False, reason
    if "SKIP" in parts:
        reason = "virtio-snd-msix marker reported SKIP"
        if "reason" in fields:
            reason += f" reason={fields['reason']}"
        if "err" in fields:
            reason += f" err={fields['err']}"
        return False, reason

    mode = fields.get("mode")
    if mode is None:
        return False, "virtio-snd-msix marker missing mode=... field"
    if mode != "msix":
        msgs = fields.get("messages", "?")
        return False, f"mode={mode} (expected msix) messages={msgs}"
    return True, "ok"

def _require_virtio_input_msix_marker(tail: bytes) -> tuple[bool, str]:
    """
    Return (ok, reason). `ok` is True iff the guest reported virtio-input running in MSI-X mode
    via the marker: AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|PASS|mode=msix|...
    """
    marker_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|")
    if marker_line is None:
        return False, "missing virtio-input-msix marker (guest selftest too old?)"

    fields = _parse_marker_kv_fields(marker_line)

    parts = marker_line.split("|")
    if "FAIL" in parts:
        reason = "virtio-input-msix marker reported FAIL"
        if "reason" in fields:
            reason += f" reason={fields['reason']}"
        if "err" in fields:
            reason += f" err={fields['err']}"
        return False, reason
    if "SKIP" in parts:
        reason = "virtio-input-msix marker reported SKIP"
        if "reason" in fields:
            reason += f" reason={fields['reason']}"
        if "err" in fields:
            reason += f" err={fields['err']}"
        return False, reason
    mode = fields.get("mode")
    if mode is None:
        return False, "virtio-input-msix marker missing mode=... field"
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


def _emit_virtio_net_msix_host_marker(tail: bytes) -> None:
    """
    Best-effort: emit a host-side marker mirroring the guest `virtio-net-msix` TEST marker.

    The guest selftest may emit:
      AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|PASS/FAIL/SKIP|mode=...|messages=...|config_vector=<n|none>|rx_vector=<n|none>|tx_vector=<n|none>|...

    Mirror it into:
      AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_MSIX|PASS/FAIL/SKIP|mode=...|messages=...|config_vector=<n|none>|rx_vector=<n|none>|tx_vector=<n|none>|...

    This does not affect harness PASS/FAIL; it's only for log scraping/diagnostics.
    """
    marker_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|")
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

    fields = _parse_marker_kv_fields(marker_line)
    parts = [f"AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_MSIX|{status}"]

    # Keep ordering stable for log scraping.
    ordered = [
        "mode",
        "messages",
        "config_vector",
        "rx_vector",
        "tx_vector",
        "bytes",
        "reason",
        "err",
    ]
    for k in ordered:
        if k in fields:
            parts.append(f"{k}={_sanitize_marker_value(fields[k])}")

    extra = sorted(k for k in fields if k not in ordered)
    for k in extra:
        parts.append(f"{k}={_sanitize_marker_value(fields[k])}")

    print("|".join(parts))


def _emit_virtio_blk_msix_host_marker(tail: bytes) -> None:
    """
    Best-effort: emit a host-side marker mirroring the guest `virtio-blk-msix` TEST marker.

    The guest selftest may emit:
      AERO_VIRTIO_SELFTEST|TEST|virtio-blk-msix|PASS/SKIP|mode=...|messages=...|config_vector=...|queue_vector=...

    Mirror it into:
      AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_MSIX|PASS/SKIP|mode=...|messages=...|config_vector=...|queue_vector=...

    This does not affect harness PASS/FAIL; it's only for log scraping/diagnostics.
    """
    marker_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-msix|")
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

    fields = _parse_marker_kv_fields(marker_line)
    parts = [f"AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_MSIX|{status}"]

    # Keep ordering stable for log scraping.
    ordered = [
        "mode",
        "messages",
        "config_vector",
        "queue_vector",
        "returned_len",
        "reason",
        "err",
    ]
    for k in ordered:
        if k in fields:
            parts.append(f"{k}={_sanitize_marker_value(fields[k])}")

    extra = sorted(k for k in fields if k not in ordered)
    for k in extra:
        parts.append(f"{k}={_sanitize_marker_value(fields[k])}")

    print("|".join(parts))


def _emit_virtio_snd_msix_host_marker(tail: bytes) -> None:
    """
    Best-effort: emit a host-side marker mirroring the guest `virtio-snd-msix` TEST marker.

    The guest selftest emits:
      AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|PASS/FAIL/SKIP|mode=...|messages=...|config_vector=...|queue0_vector=...|queue1_vector=...|queue2_vector=...|queue3_vector=...|interrupts=...|dpcs=...|drain0=...|drain1=...|drain2=...|drain3=...|...

    Mirror it into:
      AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_MSIX|PASS/FAIL/SKIP|mode=...|messages=...|config_vector=...|queue0_vector=...|queue1_vector=...|queue2_vector=...|queue3_vector=...|interrupts=...|dpcs=...|drain0=...|drain1=...|drain2=...|drain3=...|...

    This does not affect harness PASS/FAIL; it's only for log scraping/diagnostics.
    """
    marker_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|")
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

    fields = _parse_marker_kv_fields(marker_line)
    parts = [f"AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_MSIX|{status}"]

    # Keep ordering stable for log scraping.
    ordered = [
        "mode",
        "messages",
        "config_vector",
        "queue0_vector",
        "queue1_vector",
        "queue2_vector",
        "queue3_vector",
        "interrupts",
        "dpcs",
        "drain0",
        "drain1",
        "drain2",
        "drain3",
        "reason",
        "err",
    ]
    for k in ordered:
        if k in fields:
            parts.append(f"{k}={_sanitize_marker_value(fields[k])}")

    extra = sorted(k for k in fields if k not in ordered)
    for k in extra:
        parts.append(f"{k}={_sanitize_marker_value(fields[k])}")

    print("|".join(parts))


def _emit_virtio_input_msix_host_marker(tail: bytes) -> None:
    """
    Best-effort: emit a host-side marker mirroring the guest `virtio-input-msix` TEST marker.

    The guest selftest emits:
      AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|PASS/FAIL/SKIP|mode=...|messages=...|mapping=...|...

    Mirror it into:
      AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_MSIX|PASS/FAIL/SKIP|mode=...|messages=...|mapping=...|...

    This does not affect harness PASS/FAIL; it's only for log scraping/diagnostics.
    """
    marker_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|")
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

    fields = _parse_marker_kv_fields(marker_line)
    parts = [f"AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_MSIX|{status}"]

    # Keep ordering stable for log scraping.
    ordered = [
        "mode",
        "messages",
        "mapping",
        "used_vectors",
        "config_vector",
        "queue0_vector",
        "queue1_vector",
        "msix_devices",
        "intx_devices",
        "unknown_devices",
        "intx_spurious",
        "total_interrupts",
        "total_dpcs",
        "config_irqs",
        "queue0_irqs",
        "queue1_irqs",
        "reason",
        "err",
    ]
    for k in ordered:
        if k in fields:
            parts.append(f"{k}={_sanitize_marker_value(fields[k])}")

    extra = sorted(k for k in fields if k not in ordered)
    for k in extra:
        parts.append(f"{k}={_sanitize_marker_value(fields[k])}")

    print("|".join(parts))


def _emit_virtio_input_bind_host_marker(tail: bytes) -> None:
    """
    Best-effort: emit a host-side marker mirroring the guest `virtio-input-bind` TEST marker.

    The guest selftest emits:
      AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|PASS|service=...|pnp_id=...|devices=...|...
      AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|FAIL|reason=...|expected=...|actual=...|pnp_id=...|devices=...|...

    Mirror it into:
      AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_BIND|PASS/FAIL|service=...|pnp_id=...|devices=...|...

    This does not affect harness PASS/FAIL; it's only for log scraping/diagnostics.
    """
    marker_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|")
    if marker_line is None:
        return

    status = _try_extract_marker_status(marker_line)
    if status is None:
        return

    fields = _parse_marker_kv_fields(marker_line)
    parts = [f"AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_BIND|{status}"]

    # Keep ordering stable for log scraping.
    ordered = (
        "reason",
        "service",
        "expected",
        "actual",
        "pnp_id",
        "devices",
        "wrong_service",
        "missing_service",
        "problem",
    )
    for k in ordered:
        if k in fields:
            parts.append(f"{k}={_sanitize_marker_value(fields[k])}")

    extra = sorted(k for k in fields if k not in ordered)
    for k in extra:
        parts.append(f"{k}={_sanitize_marker_value(fields[k])}")

    print("|".join(parts))


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

    # For FAIL/SKIP markers that use a plain token (e.g. `...|FAIL|force_null_backend|...`),
    # mirror it into reason=... so log scraping can treat it uniformly.
    if status in ("FAIL", "SKIP") and "reason" not in fields:
        toks = marker_line.split("|")
        try:
            idx = toks.index(status)
            if idx + 1 < len(toks):
                reason_tok = toks[idx + 1].strip()
                if reason_tok and "=" not in reason_tok:
                    fields["reason"] = reason_tok
        except ValueError:
            pass

    # Keep ordering stable for log scraping: reason first, then remaining keys sorted.
    if "reason" in fields:
        parts.append(f"reason={_sanitize_marker_value(fields['reason'])}")
    for k in sorted(k for k in fields if k != "reason"):
        parts.append(f"{k}={_sanitize_marker_value(fields[k])}")
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

    # For FAIL/SKIP markers that use a plain token (e.g. `...|SKIP|endpoint_missing`), mirror
    # it into reason=... so log scraping can treat it uniformly.
    if status in ("FAIL", "SKIP") and "reason" not in fields:
        toks = marker_line.split("|")
        try:
            idx = toks.index(status)
            if idx + 1 < len(toks):
                reason_tok = toks[idx + 1].strip()
                if reason_tok and "=" not in reason_tok:
                    fields["reason"] = reason_tok
        except ValueError:
            pass

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

    # For FAIL/SKIP markers that use a plain token (e.g. `...|SKIP|flag_not_set`), mirror
    # it into reason=... so log scraping can treat it uniformly.
    if status in ("FAIL", "SKIP") and "reason" not in fields:
        toks = marker_line.split("|")
        try:
            idx = toks.index(status)
            if idx + 1 < len(toks):
                reason_tok = toks[idx + 1].strip()
                if reason_tok and "=" not in reason_tok:
                    fields["reason"] = reason_tok
        except ValueError:
            pass

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

    # For FAIL/SKIP markers that use a plain token (e.g. `...|SKIP|flag_not_set`), mirror
    # it into reason=... so log scraping can treat it uniformly.
    if status in ("FAIL", "SKIP") and "reason" not in fields:
        toks = marker_line.split("|")
        try:
            idx = toks.index(status)
            if idx + 1 < len(toks):
                reason_tok = toks[idx + 1].strip()
                if reason_tok and "=" not in reason_tok:
                    fields["reason"] = reason_tok
        except ValueError:
            pass

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


def _emit_virtio_blk_counters_host_marker(
    tail: bytes, *, blk_counters_line: Optional[str] = None, blk_test_line: Optional[str] = None
) -> None:
    """
    Best-effort: emit a host-side marker mirroring the guest's virtio-blk recovery/reset/abort counters.

    Guest marker:
      AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|INFO|abort=...|reset_device=...|reset_bus=...|pnp=...|ioctl_reset=...|capacity_change_events=<n|not_supported>
      AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|SKIP|reason=...|returned_len=...

    Host marker:
      AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_COUNTERS|INFO/SKIP|...

    This does not affect harness PASS/FAIL; it's only for log scraping/diagnostics.
    """
    marker_line = blk_counters_line
    if marker_line is None:
        marker_line = _try_extract_last_marker_line(
            tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|"
        )
    if marker_line is None:
        # Backward compatible fallback: older guest selftests emitted the counters on the virtio-blk
        # per-test marker rather than the dedicated virtio-blk-counters marker.
        if blk_test_line is None:
            blk_test_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|")
        if blk_test_line is None:
            return
        fields = _parse_marker_kv_fields(blk_test_line)
        mapping = {
            "abort_srb": "abort",
            "reset_device_srb": "reset_device",
            "reset_bus_srb": "reset_bus",
            "pnp_srb": "pnp",
            "ioctl_reset": "ioctl_reset",
        }
        mapped: dict[str, str] = {}
        for src, dst in mapping.items():
            if src in fields:
                mapped[dst] = fields[src]
        if not mapped:
            return
        parts = ["AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_COUNTERS|INFO"]
        for k in ("abort", "reset_device", "reset_bus", "pnp", "ioctl_reset"):
            if k in mapped:
                parts.append(f"{k}={_sanitize_marker_value(mapped[k])}")
        print("|".join(parts))
        return

    toks = marker_line.split("|")
    raw_status = toks[3] if len(toks) >= 4 else "INFO"
    raw_status = raw_status.strip().upper()
    # Keep the host marker stable: treat any non-SKIP guest status as INFO.
    status = "SKIP" if raw_status == "SKIP" else "INFO"

    fields = _parse_marker_kv_fields(marker_line)
    parts = [f"AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_COUNTERS|{status}"]

    ordered = (
        "abort",
        "reset_device",
        "reset_bus",
        "pnp",
        "ioctl_reset",
        "capacity_change_events",
        "reason",
        "returned_len",
    )
    for k in ordered:
        if k in fields:
            parts.append(f"{k}={_sanitize_marker_value(fields[k])}")

    extra = sorted(k for k in fields if k not in ordered)
    for k in extra:
        parts.append(f"{k}={_sanitize_marker_value(fields[k])}")
    print("|".join(parts))


def _emit_virtio_blk_reset_recovery_host_marker(
    tail: bytes, *, blk_reset_recovery_line: Optional[str] = None
) -> None:
    """
    Best-effort: emit a host-side marker mirroring the guest's virtio-blk reset-recovery counters.

    Preferred guest marker (newer guest selftests):
      AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|INFO|reset_detected=...|hw_reset_bus=...
      AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|SKIP|reason=...|returned_len=...

    Backward compatible guest diagnostic (older guest selftests):
      virtio-blk-miniport-reset-recovery|INFO|reset_detected=...|hw_reset_bus=...
      virtio-blk-miniport-reset-recovery|WARN|reason=...|returned_len=...|expected_min=...

    Host marker:
      AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESET_RECOVERY|INFO/SKIP|...

    This does not affect harness PASS/FAIL; it's only for log scraping/diagnostics.
    """
    marker_line = blk_reset_recovery_line
    if marker_line is None:
        marker_line = _try_extract_last_marker_line(
            tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|"
        )
        if marker_line is None:
            marker_line = _try_extract_last_marker_line(
                tail, b"virtio-blk-miniport-reset-recovery|"
            )
    if marker_line is None:
        return

    if marker_line.startswith("AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|"):
        toks = marker_line.split("|")
        raw_status = toks[3] if len(toks) >= 4 else "INFO"
        raw_status = raw_status.strip().upper()
        status = "SKIP" if raw_status == "SKIP" else "INFO"
    elif marker_line.startswith("virtio-blk-miniport-reset-recovery|"):
        toks = marker_line.split("|")
        level = toks[1].strip().upper() if len(toks) >= 2 else ""
        if level == "INFO":
            status = "INFO"
        elif level == "WARN":
            status = "SKIP"
        else:
            return
    else:
        return

    fields = _parse_marker_kv_fields(marker_line)
    parts = [f"AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESET_RECOVERY|{status}"]

    ordered = (
        "reset_detected",
        "hw_reset_bus",
        "reason",
        "returned_len",
        "expected_min",
    )
    for k in ordered:
        if k in fields:
            parts.append(f"{k}={_sanitize_marker_value(fields[k])}")

    extra = sorted(k for k in fields if k not in ordered)
    for k in extra:
        parts.append(f"{k}={_sanitize_marker_value(fields[k])}")

    print("|".join(parts))


def _emit_virtio_blk_resize_host_marker(tail: bytes, *, blk_resize_line: Optional[str] = None) -> None:
    """
    Best-effort: emit a host-side marker summarizing the guest's virtio-blk runtime resize selftest.

    Guest markers:
      AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|READY|disk=<N>|old_bytes=<u64>
      AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|PASS|disk=<N>|old_bytes=<u64>|new_bytes=<u64>|elapsed_ms=<u32>
      AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|FAIL|reason=...|disk=<N>|old_bytes=<u64>|last_bytes=<u64>|err=<GetLastError>
      AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|SKIP|flag_not_set

    Host marker:
      AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESIZE|PASS/FAIL/SKIP/READY|...

    Note: this does not affect harness PASS/FAIL; it is intended for log scraping/diagnostics.
    """
    marker_line = blk_resize_line
    if marker_line is None:
        marker_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|")
    if marker_line is None:
        return

    toks = marker_line.split("|")

    status = toks[3] if len(toks) >= 4 else "INFO"
    if status not in ("PASS", "FAIL", "SKIP", "READY", "INFO"):
        status = "INFO"

    fields = _parse_marker_kv_fields(marker_line)
    parts = [f"AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESIZE|{status}"]

    # The guest SKIP marker uses a plain token (e.g. `...|SKIP|flag_not_set`) rather than
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

    ordered = (
        "disk",
        "old_bytes",
        "new_bytes",
        "elapsed_ms",
        "last_bytes",
        "err",
        "reason",
    )
    for k in ordered:
        if k in fields:
            parts.append(f"{k}={_sanitize_marker_value(fields[k])}")

    extra = sorted(k for k in fields if k not in ordered)
    for k in extra:
        parts.append(f"{k}={_sanitize_marker_value(fields[k])}")

    print("|".join(parts))


def _emit_virtio_blk_reset_host_marker(tail: bytes, *, blk_reset_line: Optional[str] = None) -> None:
    """
    Best-effort: emit a host-side marker mirroring the guest's virtio-blk miniport reset selftest.

    Guest markers:
      AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|PASS|performed=1|counter_before=...|counter_after=...
      AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|SKIP|reason=flag_not_set
      AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|SKIP|reason=not_supported
      AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|FAIL|reason=...|err=...

    Host marker:
      AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESET|PASS/FAIL/SKIP|...

    This does not affect harness PASS/FAIL; it's only for log scraping/diagnostics.
    """
    marker_line = blk_reset_line
    if marker_line is None:
        marker_line = _try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|")
    if marker_line is None:
        return

    toks = marker_line.split("|")
    status = toks[3] if len(toks) >= 4 else "INFO"
    if status not in ("PASS", "FAIL", "SKIP", "INFO"):
        status = "INFO"

    fields = _parse_marker_kv_fields(marker_line)
    parts = [f"AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESET|{status}"]

    # Backcompat: mirror legacy markers like `...|SKIP|flag_not_set` / `...|FAIL|post_reset_io_failed` (no `reason=` field)
    # as `reason=...` so log scraping can treat it uniformly.
    if status in ("SKIP", "FAIL") and "reason" not in fields:
        try:
            idx = toks.index(status)
            if idx + 1 < len(toks):
                reason_tok = toks[idx + 1].strip()
                if reason_tok and "=" not in reason_tok:
                    fields["reason"] = reason_tok
        except Exception:
            pass

    ordered = ("performed", "counter_before", "counter_after", "err", "reason")
    for k in ordered:
        if k in fields:
            parts.append(f"{k}={_sanitize_marker_value(fields[k])}")
    extra = sorted(k for k in fields if k not in ordered)
    for k in extra:
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
