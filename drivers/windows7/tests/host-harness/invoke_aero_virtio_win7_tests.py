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
so the canonical Win7 INF (`aero-virtio-snd.inf`) can bind to `PCI\\VEN_1AF4&DEV_1059&REV_01` under QEMU.

Use `--virtio-transitional` to opt back into QEMU's default transitional devices (legacy + modern)
for older QEMU builds (or when intentionally testing legacy driver packages).
In transitional mode the harness uses QEMU defaults for virtio-blk/virtio-net (and relaxes per-test marker
requirements so older guest selftest binaries can still be used).
It also attempts to attach virtio-input keyboard/mouse devices when QEMU advertises them; otherwise it
warns that the guest virtio-input selftest will likely fail.

It:
- starts a tiny HTTP server on 127.0.0.1:<port> (guest reaches it as 10.0.2.2:<port> via slirp)
  - use `--http-log <path>` to record per-request logs (useful for CI artifacts)
- launches QEMU with virtio-blk + virtio-net + virtio-input (and optionally virtio-snd) and COM1 redirected to a log file
  - in transitional mode virtio-input is skipped (with a warning) if QEMU does not advertise virtio-keyboard-pci/virtio-mouse-pci
- captures QEMU stderr to `<serial-base>.qemu.stderr.log` (next to the serial log) for debugging early exits
- tails the serial log until it sees AERO_VIRTIO_SELFTEST|RESULT|PASS/FAIL
  - in default (non-transitional) mode, a PASS result also requires per-test markers for virtio-blk, virtio-input,
    virtio-snd (PASS or SKIP), virtio-snd-capture (PASS or SKIP), and virtio-net so older selftest binaries cannot accidentally pass
  - when --with-virtio-snd is enabled, virtio-snd and virtio-snd-capture must PASS (not SKIP)
