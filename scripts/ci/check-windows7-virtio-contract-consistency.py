#!/usr/bin/env python3
"""
Guardrail: prevent drift between the definitive Windows 7 virtio contract
(`docs/windows7-virtio-driver-contract.md`, Contract ID: AERO-W7-VIRTIO) and the
Windows device/driver binding docs/manifest (`docs/windows-device-contract.*`).

Why this exists:
  - PCI IDs are effectively API for Windows INF matching and Guest Tools
    CriticalDeviceDatabase seeding.
  - A mismatch between documents can silently break driver binding.

This check is intentionally lightweight and validates only the stable PCI ID
surface (VEN/DEV + SUBSYS) for boot-critical virtio devices:

- virtio-blk
- virtio-net
- virtio-snd

This keeps the human-readable docs (`windows-device-contract.md`), the machine
manifest (`windows-device-contract.json`), and the authoritative virtio contract
(`windows7-virtio-driver-contract.md`) aligned.
"""

from __future__ import annotations

import json
import re
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]

W7_VIRTIO_CONTRACT_MD = REPO_ROOT / "docs/windows7-virtio-driver-contract.md"
WINDOWS_DEVICE_CONTRACT_MD = REPO_ROOT / "docs/windows-device-contract.md"
WINDOWS_DEVICE_CONTRACT_JSON = REPO_ROOT / "docs/windows-device-contract.json"


