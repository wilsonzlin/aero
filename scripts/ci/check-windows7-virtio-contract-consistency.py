#!/usr/bin/env python3
"""
Guardrail: prevent drift between the definitive Windows 7 virtio contract
(`docs/windows7-virtio-driver-contract.md`, Contract ID: AERO-W7-VIRTIO) and the
Windows device/driver binding docs/manifest (`docs/windows-device-contract.*`).

Why this exists:
  - PCI IDs are effectively API for Windows INF matching and Guest Tools
    CriticalDeviceDatabase seeding.
  - A mismatch between documents can silently break driver binding.

This check intentionally focuses on **PCI identity** since these values are
effectively API for Windows driver binding. It validates **all Aero virtio
devices** present in AERO-W7-VIRTIO v1:

- virtio-blk
- virtio-net
- virtio-input (keyboard + mouse instances)
- virtio-snd

Checked fields:
  - Vendor ID (must be virtio: 0x1AF4)
  - Modern PCI Device ID (must follow 0x1040 + virtio device id)
  - Subsystem Vendor ID (must be 0x1AF4)
  - Subsystem Device ID (Aero contract-defined, including virtio-input variants)
  - PCI Revision ID must be present/consistent across docs and manifest patterns
"""

from __future__ import annotations

from dataclasses import dataclass
import json
import re
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]

W7_VIRTIO_CONTRACT_MD = REPO_ROOT / "docs/windows7-virtio-driver-contract.md"
WINDOWS_DEVICE_CONTRACT_MD = REPO_ROOT / "docs/windows-device-contract.md"
WINDOWS_DEVICE_CONTRACT_JSON = REPO_ROOT / "docs/windows-device-contract.json"

VIRTIO_PCI_VENDOR_ID = 0x1AF4
VIRTIO_PCI_MODERN_DEVICE_ID_BASE = 0x1040
VIRTIO_PCI_TRANSITIONAL_DEVICE_ID_MIN = 0x1000
VIRTIO_PCI_TRANSITIONAL_DEVICE_ID_MAX = 0x103F

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

def parse_hex_str(value: object, *, context: str) -> int:
    if not isinstance(value, str):
        fail(f"{context}: expected hex string, got {type(value).__name__}")
    try:
        return int(value, 16)
    except ValueError:
        fail(f"{context}: expected hex string like '0x1AF4', got: {value!r}")

def extract_section(md: str, *, path: Path, heading_re: str, until_re: str) -> str:
    """
    Extract a markdown section starting at `heading_re` and stopping before the
    next match of `until_re`.
    """
    m = re.search(rf"{heading_re}.*?(?={until_re}|\Z)", md, flags=re.S | re.M)
    if not m:
        fail(f"could not locate expected section {heading_re!r} in {path.as_posix()}")
    return m.group(0)


@dataclass(frozen=True)
class VirtioPciIdentity:
    pci_vendor_id: int
    pci_device_id: int
    subsystem_vendor_id: int
    subsystem_device_id: int
    revision_id: int


@dataclass(frozen=True)
class VirtioDeviceTypeInfo:
    virtio_device_id: int
    pci_device_id: int


@dataclass(frozen=True)
class ManifestVirtioEntry:
    device: str
    pci_vendor_id: int
    pci_device_id: int
    virtio_device_type: int
    hardware_id_patterns: list[str]


