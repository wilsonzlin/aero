#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""
Probe QEMU-emitted PCI identity fields for the virtio devices used by the Win7 host harness.

This tool is intended for contributors who want to confirm what their local QEMU build is
advertising (Vendor/Device/Subsys/Revision) for modern-only virtio-pci devices.

Why this exists
---------------
The Aero Windows 7 virtio device contract encodes the contract major version in the PCI
Revision ID (contract v1 = 0x01). Many QEMU virtio devices report REV_00 by default, so the
Win7 host harness forces REV_01 via `x-pci-revision=0x01`.

This script lets you verify:
  - what QEMU reports by default (with `disable-legacy=on` only), and
  - what QEMU reports when applying the contract-v1 overrides (REV_01).

It uses QMP (`query-pci`) for structured parsing (no guest OS required).
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Final


@dataclass(frozen=True)
class _PciId:
    vendor_id: int
    device_id: int
    subsystem_vendor_id: int | None
    subsystem_id: int | None
    revision: int | None


def _fmt_hex(width: int, value: int | None) -> str:
    if value is None:
        return "?"
    return f"0x{value:0{width}x}"


def _qmp_maybe_int(v: object) -> int | None:
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


def _qmp_device_vendor_device_id(dev: dict[str, object]) -> tuple[int | None, int | None]:
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


def _iter_query_pci_buses(query_pci_result: object) -> list[dict[str, object]]:
    """
    Flatten QMP `query-pci` output into a list of bus objects.

    QEMU represents subordinate buses behind bridge devices via:
      bus.devices[*].pci_bridge.bus.devices[*]...

    Some versions may also return a flat list. This helper supports both and deduplicates by bus
    number when present.
    """
    buses: list[dict[str, object]] = []
    if not isinstance(query_pci_result, list):
        return buses

    stack: list[dict[str, object]] = [b for b in query_pci_result if isinstance(b, dict)]
    seen_nums: set[int] = set()

    while stack:
        bus_obj = stack.pop()
        bus_num = _qmp_maybe_int(bus_obj.get("bus"))
        if bus_num is None:
            bus_num = _qmp_maybe_int(bus_obj.get("number"))
        if bus_num is not None:
            if bus_num in seen_nums:
                continue
            seen_nums.add(bus_num)

        buses.append(bus_obj)

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


def _qemu_device_help_text(qemu_system: str) -> str | None:
    try:
        proc = subprocess.run(
            [qemu_system, "-device", "help"],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            check=False,
            text=True,
        )
    except FileNotFoundError:
        return None
    except OSError:
        return None
    return proc.stdout or ""


def _qemu_detect_optional_devices_from_help_text(help_text: str) -> tuple[bool, str | None]:
    """
    Detect optional device support from `qemu-system-* -device help` output.

    Returns:
        (has_virtio_tablet, virtio_snd_device_name)
    """

    has_tablet = "virtio-tablet-pci" in help_text

    # QEMU device naming has changed over time. Prefer the modern upstream name but fall back
    # to distro aliases when present.
    snd_device: str | None = None
    if "virtio-sound-pci" in help_text:
        snd_device = "virtio-sound-pci"
    elif "virtio-snd-pci" in help_text:
        snd_device = "virtio-snd-pci"

    return has_tablet, snd_device


