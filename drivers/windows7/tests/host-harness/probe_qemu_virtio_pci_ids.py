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
import subprocess
import tempfile
from dataclasses import dataclass
from pathlib import Path


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

            def _as_int(v: object) -> int | None:
                if isinstance(v, int):
                    return v
                if isinstance(v, str):
                    try:
                        return int(v, 0)
                    except ValueError:
                        return None
                return None

            vendor = _as_int(dev.get("vendor_id"))
            device = _as_int(dev.get("device_id"))
            if vendor is None or device is None:
                continue

            subsys_vendor = _as_int(dev.get("subsystem_vendor_id"))
            subsys = _as_int(dev.get("subsystem_id"))
            rev = _as_int(dev.get("revision"))
            devices.append(_PciId(vendor, device, subsys_vendor, subsys, rev))

    return devices


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--qemu-system", default="qemu-system-x86_64", help="Path to qemu-system-* binary")
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

    rev_arg = ""
    if args.mode == "contract-v1":
        rev_arg = ",x-pci-revision=0x01"

    with tempfile.TemporaryDirectory(prefix="aero-qemu-pci-probe-") as td:
        disk_path = Path(td) / "disk.img"
        # Small placeholder disk (only used for device instantiation).
        disk_path.write_bytes(b"\x00" * 1024 * 1024)

        qemu_args = [
            args.qemu_system,
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

        proc = subprocess.Popen(
            qemu_args,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
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
            filtered = [d for d in devices if (d.vendor_id, d.device_id) in want]

            print(f"QEMU: {args.qemu_system}")
            print(f"Mode: {args.mode}")
            print("")
            print("Detected virtio devices (vendor/device/subsys/rev):")
            for d in sorted(filtered, key=lambda x: (x.vendor_id, x.device_id, x.subsystem_id or -1)):
                print(
                    f"  {_fmt_hex(4, d.vendor_id)}:{_fmt_hex(4, d.device_id)}"
                    f"  subsys={_fmt_hex(4, d.subsystem_id)}:{_fmt_hex(4, d.subsystem_vendor_id)}"
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
