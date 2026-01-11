#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""
Win7 virtio functional tests host harness (QEMU).

This is a Python alternative to Invoke-AeroVirtioWin7Tests.ps1 for environments where
PowerShell is inconvenient (e.g. Linux CI).

Note: This harness intentionally uses *modern-only* virtio-pci devices (`disable-legacy=on`) for
virtio-blk/virtio-net/virtio-input so the Win7 drivers bind to the Aero contract v1 IDs
(DEV_1041/DEV_1042/DEV_1052).

The current Aero Win7 virtio-snd driver build uses the **legacy** virtio-pci I/O-port transport.
When `--with-virtio-snd` is enabled, the harness keeps legacy mode enabled for virtio-snd (it does
not set `disable-legacy=on`), so virtio-snd may enumerate as the transitional ID `DEV_1018`.

Use `--virtio-transitional` to opt back into QEMU's default transitional devices (legacy + modern)
for older QEMU builds (or when intentionally testing legacy driver packages).

It:
- starts a tiny HTTP server on 127.0.0.1:<port> (guest reaches it as 10.0.2.2:<port> via slirp)
- launches QEMU with virtio-blk + virtio-net + virtio-input (and optionally virtio-snd) and COM1 redirected to a log file
- tails the serial log until it sees AERO_VIRTIO_SELFTEST|RESULT|PASS/FAIL
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


@dataclass(frozen=True)
class _WaveFmtChunk:
    format_tag: int
    channels: int
    sample_rate: int
    block_align: int
    bits_per_sample: int


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