"""

from __future__ import annotations

import argparse
import http.server
import math
import os
import shlex
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


class _QuietHandler(http.server.BaseHTTPRequestHandler):
    expected_path: str = "/aero-virtio-selftest"
    http_log_path: Optional[Path] = None

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


class _ReusableTcpServer(socketserver.TCPServer):
    allow_reuse_address = True


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


def _virtio_snd_skip_failure_message(tail: bytes) -> str:
    # The guest selftest's virtio-snd marker is intentionally strict and machine-friendly:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-snd|PASS/FAIL/SKIP
    #
    # Any reason for SKIP is logged as human-readable text, so the host harness must infer
    # a useful error message from the tail log.
    if b"virtio-snd: skipped (enable with --test-snd)" in tail:
        return (
            "FAIL: virtio-snd test was skipped (guest not configured with --test-snd) "
            "but --with-virtio-snd was enabled"
        )
    if b"virtio-snd: disabled by --disable-snd" in tail:
        return "FAIL: virtio-snd test was skipped (--disable-snd) but --with-virtio-snd was enabled"
    if b"virtio-snd:" in tail and b"device not detected" in tail:
        return "FAIL: virtio-snd test was skipped (device missing) but --with-virtio-snd was enabled"
    return "FAIL: virtio-snd test was skipped but --with-virtio-snd was enabled"


def _virtio_snd_capture_skip_failure_message(tail: bytes) -> str:
    # The capture marker is separate from the playback marker:
    #   AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|PASS/FAIL/SKIP|...
    prefix = b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|"
    if b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|endpoint_missing" in tail:
        return "FAIL: virtio-snd capture endpoint missing but --with-virtio-snd was enabled"
    if b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|flag_not_set" in tail:
        return (
            "FAIL: virtio-snd capture test was skipped (flag_not_set) but --with-virtio-snd was enabled "
            "(ensure the guest is configured with --test-snd/--require-snd or capture flags)"
        )
    if b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|disabled" in tail:
        return (
            "FAIL: virtio-snd capture test was skipped (disabled via --disable-snd or --disable-snd-capture) "
            "but --with-virtio-snd was enabled"
        )
    if b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|device_missing" in tail:
        return "FAIL: virtio-snd capture test was skipped (device missing) but --with-virtio-snd was enabled"

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
                f"FAIL: virtio-snd capture test was skipped ({reason_str}) "
                "but --with-virtio-snd was enabled"
            )
    return "FAIL: virtio-snd capture test was skipped but --with-virtio-snd was enabled"


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
    parser.add_argument("--http-port", type=int, default=18080)
    parser.add_argument("--http-path", default="/aero-virtio-selftest")
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
        "--with-virtio-snd",
        "--enable-virtio-snd",
        dest="enable_virtio_snd",
        action="store_true",
        help=(
            "Attach a virtio-snd device (virtio-sound-pci). When enabled, the harness requires the guest virtio-snd "
            "selftests (playback + capture) to PASS (not SKIP)."
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
    parser.add_argument(
        "--follow-serial",
        action="store_true",
        help="Stream newly captured COM1 serial output to stdout while waiting",
    )

    # Any remaining args are passed directly to QEMU.
    args, qemu_extra = parser.parse_known_args()

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

        wav_path: Optional[Path] = None
        if args.virtio_transitional:
            drive = f"file={disk_image},if=virtio,cache=writeback"
            if args.snapshot:
                drive += ",snapshot=on"

            virtio_input_args: list[str] = []
            if _qemu_has_device(args.qemu_system, "virtio-keyboard-pci") and _qemu_has_device(
                args.qemu_system, "virtio-mouse-pci"
            ):
                virtio_input_args = [
                    "-device",
                    "virtio-keyboard-pci",
                    "-device",
                    "virtio-mouse-pci",
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
                    # Quote the path inside the audiodev keyval string so QEMU builds on Windows
                    # (or any environment where the output path contains spaces) do not misparse it.
                    escaped = str(wav_path).replace('"', '\\"')
                    audiodev_arg = f'wav,id=snd0,path="{escaped}"'
                else:
                    raise AssertionError(f"Unhandled backend: {backend}")

                virtio_snd_args = ["-audiodev", audiodev_arg, "-device", device_arg]

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
            drive = f"file={disk_image},if=none,id={drive_id},cache=writeback"
            if args.snapshot:
                drive += ",snapshot=on"

            virtio_net = f"virtio-net-pci,netdev=net0,disable-legacy=on,x-pci-revision={aero_pci_rev}"
            virtio_blk = f"virtio-blk-pci,drive={drive_id},disable-legacy=on,x-pci-revision={aero_pci_rev}"
            virtio_kbd = f"virtio-keyboard-pci,disable-legacy=on,x-pci-revision={aero_pci_rev}"
            virtio_mouse = f"virtio-mouse-pci,disable-legacy=on,x-pci-revision={aero_pci_rev}"

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
                    escaped = str(wav_path).replace('"', '\\"')
                    audiodev_arg = f'wav,id=snd0,path="{escaped}"'
                else:
                    raise AssertionError(f"Unhandled backend: {backend}")

                virtio_snd_args = ["-audiodev", audiodev_arg, "-device", device_arg]

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
            saw_virtio_snd_pass = False
            saw_virtio_snd_skip = False
            saw_virtio_snd_fail = False
            saw_virtio_snd_capture_pass = False
            saw_virtio_snd_capture_skip = False
            saw_virtio_snd_capture_fail = False
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
                    if not saw_virtio_input_fail and b"AERO_VIRTIO_SELFTEST|TEST|virtio-input|FAIL" in tail:
                        saw_virtio_input_fail = True
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
                                    "FAIL: selftest RESULT=PASS but did not emit virtio-blk test marker",
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
                                    "FAIL: selftest RESULT=PASS but did not emit virtio-input test marker",
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
                            else:
                                # Even when virtio-snd isn't attached, require the markers so older selftest binaries
                                # (that predate virtio-snd playback/capture coverage) cannot accidentally pass.
                                if not (saw_virtio_snd_pass or saw_virtio_snd_skip):
                                    print(
                                        "FAIL: selftest RESULT=PASS but did not emit virtio-snd test marker",
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
                                        "FAIL: selftest RESULT=PASS but did not emit virtio-snd-capture test marker",
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
                                    "FAIL: selftest RESULT=PASS but did not emit virtio-net test marker",
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
                        print("PASS: AERO_VIRTIO_SELFTEST|RESULT|PASS")
                        result_code = 0
                        break
                    if b"AERO_VIRTIO_SELFTEST|RESULT|FAIL" in tail:
                        print("FAIL: AERO_VIRTIO_SELFTEST|RESULT|FAIL")
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
                                        "FAIL: selftest RESULT=PASS but did not emit virtio-blk test marker",
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
                                        "FAIL: selftest RESULT=PASS but did not emit virtio-input test marker",
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
                                else:
                                    if not (saw_virtio_snd_pass or saw_virtio_snd_skip):
                                        print(
                                            "FAIL: selftest RESULT=PASS but did not emit virtio-snd test marker",
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
                                            "FAIL: selftest RESULT=PASS but did not emit virtio-snd-capture test marker",
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
                                        "FAIL: selftest RESULT=PASS but did not emit virtio-net test marker",
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
            _stop_process(proc)
            httpd.shutdown()
            try:
                stderr_f.close()
            except Exception:
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
    PCI Revision ID 0x01 (`REV_01`). The canonical Win7 INF (`aero-virtio-snd.inf`) is intentionally
    strict and matches only `PCI\\VEN_1AF4&DEV_1059&REV_01`, so we force `disable-legacy=on,x-pci-revision=0x01`.
    """
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

    return f"{device_name},audiodev=snd0,disable-legacy=on,x-pci-revision=0x01"


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
    if kind == "pcm" and bits_per_sample == 16:
        return _compute_pcm16_metrics(path, data_offset, data_size)

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