def parse_w7_virtio_contract(md: str) -> tuple[int, int, int, dict[str, VirtioDeviceTypeInfo], dict[str, int]]:
    # 1.1 PCI identification gives the global IDs used by all Aero virtio devices.
    ident = extract_section(
        md,
        path=W7_VIRTIO_CONTRACT_MD,
        heading_re=r"^### 1\.1 PCI identification\b",
        until_re=r"^### ",
    )

    def extract_hex(label: str, pattern: str) -> int:
        m = re.search(pattern, ident, flags=re.M)
        if not m:
            fail(
                f"could not parse {label} from {W7_VIRTIO_CONTRACT_MD.as_posix()} "
                f"(expected to find {pattern!r} under '### 1.1 PCI identification')"
            )
        return int(m.group(1), 16)

    vendor_id = extract_hex("Vendor ID", r"\*\*Vendor ID:\*\*\s*`(0x[0-9A-Fa-f]+)`")
    revision_id = extract_hex("PCI Revision ID", r"\*\*PCI Revision ID:\*\*\s*`(0x[0-9A-Fa-f]+)`")
    subsystem_vendor_id = extract_hex(
        "Subsystem Vendor ID", r"\*\*Subsystem Vendor ID:\*\*\s*`(0x[0-9A-Fa-f]+)`"
    )

    if vendor_id != VIRTIO_PCI_VENDOR_ID:
        fail(
            f"AERO-W7-VIRTIO Vendor ID must be 0x{VIRTIO_PCI_VENDOR_ID:04X}, "
            f"got 0x{vendor_id:04X} (from {W7_VIRTIO_CONTRACT_MD.as_posix()})"
        )
    if subsystem_vendor_id != VIRTIO_PCI_VENDOR_ID:
        fail(
            f"AERO-W7-VIRTIO Subsystem Vendor ID must be 0x{VIRTIO_PCI_VENDOR_ID:04X}, "
            f"got 0x{subsystem_vendor_id:04X} (from {W7_VIRTIO_CONTRACT_MD.as_posix()})"
        )

    # Parse the "modern-only ID space" table.
    device_ids_section = extract_section(
        md,
        path=W7_VIRTIO_CONTRACT_MD,
        heading_re=r"^#### 1\.1\.1\b",
        until_re=r"^#### 1\.1\.2\b",
    )
    device_ids: dict[str, VirtioDeviceTypeInfo] = {}
    for m in re.finditer(
        r"^\|\s*(?P<device>virtio-[a-z0-9-]+)\s*\|\s*(?P<virtio_id>\d+)\s*\|\s*`(?P<pci_id>0x[0-9A-Fa-f]+)`\s*\|",
        device_ids_section,
        flags=re.M,
    ):
        device = m.group("device")
        virtio_id = int(m.group("virtio_id"))
        pci_id = int(m.group("pci_id"), 16)
        if device in device_ids:
            fail(f"duplicate device row {device!r} in AERO-W7-VIRTIO 1.1.1 table")
        expected_pci_id = VIRTIO_PCI_MODERN_DEVICE_ID_BASE + virtio_id
        if pci_id != expected_pci_id:
            fail(
                "AERO-W7-VIRTIO 1.1.1 table violates modern virtio-pci ID formula:\n"
                f"  device: {device}\n"
                f"  virtio device id: {virtio_id}\n"
                f"  expected PCI device id: 0x{expected_pci_id:04X}\n"
                f"  got: 0x{pci_id:04X}"
            )
        device_ids[device] = VirtioDeviceTypeInfo(virtio_device_id=virtio_id, pci_device_id=pci_id)

    if not device_ids:
        fail(f"could not parse any virtio device IDs from {W7_VIRTIO_CONTRACT_MD.as_posix()} (section 1.1.1)")

    subsystem_ids_section = extract_section(
        md,
        path=W7_VIRTIO_CONTRACT_MD,
        heading_re=r"^#### 1\.1\.2\b",
        until_re=r"^### ",
    )
    subsystem_ids: dict[str, int] = {}
    for m in re.finditer(
        r"^\|\s*(?P<instance>virtio-[^|]+?)\s*\|\s*`(?P<subsys_id>0x[0-9A-Fa-f]+)`\s*\|",
        subsystem_ids_section,
        flags=re.M,
    ):
        instance = m.group("instance").strip()
        subsys_id = int(m.group("subsys_id"), 16)
        if instance in subsystem_ids:
            fail(f"duplicate device instance row {instance!r} in AERO-W7-VIRTIO 1.1.2 table")
        base_device = instance.split(" (", 1)[0].strip()
        if base_device not in device_ids:
            fail(
                "AERO-W7-VIRTIO 1.1.2 references an unknown virtio device:\n"
                f"  instance: {instance}\n"
                f"  base device: {base_device}\n"
                "  (expected the base device to appear in table 1.1.1)"
            )
        subsystem_ids[instance] = subsys_id

    if not subsystem_ids:
        fail(f"could not parse any subsystem IDs from {W7_VIRTIO_CONTRACT_MD.as_posix()} (section 1.1.2)")

    return vendor_id, subsystem_vendor_id, revision_id, device_ids, subsystem_ids