def fail(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(1)


def read_text(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8")
    except FileNotFoundError:
        fail(f"missing required file: {path.as_posix()}")


def parse_hex(value: str) -> int:
    try:
        return int(value, 16)
    except ValueError:
        fail(f"expected hex literal, got: {value!r}")


def extract_virtio_from_w7_contract(md: str, *, device_name: str, section_heading: str) -> dict[str, int]:
    # Isolate the per-device section to avoid matching other PCI IDs.
    section_match = re.search(
        rf"^{re.escape(section_heading)}.*?(?=^### 3\.[0-9]+\s|^##\s|\Z)",
        md,
        flags=re.S | re.M,
    )
    if not section_match:
        fail(
            f"could not locate {device_name} section in {W7_VIRTIO_CONTRACT_MD.as_posix()} "
            f"(expected heading '{section_heading}')"
        )
    section = section_match.group(0)

    def field(label: str) -> int:
        m = re.search(rf"^- {re.escape(label)}:\s*`(0x[0-9A-Fa-f]+)`\s*$", section, flags=re.M)
        if not m:
            fail(
                f"could not parse '{label}' for {device_name} in {W7_VIRTIO_CONTRACT_MD.as_posix()} "
                "(expected a bullet like '- Device ID: `0x1059`')"
            )
        return int(m.group(1), 16)

    return {
        "pci_vendor_id": field("Vendor ID"),
        "pci_device_id": field("Device ID"),
        "subsystem_vendor_id": field("Subsystem Vendor ID"),
        "subsystem_device_id": field("Subsystem Device ID"),
        "revision_id": field("Revision ID"),
    }


def extract_virtio_from_windows_device_contract(md: str, *, device_name: str) -> dict[str, int]:
    # Parse the single-row table entry:
    # | virtio-blk | `1AF4:1042` (REV `0x01`) | `1AF4:0002` | ...
    row = re.search(
        rf"^\|\s*{re.escape(device_name)}\s*\|\s*`(?P<pci>[0-9A-Fa-f]{{4}}:[0-9A-Fa-f]{{4}})`[^|]*\|"
        rf"\s*`(?P<subsys>[0-9A-Fa-f]{{4}}:[0-9A-Fa-f]{{4}})`[^|]*\|",
        md,
        flags=re.M,
    )
    if not row:
        fail(
            f"could not parse {device_name} row from {WINDOWS_DEVICE_CONTRACT_MD.as_posix()} "
            "(expected a table row like '| virtio-blk | `1AF4:1042` | `1AF4:0002` |')"
        )

    pci_vendor, pci_device = row.group("pci").split(":")
    subsys_vendor, subsys_device = row.group("subsys").split(":")

    return {
        "pci_vendor_id": parse_hex(pci_vendor),
        "pci_device_id": parse_hex(pci_device),
        "subsystem_vendor_id": parse_hex(subsys_vendor),
        "subsystem_device_id": parse_hex(subsys_device),
    }


def extract_virtio_from_manifest(data: dict, *, device_name: str) -> dict[str, object]:
    devices = data.get("devices")
    if not isinstance(devices, list):
        fail(f"'devices' must be a list in {WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}")

    for device in devices:
        if isinstance(device, dict) and device.get("device") == device_name:
            return device

    fail(f"device entry {device_name!r} not found in {WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}")


def main() -> None:
    w7_md = read_text(W7_VIRTIO_CONTRACT_MD)
    windows_doc_md = read_text(WINDOWS_DEVICE_CONTRACT_MD)

    try:
        manifest = json.loads(read_text(WINDOWS_DEVICE_CONTRACT_JSON))
    except json.JSONDecodeError as e:
        fail(f"invalid JSON in {WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}: {e}")

    for device_name, section_heading in [
        ("virtio-blk", "### 3.1 virtio-blk"),
        ("virtio-net", "### 3.2 virtio-net"),
        ("virtio-snd", "### 3.4 virtio-snd"),
    ]:
        w7_contract = extract_virtio_from_w7_contract(w7_md, device_name=device_name, section_heading=section_heading)
        windows_doc = extract_virtio_from_windows_device_contract(windows_doc_md, device_name=device_name)
        entry = extract_virtio_from_manifest(manifest, device_name=device_name)

        # 1) windows-device-contract.md must match the authoritative W7 contract.
        if (
            windows_doc["pci_vendor_id"] != w7_contract["pci_vendor_id"]
            or windows_doc["pci_device_id"] != w7_contract["pci_device_id"]
        ):
            fail(
                f"{device_name} Vendor/Device ID mismatch between contracts:\n"
                f"  {W7_VIRTIO_CONTRACT_MD.as_posix()}: "
                f"{w7_contract['pci_vendor_id']:04X}:{w7_contract['pci_device_id']:04X}\n"
                f"  {WINDOWS_DEVICE_CONTRACT_MD.as_posix()}: "
                f"{windows_doc['pci_vendor_id']:04X}:{windows_doc['pci_device_id']:04X}"
            )

        if (
            windows_doc["subsystem_vendor_id"] != w7_contract["subsystem_vendor_id"]
            or windows_doc["subsystem_device_id"] != w7_contract["subsystem_device_id"]
        ):
            fail(
                f"{device_name} Subsystem Vendor/Device ID mismatch between contracts:\n"
                f"  {W7_VIRTIO_CONTRACT_MD.as_posix()}: "
                f"{w7_contract['subsystem_vendor_id']:04X}:{w7_contract['subsystem_device_id']:04X}\n"
                f"  {WINDOWS_DEVICE_CONTRACT_MD.as_posix()}: "
                f"{windows_doc['subsystem_vendor_id']:04X}:{windows_doc['subsystem_device_id']:04X}"
            )

        # 2) windows-device-contract.json must match Vendor/Device ID.
        try:
            manifest_vendor = int(str(entry.get("pci_vendor_id", "")), 16)
            manifest_device = int(str(entry.get("pci_device_id", "")), 16)
        except ValueError:
            fail(
                f"{device_name} entry in {WINDOWS_DEVICE_CONTRACT_JSON.as_posix()} has invalid "
                "'pci_vendor_id' or 'pci_device_id' (expected hex strings like '0x1AF4')"
            )

        if manifest_vendor != w7_contract["pci_vendor_id"] or manifest_device != w7_contract["pci_device_id"]:
            fail(
                f"{device_name} Vendor/Device ID mismatch between AERO-W7-VIRTIO and windows-device-contract.json:\n"
                f"  {W7_VIRTIO_CONTRACT_MD.as_posix()}: "
                f"{w7_contract['pci_vendor_id']:04X}:{w7_contract['pci_device_id']:04X}\n"
                f"  {WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}: "
                f"{manifest_vendor:04X}:{manifest_device:04X}"
            )

        # 3) Manifest must include a SUBSYS pattern matching the authoritative contract.
        expected_subsys = f"SUBSYS_{w7_contract['subsystem_device_id']:04X}{w7_contract['subsystem_vendor_id']:04X}"
        patterns = entry.get("hardware_id_patterns")
        if not isinstance(patterns, list) or not all(isinstance(p, str) for p in patterns):
            fail(
                f"{device_name} 'hardware_id_patterns' must be a string list in {WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}"
            )

        patterns_upper = [p.upper() for p in patterns]
        if not any(expected_subsys in p for p in patterns_upper):
            fail(
                f"{device_name} manifest is missing the expected subsystem-qualified hardware ID:\n"
                f"  expected: {expected_subsys}\n"
                f"  got: {patterns}"
            )


if __name__ == "__main__":
    main()