def _build_qemu_args(
    *,
    qemu_system: str,
    disk_path: Path,
    mode: str,
    with_virtio_snd: bool,
    with_virtio_tablet: bool,
    device_help_text: str,
) -> list[str]:
    """
    Construct the qemu-system command line used for probing.

    virtio-tablet-pci is opt-in (via `--with-virtio-tablet`) and only attached when QEMU
    advertises the device in `-device help`. This keeps the probe compatible with older
    QEMU builds and avoids attaching the tablet twice.
    """

    rev_arg = ""
    if mode == "contract-v1":
        rev_arg = ",x-pci-revision=0x01"

    has_tablet, snd_device_name = _qemu_detect_optional_devices_from_help_text(device_help_text)

    qemu_args: list[str] = [
        qemu_system,
        "-nodefaults",
        "-machine",
        "q35",
        "-m",
        "128",
        "-display",
        "none",
        "-no-reboot",
        "-qmp",
        "stdio",
        "-netdev",
        "user,id=net0",
        "-device",
        f"virtio-net-pci,netdev=net0,disable-legacy=on{rev_arg}",
        "-drive",
        f"file={disk_path},if=none,format=raw,id=drive0",
        "-device",
        f"virtio-blk-pci,drive=drive0,disable-legacy=on{rev_arg}",
        "-device",
        f"virtio-keyboard-pci,disable-legacy=on{rev_arg}",
        "-device",
        f"virtio-mouse-pci,disable-legacy=on{rev_arg}",
    ]

    if with_virtio_tablet and has_tablet:
        qemu_args += [
            "-device",
            f"virtio-tablet-pci,disable-legacy=on{rev_arg}",
        ]

    if with_virtio_snd:
        if not snd_device_name:
            raise SystemExit(
                "ERROR: QEMU does not advertise a virtio-snd PCI device (expected virtio-sound-pci or virtio-snd-pci)."
            )
        qemu_args += [
            "-audiodev",
            "none,id=snd0",
            "-device",
            f"{snd_device_name},audiodev=snd0,disable-legacy=on{rev_arg}",
        ]

    return qemu_args


def _read_qmp_obj(stdout: "subprocess._TextIOWrapper") -> dict:
    while True:
        line = stdout.readline()
        if line == "":
            raise RuntimeError("QMP EOF while waiting for response")
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            # Some QEMU builds may emit non-JSON banner text on stdio; ignore it.
            continue
        # Ignore async events.
        if "event" in obj and "id" not in obj:
            continue
        return obj


def _qmp_exec(proc: subprocess.Popen[str], execute: str, arguments: dict | None, req_id: int) -> dict:
    msg: dict = {"execute": execute, "id": req_id}
    if arguments:
        msg["arguments"] = arguments
    proc.stdin.write(json.dumps(msg) + "\n")
    proc.stdin.flush()

    while True:
        obj = _read_qmp_obj(proc.stdout)
        if obj.get("id") != req_id:
            continue
        if "error" in obj:
            raise RuntimeError(f"QMP error for {execute}: {obj['error']}")
        return obj


def _iter_pci_devices(query_pci_result: object) -> list[_PciId]:
    """
    Attempt to extract vendor/device/subsystem/revision from QMP query-pci output.

    QEMU's QMP schema is stable but can vary slightly between versions; we treat unknown/missing
    fields as optional.
    """
    devices: list[_PciId] = []

    for bus in _iter_query_pci_buses(query_pci_result):
        bus_devices = bus.get("devices")
        if not isinstance(bus_devices, list):
            continue
        for dev in bus_devices:
            if not isinstance(dev, dict):
                continue

            id_obj = dev.get("id")
            id_dict = id_obj if isinstance(id_obj, dict) else None

            vendor, device = _qmp_device_vendor_device_id(dev)
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