def parse_windows_device_contract_md(md: str) -> dict[str, VirtioPciIdentity]:
    device_table = extract_section(
        md,
        path=WINDOWS_DEVICE_CONTRACT_MD,
        heading_re=r"^## Device table \(normative\)",
        until_re=r"^## ",
    )

    rows: dict[str, VirtioPciIdentity] = {}
    for m in re.finditer(
        r"^\|\s*(?P<label>virtio-[^|]+?)\s*\|\s*`(?P<pci_vendor>[0-9A-Fa-f]{4}):(?P<pci_device>[0-9A-Fa-f]{4})`"
        r"\s*\(REV\s*`(?P<rev>0x[0-9A-Fa-f]{2})`\)\s*\|\s*`(?P<subsys_vendor>[0-9A-Fa-f]{4}):(?P<subsys_device>[0-9A-Fa-f]{4})`",
        device_table,
        flags=re.M,
    ):
        label = m.group("label").strip()
        if label in rows:
            fail(f"duplicate device row {label!r} in {WINDOWS_DEVICE_CONTRACT_MD.as_posix()}")

        rows[label] = VirtioPciIdentity(
            pci_vendor_id=parse_hex(m.group("pci_vendor")),
            pci_device_id=parse_hex(m.group("pci_device")),
            subsystem_vendor_id=parse_hex(m.group("subsys_vendor")),
            subsystem_device_id=parse_hex(m.group("subsys_device")),
            revision_id=parse_hex(m.group("rev")),
        )

    if not rows:
        fail(f"could not parse any virtio rows from {WINDOWS_DEVICE_CONTRACT_MD.as_posix()} (Device table)")

    return rows


def parse_windows_device_contract_manifest(data: dict) -> dict[str, ManifestVirtioEntry]:
    devices = data.get("devices")
    if not isinstance(devices, list):
        fail(f"'devices' must be a list in {WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}")

    out: dict[str, ManifestVirtioEntry] = {}
    for dev in devices:
        if not isinstance(dev, dict):
            continue
        device_name = dev.get("device")
        if not isinstance(device_name, str) or not device_name.startswith("virtio-"):
            continue

        if device_name in out:
            fail(f"duplicate device entry {device_name!r} in {WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}")

        patterns = dev.get("hardware_id_patterns")
        if not isinstance(patterns, list) or not all(isinstance(p, str) for p in patterns):
            fail(
                f"{WINDOWS_DEVICE_CONTRACT_JSON.as_posix()} entry {device_name!r}: "
                "'hardware_id_patterns' must be a list of strings"
            )

        virtio_device_type = dev.get("virtio_device_type")
        if not isinstance(virtio_device_type, int):
            fail(
                f"{WINDOWS_DEVICE_CONTRACT_JSON.as_posix()} entry {device_name!r}: "
                "'virtio_device_type' must be an integer for virtio devices"
            )

        out[device_name] = ManifestVirtioEntry(
            device=device_name,
            pci_vendor_id=parse_hex_str(dev.get("pci_vendor_id"), context=f"{device_name}.pci_vendor_id"),
            pci_device_id=parse_hex_str(dev.get("pci_device_id"), context=f"{device_name}.pci_device_id"),
            virtio_device_type=virtio_device_type,
            hardware_id_patterns=patterns,
        )

    if not out:
        fail(f"no virtio-* device entries found in {WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}")

    return out


def hardware_id_contains_vendor_device(pattern: str, vendor_id: int, device_id: int) -> bool:
    expected = f"PCI\\VEN_{vendor_id:04X}&DEV_{device_id:04X}"
    return pattern.upper().startswith(expected)


def hardware_id_extract_vendor_device(pattern: str) -> tuple[int, int] | None:
    m = re.search(r"PCI\\VEN_([0-9A-Fa-f]{4})&DEV_([0-9A-Fa-f]{4})", pattern)
    if not m:
        return None
    return int(m.group(1), 16), int(m.group(2), 16)

def docs_mark_transitional_ids_out_of_scope(md: str) -> bool:
    # windows-device-contract.md includes a normative compatibility note explicitly
    # stating transitional virtio-pci IDs are out-of-scope for AERO-W7-VIRTIO.
    return (
        re.search(r"Transitional virtio-pci IDs.*out of scope", md, flags=re.I | re.S) is not None
    )