def _assert_qemu_supports_aero_w7_virtio_contract_v1(qemu_system: str) -> None:
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
    parser.add_argument("--snapshot", action="store_true", help="Discard disk writes (snapshot=on)")
    parser.add_argument(
        "--virtio-transitional",
        action="store_true",
        help="Use transitional virtio-pci devices (legacy + modern). "
        "By default the harness uses modern-only virtio-pci (disable-legacy=on, x-pci-revision=0x01) "
        "so Win7 drivers can bind to the Aero contract v1 IDs.",
    )
    parser.add_argument(
        "--with-virtio-snd",
        "--enable-virtio-snd",
        dest="enable_virtio_snd",
        action="store_true",
        help=(
            "Attach a virtio-snd device (virtio-sound-pci). Required when the guest selftest runs "
            "virtio-snd playback (guest must be configured with --test-snd/--require-snd)."
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
            "After QEMU exits, verify the captured wav file contains non-silent 16-bit PCM "
            "(requires --enable-virtio-snd and --virtio-snd-audio-backend=wav)"
        ),
    )
    parser.add_argument(
        "--virtio-snd-wav-peak-threshold",
        type=int,
        default=200,
        help="Peak absolute sample threshold for --virtio-snd-verify-wav (default: 200)",
    )
    parser.add_argument(
        "--virtio-snd-wav-rms-threshold",
        type=int,
        default=50,
        help="RMS threshold for --virtio-snd-verify-wav (default: 50)",
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

    if not args.virtio_transitional:
        try:
            _assert_qemu_supports_aero_w7_virtio_contract_v1(args.qemu_system)
        except RuntimeError as e:
            print(f"ERROR: {e}", file=sys.stderr)
            return 2

    disk_image = Path(args.disk_image).resolve()
    serial_log = Path(args.serial_log).resolve()
    serial_log.parent.mkdir(parents=True, exist_ok=True)

    if serial_log.exists():
        serial_log.unlink()

    qemu_stderr_log = serial_log.with_name(serial_log.stem + ".qemu.stderr.log")
    try:
        qemu_stderr_log.unlink()
    except FileNotFoundError:
        pass

    handler = type("_Handler", (_QuietHandler,), {"expected_path": args.http_path})

    with _ReusableTcpServer(("127.0.0.1", args.http_port), handler) as httpd:
        thread = Thread(target=httpd.serve_forever, daemon=True)
        thread.start()

        wav_path: Optional[Path] = None
        if args.virtio_transitional:
            drive = f"file={disk_image},if=virtio,cache=writeback"
            if args.snapshot:
                drive += ",snapshot=on"

            virtio_snd_args: list[str] = []
            if args.enable_virtio_snd:
                try:
                    device_arg = _get_qemu_virtio_sound_device_arg(args.qemu_system)
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
                    audiodev_arg = f"wav,id=snd0,path={wav_path}"
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
                    device_arg = _get_qemu_virtio_sound_device_arg(args.qemu_system)
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
                    audiodev_arg = f"wav,id=snd0,path={wav_path}"
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
            saw_virtio_input_pass = False
            saw_virtio_input_fail = False
            saw_virtio_snd_pass = False
            saw_virtio_snd_skip = False
            saw_virtio_snd_fail = False
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

                    if b"AERO_VIRTIO_SELFTEST|RESULT|PASS" in tail:
                        if require_per_test_markers:
                            # Require per-test markers so older selftest binaries cannot
                            # accidentally pass the host harness.
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
                                        if b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP|flag_not_set" in tail:
                                            msg = (
                                                "FAIL: virtio-snd test was skipped (guest not configured with --test-snd) "
                                                "but --with-virtio-snd was enabled"
                                            )
                                        elif b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP|disabled" in tail:
                                            msg = (
                                                "FAIL: virtio-snd test was skipped (--disable-snd) "
                                                "but --with-virtio-snd was enabled"
                                            )
                                        else:
                                            msg = "FAIL: virtio-snd test was skipped but --with-virtio-snd was enabled"
                                    print(msg, file=sys.stderr)
                                    _print_tail(serial_log)
                                    result_code = 1
                                    break
                            else:
                                # Even when virtio-snd isn't attached, require the marker so older selftest binaries
                                # (that predate virtio-snd testing) cannot accidentally pass.
                                if not (saw_virtio_snd_pass or saw_virtio_snd_skip):
                                    print(
                                        "FAIL: selftest RESULT=PASS but did not emit virtio-snd test marker",
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
                                    if b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP|flag_not_set" in tail:
                                        msg = (
                                            "FAIL: virtio-snd test was skipped (guest not configured with --test-snd) "
                                            "but --with-virtio-snd was enabled"
                                        )
                                    elif b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP|disabled" in tail:
                                        msg = "FAIL: virtio-snd test was skipped (--disable-snd) but --with-virtio-snd was enabled"
                                    else:
                                        msg = "FAIL: virtio-snd test was skipped but --with-virtio-snd was enabled"
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
                        if b"AERO_VIRTIO_SELFTEST|RESULT|PASS" in tail:
                            if require_per_test_markers:
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
                                            if b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP|flag_not_set" in tail:
                                                msg = (
                                                    "FAIL: virtio-snd test was skipped (guest not configured with --test-snd) "
                                                    "but --with-virtio-snd was enabled"
                                                )
                                            elif b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP|disabled" in tail:
                                                msg = (
                                                    "FAIL: virtio-snd test was skipped (--disable-snd) "
                                                    "but --with-virtio-snd was enabled"
                                                )
                                            else:
                                                msg = (
                                                    "FAIL: virtio-snd test was skipped but --with-virtio-snd was enabled"
                                                )
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
                                        if b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP|flag_not_set" in tail:
                                            msg = (
                                                "FAIL: virtio-snd test was skipped (guest not configured with --test-snd) "
                                                "but --with-virtio-snd was enabled"
                                            )
                                        elif b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP|disabled" in tail:
                                            msg = (
                                                "FAIL: virtio-snd test was skipped (--disable-snd) "
                                                "but --with-virtio-snd was enabled"
                                            )
                                        else:
                                            msg = "FAIL: virtio-snd test was skipped but --with-virtio-snd was enabled"
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


def _get_qemu_virtio_sound_device_arg(qemu_system: str) -> str:
    """
    Return the virtio-snd PCI device arg string.

    The current Aero Win7 virtio-snd driver uses the legacy virtio-pci I/O-port transport, so we
    keep legacy mode enabled (do not set disable-legacy=on).
    """
    device_name = _detect_virtio_snd_device(qemu_system)
    proc = subprocess.run(
        [qemu_system, "-device", f"{device_name},help"],
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        out = (proc.stdout or "").strip()
        raise RuntimeError(
            f"virtio-snd device '{device_name}' is not supported by this QEMU binary ({qemu_system}). Output:\n{out}"
        )

    out = proc.stdout or ""
    device = f"{device_name},audiodev=snd0"
    if "disable-legacy" in out:
        device += ",disable-legacy=off"
    if "x-pci-revision" in out:
        device += ",x-pci-revision=0x01"
    return device


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
                fmt = _WaveFmtChunk(
                    format_tag=w_format_tag,
                    channels=n_channels,
                    sample_rate=n_samples_per_sec,
                    block_align=n_block_align,
                    bits_per_sample=w_bits_per_sample,
                )
                if chunk_size > 16:
                    f.seek(chunk_size - 16, os.SEEK_CUR)
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

        if fmt.format_tag != 1:
            print(f"{marker_prefix}|FAIL|reason=unsupported_format_tag_{fmt.format_tag}")
            return False
        if fmt.bits_per_sample != 16:
            print(f"{marker_prefix}|FAIL|reason=unsupported_bits_per_sample_{fmt.bits_per_sample}")
            return False
        if info.data_size <= 0:
            print(f"{marker_prefix}|FAIL|reason=missing_or_empty_data_chunk")
            return False

        peak, rms, sample_values = _compute_pcm16_metrics(path, info.data_offset, info.data_size)
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