def main() -> int:
    # Disable long-option abbreviation matching for stability as the CLI grows. This tool is often
    # invoked from scripts/CI and we want unknown args/typos to fail loudly instead of being silently
    # consumed as abbreviated options.
    parser = argparse.ArgumentParser(allow_abbrev=False)
    parser.add_argument("--qemu-system", default="qemu-system-x86_64", help="Path to qemu-system-* binary")
    parser.add_argument(
        "--with-virtio-snd",
        action="store_true",
        help=(
            "Also attach a virtio-snd PCI device (requires QEMU virtio-sound-pci/virtio-snd-pci + -audiodev support). "
            "This matches the host harness behavior when virtio-snd is enabled."
        ),
    )
    parser.add_argument(
        "--with-virtio-tablet",
        action="store_true",
        help=(
            "Also attach a virtio-tablet-pci device (absolute pointer). If your QEMU build does not support "
            "virtio-tablet-pci, the probe runs unchanged without it."
        ),
    )
    parser.add_argument(
        "--mode",
        choices=["default", "contract-v1"],
        default="default",
        help=(
            "default: use disable-legacy=on only (shows what QEMU advertises by default). "
            "contract-v1: additionally forces x-pci-revision=0x01."
        ),
    )
    parser.add_argument(
        "--dump-query-pci",
        action="store_true",
        help="Print the raw QMP query-pci JSON (useful if your QEMU version has a different schema).",
    )
    parser.add_argument(
        "--dump-info-pci",
        action="store_true",
        help="Print the HMP 'info pci' output via QMP (useful for manual inspection).",
    )
    args = parser.parse_args()

    if not args.qemu_system or not str(args.qemu_system).strip():
        print("ERROR: --qemu-system must be non-empty", file=sys.stderr)
        return 2

    # Keep behaviour consistent with the main Win7 host harness: when a qemu-system path is supplied,
    # fail fast if it points to a directory (common copy/paste mistake, produces confusing subprocess errors).
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
            if not qemu_path.exists():
                print(f"ERROR: qemu-system binary not found: {qemu_path}", file=sys.stderr)
                return 2
        except Exception:
            pass

    device_help_text: Final[str] = ""
    if args.with_virtio_snd or args.with_virtio_tablet:
        help_text = _qemu_device_help_text(args.qemu_system)
        if help_text is None:
            print(f"ERROR: qemu-system binary not found: {args.qemu_system}", file=sys.stderr)
            return 2
        device_help_text = help_text or ""

    with tempfile.TemporaryDirectory(prefix="aero-qemu-pci-probe-") as td:
        disk_path = Path(td) / "disk.img"
        # Small placeholder disk (only used for device instantiation).
        disk_path.write_bytes(b"\x00" * 1024 * 1024)

        qemu_args = _build_qemu_args(
            qemu_system=args.qemu_system,
            disk_path=disk_path,
            mode=args.mode,
            with_virtio_snd=args.with_virtio_snd,
            with_virtio_tablet=args.with_virtio_tablet,
            device_help_text=device_help_text,
        )

        try:
            proc = subprocess.Popen(
                qemu_args,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.PIPE,
                text=True,
            )
        except FileNotFoundError:
            print(f"ERROR: qemu-system binary not found: {args.qemu_system}", file=sys.stderr)
            return 2
        except OSError as e:
            print(f"ERROR: failed to start qemu-system binary: {args.qemu_system}: {e}", file=sys.stderr)
            return 2
        try:
            greeting = _read_qmp_obj(proc.stdout)
            if "QMP" not in greeting:
                raise RuntimeError(f"Unexpected QMP greeting: {greeting}")

            _qmp_exec(proc, "qmp_capabilities", None, 1)
            res = _qmp_exec(proc, "query-pci", None, 2)
            query = res.get("return")
            if args.dump_query_pci:
                print(json.dumps(query, indent=2, sort_keys=True))
                print("")
            if args.dump_info_pci:
                info = _qmp_exec(proc, "human-monitor-command", {"command-line": "info pci"}, 3).get("return", "")
                print("--- HMP: info pci ---")
                print(info.rstrip())
                print("")
            devices = _iter_pci_devices(query)

            want = {(0x1AF4, 0x1041), (0x1AF4, 0x1042), (0x1AF4, 0x1052)}
            if args.with_virtio_snd:
                want.add((0x1AF4, 0x1059))
            filtered = [d for d in devices if (d.vendor_id, d.device_id) in want]

            print(f"QEMU: {args.qemu_system}")
            print(f"Mode: {args.mode}")
            print("")
            print("Detected virtio devices (vendor/device/subsys/rev):")
            for d in sorted(filtered, key=lambda x: (x.vendor_id, x.device_id, x.subsystem_id or -1)):
                print(
                    f"  {_fmt_hex(4, d.vendor_id)}:{_fmt_hex(4, d.device_id)}"
                    f"  subsys={_fmt_hex(4, d.subsystem_vendor_id)}:{_fmt_hex(4, d.subsystem_id)}"
                    f"  rev={_fmt_hex(2, d.revision)}"
                )
            if not filtered:
                print("  (no matching devices found in query-pci output)")
                err = (proc.stderr.read() or "").strip()
                if err:
                    print("")
                    print("--- QEMU stderr ---")
                    print(err)

            return 0
        finally:
            try:
                _qmp_exec(proc, "quit", None, 99)
            except Exception:
                pass
            try:
                proc.wait(timeout=2)
            except Exception:
                try:
                    proc.kill()
                except Exception:
                    pass


if __name__ == "__main__":
    raise SystemExit(main())