def main() -> None:
    w7_vendor, w7_subsys_vendor, w7_revision, w7_device_ids, w7_subsystem_ids = parse_w7_virtio_contract(
        read_text(W7_VIRTIO_CONTRACT_MD)
    )

    windows_md_text = read_text(WINDOWS_DEVICE_CONTRACT_MD)
    windows_doc_rows = parse_windows_device_contract_md(windows_md_text)

    try:
        manifest_raw = json.loads(read_text(WINDOWS_DEVICE_CONTRACT_JSON))
    except json.JSONDecodeError as e:
        fail(f"invalid JSON in {WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}: {e}")
    if not isinstance(manifest_raw, dict):
        fail(f"top-level JSON must be an object in {WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}")

    manifest_entries = parse_windows_device_contract_manifest(manifest_raw)

    # Ensure windows-device-contract.md doesn't invent new virtio instances that aren't in AERO-W7-VIRTIO.
    for label in windows_doc_rows.keys():
        if label not in w7_subsystem_ids:
            fail(
                "windows-device-contract.md contains a virtio device row not present in AERO-W7-VIRTIO:\n"
                f"  row: {label!r}\n"
                f"  hint: add it to {W7_VIRTIO_CONTRACT_MD.as_posix()} first (AERO-W7-VIRTIO is authoritative)"
            )

    # 1) windows-device-contract.md must match the authoritative W7 contract per device instance.
    for instance_label, subsys_id in w7_subsystem_ids.items():
        base_device = instance_label.split(" (", 1)[0].strip()
        type_info = w7_device_ids[base_device]
        expected = VirtioPciIdentity(
            pci_vendor_id=w7_vendor,
            pci_device_id=type_info.pci_device_id,
            subsystem_vendor_id=w7_subsys_vendor,
            subsystem_device_id=subsys_id,
            revision_id=w7_revision,
        )

        row = windows_doc_rows.get(instance_label)
        if row is None:
            fail(
                "missing virtio device row in windows-device-contract.md:\n"
                f"  expected row for: {instance_label!r}\n"
                f"  (based on AERO-W7-VIRTIO 1.1.2)"
            )

        if row != expected:
            fail(
                "virtio PCI identity mismatch between contracts:\n"
                f"  device instance: {instance_label}\n"
                f"  {W7_VIRTIO_CONTRACT_MD.as_posix()} expects: "
                f"{expected.pci_vendor_id:04X}:{expected.pci_device_id:04X} "
                f"SUBSYS {expected.subsystem_vendor_id:04X}:{expected.subsystem_device_id:04X} "
                f"REV 0x{expected.revision_id:02X}\n"
                f"  {WINDOWS_DEVICE_CONTRACT_MD.as_posix()} has:      "
                f"{row.pci_vendor_id:04X}:{row.pci_device_id:04X} "
                f"SUBSYS {row.subsystem_vendor_id:04X}:{row.subsystem_device_id:04X} "
                f"REV 0x{row.revision_id:02X}"
            )

    # Ensure windows-device-contract.json doesn't invent virtio devices that aren't in AERO-W7-VIRTIO.
    for device_name in manifest_entries.keys():
        if device_name not in w7_device_ids:
            fail(
                "windows-device-contract.json contains a virtio device entry not present in AERO-W7-VIRTIO:\n"
                f"  entry: {device_name!r}\n"
                f"  hint: add it to {W7_VIRTIO_CONTRACT_MD.as_posix()} first (AERO-W7-VIRTIO is authoritative)"
            )

    # 2) windows-device-contract.json must match Vendor/Device ID and virtio_device_type for each base device.
    for device_name, type_info in w7_device_ids.items():
        entry = manifest_entries.get(device_name)
        if entry is None:
            fail(
                "missing virtio device entry in windows-device-contract.json:\n"
                f"  expected entry for: {device_name!r}\n"
                f"  (based on AERO-W7-VIRTIO 1.1.1)"
            )

        if entry.pci_vendor_id != w7_vendor or entry.pci_device_id != type_info.pci_device_id:
            fail(
                "virtio Vendor/Device ID mismatch between AERO-W7-VIRTIO and windows-device-contract.json:\n"
                f"  device: {device_name}\n"
                f"  {W7_VIRTIO_CONTRACT_MD.as_posix()}: "
                f"{w7_vendor:04X}:{type_info.pci_device_id:04X}\n"
                f"  {WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}: "
                f"{entry.pci_vendor_id:04X}:{entry.pci_device_id:04X}"
            )

        if entry.pci_vendor_id != VIRTIO_PCI_VENDOR_ID:
            fail(
                f"{WINDOWS_DEVICE_CONTRACT_JSON.as_posix()} entry {device_name!r}: "
                f"pci_vendor_id must be 0x{VIRTIO_PCI_VENDOR_ID:04X}, got 0x{entry.pci_vendor_id:04X}"
            )

        if entry.virtio_device_type != type_info.virtio_device_id:
            fail(
                f"{WINDOWS_DEVICE_CONTRACT_JSON.as_posix()} entry {device_name!r}: "
                "virtio_device_type mismatch with AERO-W7-VIRTIO:\n"
                f"  expected: {type_info.virtio_device_id}\n"
                f"  got: {entry.virtio_device_type}"
            )

        expected_device_id = VIRTIO_PCI_MODERN_DEVICE_ID_BASE + entry.virtio_device_type
        if entry.pci_device_id != expected_device_id:
            fail(
                f"{WINDOWS_DEVICE_CONTRACT_JSON.as_posix()} entry {device_name!r}: "
                "pci_device_id violates modern virtio-pci ID formula:\n"
                f"  virtio_device_type: {entry.virtio_device_type}\n"
                f"  expected pci_device_id: 0x{expected_device_id:04X}\n"
                f"  got: 0x{entry.pci_device_id:04X}"
            )

        patterns_upper = [p.upper() for p in entry.hardware_id_patterns]

        # 3) Manifest patterns must include at least a Vendor/Device prefix.
        if not any(hardware_id_contains_vendor_device(p, entry.pci_vendor_id, entry.pci_device_id) for p in entry.hardware_id_patterns):
            fail(
                f"{WINDOWS_DEVICE_CONTRACT_JSON.as_posix()} entry {device_name!r}: "
                "hardware_id_patterns is missing a PCI\\VEN_xxxx&DEV_yyyy pattern for the canonical IDs:\n"
                f"  expected prefix: PCI\\VEN_{entry.pci_vendor_id:04X}&DEV_{entry.pci_device_id:04X}\n"
                f"  got: {entry.hardware_id_patterns}"
            )

        # 4) Patterns must include revision gating (REV_XX). If the driver doesn't revision-gate in the INF,
        # we still require the *manifest* to include at least one REV pattern so automation (Guest Tools,
        # CDD seeding) can remain version-safe.
        expected_rev_fragment = f"REV_{w7_revision:02X}"
        if not any(expected_rev_fragment in p for p in patterns_upper):
            fail(
                f"{WINDOWS_DEVICE_CONTRACT_JSON.as_posix()} entry {device_name!r}: "
                "hardware_id_patterns must include at least one revision-qualified pattern:\n"
                f"  expected to contain: &{expected_rev_fragment}\n"
                f"  got: {entry.hardware_id_patterns}"
            )

        # 5) Subsystem IDs: require patterns that include the contract-defined SUBSYS fragment.
        expected_subsys_ids = sorted(
            subsys for label, subsys in w7_subsystem_ids.items() if label.split(" (", 1)[0].strip() == device_name
        )
        for subsys_id in expected_subsys_ids:
            expected_subsys_fragment = f"SUBSYS_{subsys_id:04X}{w7_subsys_vendor:04X}"
            if not any(expected_subsys_fragment in p for p in patterns_upper):
                fail(
                    f"{WINDOWS_DEVICE_CONTRACT_JSON.as_posix()} entry {device_name!r}: "
                    "hardware_id_patterns missing the expected subsystem-qualified hardware ID:\n"
                    f"  expected: {expected_subsys_fragment}\n"
                    f"  got: {entry.hardware_id_patterns}"
                )

        # 6) Transitional IDs are out-of-scope for AERO-W7-VIRTIO v1. If they appear in the manifest
        # (for optional driver compatibility), require the docs to explicitly mark them as out-of-scope
        # so consumers don't mistake them for part of the Aero contract.
        transitional_patterns: list[str] = []
        for p in entry.hardware_id_patterns:
            extracted = hardware_id_extract_vendor_device(p)
            if extracted is None:
                continue
            ven, dev = extracted
            if ven != VIRTIO_PCI_VENDOR_ID:
                continue
            if VIRTIO_PCI_TRANSITIONAL_DEVICE_ID_MIN <= dev <= VIRTIO_PCI_TRANSITIONAL_DEVICE_ID_MAX:
                transitional_patterns.append(p)

        if transitional_patterns and not docs_mark_transitional_ids_out_of_scope(windows_md_text):
            fail(
                f"{WINDOWS_DEVICE_CONTRACT_JSON.as_posix()} entry {device_name!r}: "
                "hardware_id_patterns includes virtio transitional IDs, but the docs do not explicitly "
                "mark transitional IDs as out-of-scope.\n"
                f"  transitional patterns: {transitional_patterns}\n"
                f"  hint: update {WINDOWS_DEVICE_CONTRACT_MD.as_posix()} compatibility note to include 'out of scope'"
            )


if __name__ == "__main__":
    main()
