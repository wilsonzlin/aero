#!/usr/bin/env python3
"""
Guardrail: prevent drift between the definitive Windows 7 virtio contract
(`docs/windows7-virtio-driver-contract.md`, Contract ID: AERO-W7-VIRTIO) and the
Windows device/driver binding docs/manifest (`docs/windows-device-contract.*`).

Why this exists:
  - PCI IDs are effectively API for Windows INF matching and Guest Tools
    CriticalDeviceDatabase seeding.
  - A mismatch between documents can silently break driver binding.

This check validates *all* AERO-W7-VIRTIO v1 virtio devices (blk/net/input/snd),
including multi-function cases (virtio-input keyboard + mouse). It also checks a
small set of Win7 contract v1 invariants (revision ID, required ring features,
and queue sizes), and optionally asserts that the emulator's canonical PCI
profiles match the contract.
"""

from __future__ import annotations

import json
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, Mapping


REPO_ROOT = Path(__file__).resolve().parents[2]

W7_VIRTIO_CONTRACT_MD = REPO_ROOT / "docs/windows7-virtio-driver-contract.md"
WINDOWS_DEVICE_CONTRACT_MD = REPO_ROOT / "docs/windows-device-contract.md"
WINDOWS_DEVICE_CONTRACT_JSON = REPO_ROOT / "docs/windows-device-contract.json"

# virtio-pci vendor as allocated by PCI-SIG.
VIRTIO_PCI_VENDOR_ID = 0x1AF4
# Modern virtio-pci device IDs: 0x1040 + virtio device type.
VIRTIO_PCI_DEVICE_ID_BASE = 0x1040
# Transitional virtio-pci device IDs live in the 0x1000..0x103F range.
VIRTIO_PCI_TRANSITIONAL_DEVICE_ID_MIN = 0x1000
VIRTIO_PCI_TRANSITIONAL_DEVICE_ID_MAX = 0x103F

VIRTIO_DEVICE_TYPE_IDS: Mapping[str, int] = {
    "virtio-net": 1,
    "virtio-blk": 2,
    "virtio-input": 18,
    "virtio-snd": 25,
}

AERO_DEVICES_PCI_PROFILE_RS = REPO_ROOT / "crates/devices/src/pci/profile.rs"
AERO_VIRTIO_DEVICE_SOURCES: Mapping[str, Path] = {
    "virtio-blk": REPO_ROOT / "crates/aero-virtio/src/devices/blk.rs",
    "virtio-net": REPO_ROOT / "crates/aero-virtio/src/devices/net.rs",
    "virtio-input": REPO_ROOT / "crates/aero-virtio/src/devices/input.rs",
    "virtio-snd": REPO_ROOT / "crates/aero-virtio/src/devices/snd.rs",
}


def fail(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(1)


def format_error(header: str, lines: Iterable[str]) -> str:
    body = "\n".join(f"  {line}" for line in lines)
    return f"{header}\n{body}"


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


def windows_contract_marks_transitional_ids_out_of_scope(md: str) -> bool:
    return re.search(r"Transitional virtio-pci IDs.*out of scope", md, flags=re.I | re.S) is not None


@dataclass(frozen=True)
class PciIdentity:
    vendor_id: int
    device_id: int
    subsystem_vendor_id: int
    subsystem_device_id: int
    revision_id: int

    def vendor_device_str(self) -> str:
        return f"{self.vendor_id:04X}:{self.device_id:04X}"

    def subsys_str(self) -> str:
        return f"{self.subsystem_vendor_id:04X}:{self.subsystem_device_id:04X}"


def parse_contract_revision_id(md: str) -> int:
    m = re.search(
        r"Contract version:.*?`[^`]+`.*?PCI Revision ID\s*=\s*`(?P<rev>0x[0-9A-Fa-f]+)`",
        md,
        flags=re.S,
    )
    if not m:
        fail(
            f"could not parse contract revision ID from {W7_VIRTIO_CONTRACT_MD.as_posix()} "
            "(expected 'Contract version: `1.0` (PCI Revision ID = `0x01`)')"
        )
    return int(m.group("rev"), 16)


def _extract_section(md: str, *, start: str, end: str | None = None, file: Path, what: str) -> str:
    if end is None:
        end = r"^###\s+3\.\d+\s"
    m = re.search(rf"(?P<body>{start}.*?)(?={end}|\Z)", md, flags=re.S | re.M)
    if not m:
        fail(f"could not locate {what} in {file.as_posix()} (pattern: {start})")
    return m.group("body")


def _extract_subsection(section: str, *, heading: str, file: Path) -> str:
    m = re.search(
        rf"(?P<body>^{heading}.*?)(?=^####\s+|^###\s+|^##\s+|\Z)",
        section,
        flags=re.S | re.M,
    )
    if not m:
        fail(f"could not locate subsection '{heading}' in {file.as_posix()}")
    return m.group("body")


def _parse_pci_id_block(block: str, *, file: Path, context: str) -> PciIdentity:
    def field(label: str) -> int:
        m = re.search(rf"^- {re.escape(label)}:\s*`(0x[0-9A-Fa-f]+)`\s*$", block, flags=re.M)
        if not m:
            fail(
                f"could not parse '{label}' for {context} in {file.as_posix()} "
                "(expected a bullet like '- Device ID: `0x1059`')"
            )
        return int(m.group(1), 16)

    return PciIdentity(
        vendor_id=field("Vendor ID"),
        device_id=field("Device ID"),
        subsystem_vendor_id=field("Subsystem Vendor ID"),
        subsystem_device_id=field("Subsystem Device ID"),
        revision_id=field("Revision ID"),
    )


def parse_w7_contract_pci_identities(md: str) -> Mapping[str, PciIdentity]:
    blk_section = _extract_section(
        md,
        start=r"^###\s+3\.1\s+virtio-blk",
        file=W7_VIRTIO_CONTRACT_MD,
        what="virtio-blk section",
    )
    net_section = _extract_section(
        md,
        start=r"^###\s+3\.2\s+virtio-net",
        file=W7_VIRTIO_CONTRACT_MD,
        what="virtio-net section",
    )
    input_section = _extract_section(
        md,
        start=r"^###\s+3\.3\s+virtio-input",
        file=W7_VIRTIO_CONTRACT_MD,
        what="virtio-input section",
    )
    snd_section = _extract_section(
        md,
        start=r"^###\s+3\.4\s+virtio-snd",
        file=W7_VIRTIO_CONTRACT_MD,
        what="virtio-snd section",
    )

    blk_ids = _parse_pci_id_block(
        _extract_subsection(blk_section, heading=r"####\s+3\.1\.1\s+PCI IDs", file=W7_VIRTIO_CONTRACT_MD),
        file=W7_VIRTIO_CONTRACT_MD,
        context="virtio-blk",
    )
    net_ids = _parse_pci_id_block(
        _extract_subsection(net_section, heading=r"####\s+3\.2\.1\s+PCI IDs", file=W7_VIRTIO_CONTRACT_MD),
        file=W7_VIRTIO_CONTRACT_MD,
        context="virtio-net",
    )
    snd_ids = _parse_pci_id_block(
        _extract_subsection(snd_section, heading=r"####\s+3\.4\.1\s+PCI IDs", file=W7_VIRTIO_CONTRACT_MD),
        file=W7_VIRTIO_CONTRACT_MD,
        context="virtio-snd",
    )

    input_ids_section = _extract_subsection(
        input_section, heading=r"####\s+3\.3\.1\s+PCI IDs", file=W7_VIRTIO_CONTRACT_MD
    )
    keyboard_pos = re.search(r"^Keyboard:\s*$", input_ids_section, flags=re.M)
    mouse_pos = re.search(r"^Mouse:\s*$", input_ids_section, flags=re.M)
    if not keyboard_pos or not mouse_pos:
        fail(
            f"could not locate Keyboard/Mouse PCI ID blocks in {W7_VIRTIO_CONTRACT_MD.as_posix()} "
            "(expected 'Keyboard:' and 'Mouse:' labels under '#### 3.3.1 PCI IDs')"
        )
    keyboard_block = input_ids_section[keyboard_pos.end() : mouse_pos.start()]
    mouse_block = input_ids_section[mouse_pos.end() :]

    keyboard_ids = _parse_pci_id_block(
        keyboard_block,
        file=W7_VIRTIO_CONTRACT_MD,
        context="virtio-input (keyboard)",
    )
    mouse_ids = _parse_pci_id_block(
        mouse_block,
        file=W7_VIRTIO_CONTRACT_MD,
        context="virtio-input (mouse)",
    )

    return {
        "virtio-blk": blk_ids,
        "virtio-net": net_ids,
        "virtio-snd": snd_ids,
        "virtio-input (keyboard)": keyboard_ids,
        "virtio-input (mouse)": mouse_ids,
    }


def _parse_queue_table_sizes(block: str, *, file: Path, context: str) -> dict[int, int]:
    sizes: dict[int, int] = {}
    for m in re.finditer(r"^\|\s*(?P<idx>\d+)\s*\|.*?\|\s*\*\*(?P<size>\d+)\*\*\s*\|\s*$", block, flags=re.M):
        sizes[int(m.group("idx"))] = int(m.group("size"))
    if not sizes:
        fail(
            f"could not parse any virtqueue sizes for {context} in {file.as_posix()} "
            "(expected table rows like '| 0 | `rxq` | ... | **256** |')"
        )
    return sizes


def parse_w7_contract_queue_sizes(md: str) -> Mapping[str, dict[int, int]]:
    # Queue sizes are device-wide (not variant-specific for virtio-input keyboard/mouse).
    out: dict[str, dict[int, int]] = {}
    for key, start, heading in (
        ("virtio-blk", r"^###\s+3\.1\s+virtio-blk", r"####\s+3\.1\.2\s+Virtqueues"),
        ("virtio-net", r"^###\s+3\.2\s+virtio-net", r"####\s+3\.2\.2\s+Virtqueues"),
        ("virtio-input", r"^###\s+3\.3\s+virtio-input", r"####\s+3\.3\.2\s+Virtqueues"),
        ("virtio-snd", r"^###\s+3\.4\s+virtio-snd", r"####\s+3\.4\.2\s+Virtqueues"),
    ):
        section = _extract_section(md, start=start, file=W7_VIRTIO_CONTRACT_MD, what=f"{key} section")
        vq = _parse_queue_table_sizes(
            _extract_subsection(section, heading=heading, file=W7_VIRTIO_CONTRACT_MD),
            file=W7_VIRTIO_CONTRACT_MD,
            context=key,
        )
        out[key] = vq
    return out


def parse_windows_device_contract_table(md: str) -> Mapping[str, PciIdentity]:
    expected_rows = {
        "virtio-blk",
        "virtio-net",
        "virtio-snd",
        "virtio-input (keyboard)",
        "virtio-input (mouse)",
    }
    out: dict[str, PciIdentity] = {}
    for line in md.splitlines():
        if not line.lstrip().startswith("|"):
            continue
        if line.strip().startswith("| Device |"):
            continue
        # | virtio-blk | `1AF4:1042` (REV `0x01`) | `1AF4:0002` | ...
        m = re.match(
            r"^\|\s*(?P<name>[^|]+?)\s*\|\s*`(?P<pci>[0-9A-Fa-f]{4}:[0-9A-Fa-f]{4})`\s*\(REV\s*`0x(?P<rev>[0-9A-Fa-f]{2})`\)\s*\|\s*`(?P<subsys>[0-9A-Fa-f]{4}:[0-9A-Fa-f]{4})`\s*\|",
            line,
        )
        if not m:
            continue
        name = m.group("name").strip()
        if name not in expected_rows:
            continue
        pci_vendor_s, pci_device_s = m.group("pci").split(":")
        subsys_vendor_s, subsys_device_s = m.group("subsys").split(":")
        out[name] = PciIdentity(
            vendor_id=parse_hex(pci_vendor_s),
            device_id=parse_hex(pci_device_s),
            subsystem_vendor_id=parse_hex(subsys_vendor_s),
            subsystem_device_id=parse_hex(subsys_device_s),
            revision_id=int(m.group("rev"), 16),
        )

    missing = sorted(expected_rows - set(out.keys()))
    if missing:
        fail(
            format_error(
                f"could not locate expected virtio device rows in {WINDOWS_DEVICE_CONTRACT_MD.as_posix()}:",
                [f"missing: {row!r}" for row in missing],
            )
        )
    return out


def _find_manifest_device(devices: list[object], name: str) -> dict[str, object]:
    for device in devices:
        if isinstance(device, dict) and device.get("device") == name:
            return device
    fail(f"device entry {name!r} not found in {WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}")


def _parse_manifest_hex_u16(entry: Mapping[str, object], field: str, *, device: str) -> int:
    raw = entry.get(field)
    if not isinstance(raw, str):
        fail(
            f"{WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}: {device} field {field!r} must be a hex string like '0x1AF4'"
        )
    try:
        return int(raw, 16)
    except ValueError:
        fail(
            f"{WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}: {device} field {field!r} has invalid hex string: {raw!r}"
        )


def _require_str_list(entry: Mapping[str, object], field: str, *, device: str) -> list[str]:
    raw = entry.get(field)
    if not isinstance(raw, list) or not all(isinstance(p, str) for p in raw):
        fail(f"{WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}: {device} field {field!r} must be list[str]")
    return list(raw)


def _manifest_contains_subsys(patterns: list[str], subsys_vendor: int, subsys_device: int) -> bool:
    needle = f"SUBSYS_{subsys_device:04X}{subsys_vendor:04X}"
    return any(needle in p.upper() for p in patterns)


def parse_contract_feature_bits(md: str, *, file: Path) -> dict[str, int]:
    version_section = _extract_section(
        md,
        start=r"^####\s+1\.5\.2\s+Required feature bits",
        end=r"^####\s+1\.5\.3\s+",
        file=file,
        what="section 1.5.2 (Required feature bits)",
    )
    ring_section = _extract_section(
        md,
        start=r"^###\s+2\.3\s+Supported ring/queue feature bits",
        end=r"^###\s+2\.4\s+",
        file=file,
        what="section 2.3 (Supported ring/queue feature bits)",
    )

    def bit(name: str, text: str) -> int:
        m = re.search(rf"`{re.escape(name)}`\s*\(bit\s*(?P<bit>\d+)\)", text)
        if not m:
            fail(f"could not locate `{name}` bit number in {file.as_posix()}")
        return int(m.group("bit"))

    return {
        "VIRTIO_F_VERSION_1": bit("VIRTIO_F_VERSION_1", version_section),
        "VIRTIO_F_RING_INDIRECT_DESC": bit("VIRTIO_F_RING_INDIRECT_DESC", ring_section),
        "VIRTIO_F_RING_EVENT_IDX": bit("VIRTIO_F_RING_EVENT_IDX", ring_section),
    }


def parse_windows_device_contract_feature_bits(md: str, *, file: Path) -> dict[str, int]:
    def bit(name: str) -> int:
        m = re.search(rf"`{re.escape(name)}`\s*\(bit\s*(?P<bit>\d+)\)", md)
        if not m:
            fail(f"could not locate `{name}` bit number in {file.as_posix()}")
        return int(m.group("bit"))

    return {
        "VIRTIO_F_VERSION_1": bit("VIRTIO_F_VERSION_1"),
        "VIRTIO_F_RING_INDIRECT_DESC": bit("VIRTIO_F_RING_INDIRECT_DESC"),
        "VIRTIO_F_RING_EVENT_IDX": bit("VIRTIO_F_RING_EVENT_IDX"),
    }


def parse_emulator_pci_profiles(path: Path) -> Mapping[str, PciIdentity]:
    text = read_text(path)

    const_values: dict[str, int] = {}
    for m in re.finditer(r"^pub const (?P<name>[A-Z0-9_]+): u16 = (?P<value>0x[0-9A-Fa-f]+);", text, flags=re.M):
        const_values[m.group("name")] = int(m.group("value"), 16)

    def eval_u16(expr: str) -> int:
        expr = expr.strip()
        if expr.startswith("0x") or expr.startswith("0X"):
            return int(expr, 16)
        if expr.isdigit():
            return int(expr, 10)
        if expr in const_values:
            return const_values[expr]
        fail(
            f"{path.as_posix()}: unsupported u16 expression {expr!r} while parsing PciDeviceProfile "
            "(expected a literal like 0x1af4 or a const like PCI_VENDOR_ID_VIRTIO)"
        )

    def eval_u8(expr: str) -> int:
        expr = expr.strip()
        if expr.startswith("0x") or expr.startswith("0X"):
            return int(expr, 16)
        if expr.isdigit():
            return int(expr, 10)
        fail(f"{path.as_posix()}: unsupported u8 expression {expr!r} while parsing PciDeviceProfile")

    profiles: dict[str, PciIdentity] = {}
    for profile_name in ("VIRTIO_NET", "VIRTIO_BLK", "VIRTIO_SND", "VIRTIO_INPUT_KEYBOARD", "VIRTIO_INPUT_MOUSE"):
        m = re.search(
            rf"^pub const {profile_name}: PciDeviceProfile = PciDeviceProfile \{{(?P<body>.*?)^\}};",
            text,
            flags=re.S | re.M,
        )
        if not m:
            fail(f"{path.as_posix()}: missing PciDeviceProfile constant {profile_name}")
        body = m.group("body")

        def field(field_name: str) -> str:
            fm = re.search(rf"^\s*{re.escape(field_name)}:\s*(?P<expr>[^,]+),", body, flags=re.M)
            if not fm:
                fail(f"{path.as_posix()}: profile {profile_name} missing field {field_name}")
            return fm.group("expr").strip()

        profiles[profile_name] = PciIdentity(
            vendor_id=eval_u16(field("vendor_id")),
            device_id=eval_u16(field("device_id")),
            subsystem_vendor_id=eval_u16(field("subsystem_vendor_id")),
            subsystem_device_id=eval_u16(field("subsystem_id")),
            revision_id=eval_u8(field("revision_id")),
        )

    return profiles


def parse_aero_virtio_queue_max_sizes() -> Mapping[str, dict[int, int]]:
    out: dict[str, dict[int, int]] = {}

    # blk/net/input are simple `queue_max_size` implementations returning a constant.
    for name in ("virtio-blk", "virtio-net", "virtio-input"):
        path = AERO_VIRTIO_DEVICE_SOURCES[name]
        text = read_text(path)
        m = re.search(
            r"fn\s+queue_max_size\s*\([^)]*\)\s*->\s*u16\s*\{\s*(?P<val>\d+)\s*\}",
            text,
            flags=re.S,
        )
        if not m:
            fail(
                f"could not parse queue_max_size() constant for {name} from {path.as_posix()} "
                "(expected a body like '{ 256 }')"
            )
        size = int(m.group("val"))
        # Derive queue indices from contract v1 expectations:
        # - blk: queue 0
        # - net: queues 0 and 1
        # - input: queues 0 and 1
        if name == "virtio-blk":
            out[name] = {0: size}
        else:
            out[name] = {0: size, 1: size}

    # virtio-snd has per-queue sizes (match statement).
    snd_path = AERO_VIRTIO_DEVICE_SOURCES["virtio-snd"]
    snd_text = read_text(snd_path)
    queue_consts: dict[str, int] = {}
    for m in re.finditer(
        r"^pub const (?P<name>VIRTIO_SND_QUEUE_[A-Z0-9_]+): u16 = (?P<val>\d+);",
        snd_text,
        flags=re.M,
    ):
        queue_consts[m.group("name")] = int(m.group("val"))
    required_consts = ("VIRTIO_SND_QUEUE_CONTROL", "VIRTIO_SND_QUEUE_EVENT", "VIRTIO_SND_QUEUE_TX", "VIRTIO_SND_QUEUE_RX")
    missing = [c for c in required_consts if c not in queue_consts]
    if missing:
        fail(f"{snd_path.as_posix()}: missing expected virtio-snd queue index constants: {missing}")

    def extract_braced_block(src: str, start_at: int) -> str:
        """Return the substring inside the first {...} block starting at/after start_at."""
        brace = src.find("{", start_at)
        if brace == -1:
            raise ValueError("no opening brace")
        depth = 0
        for i in range(brace, len(src)):
            ch = src[i]
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
                if depth == 0:
                    return src[brace + 1 : i]
        raise ValueError("unterminated brace block")

    fn_pos = snd_text.find("fn queue_max_size")
    if fn_pos == -1:
        fail(f"could not locate virtio-snd queue_max_size() in {snd_path.as_posix()}")
    try:
        fn_body = extract_braced_block(snd_text, fn_pos)
    except ValueError as e:
        fail(f"failed to parse virtio-snd queue_max_size() braces in {snd_path.as_posix()}: {e}")

    match_pos = fn_body.find("match queue")
    if match_pos == -1:
        fail(f"virtio-snd queue_max_size() does not contain 'match queue' in {snd_path.as_posix()}")
    try:
        match_arms = extract_braced_block(fn_body, match_pos)
    except ValueError as e:
        fail(f"failed to parse virtio-snd queue_max_size() match arms in {snd_path.as_posix()}: {e}")

    sizes: dict[int, int] = {}
    for arm in re.finditer(r"^(?P<pat>[^=]+)=>\s*(?P<val>\d+)\s*,", match_arms, flags=re.M):
        pat = arm.group("pat").strip()
        if pat == "_":
            continue
        val = int(arm.group("val"))
        for token in pat.split("|"):
            token = token.strip()
            if token in queue_consts:
                sizes[queue_consts[token]] = val
    if not sizes:
        fail(f"could not derive any virtio-snd queue sizes from {snd_path.as_posix()}")
    out["virtio-snd"] = sizes

    return out


def parse_aero_virtio_device_feature_usage() -> Mapping[str, dict[str, bool]]:
    out: dict[str, dict[str, bool]] = {}
    for name, path in AERO_VIRTIO_DEVICE_SOURCES.items():
        text = read_text(path)
        out[name] = {
            "has_version_1": "VIRTIO_F_VERSION_1" in text,
            "has_indirect_desc": "VIRTIO_F_RING_INDIRECT_DESC" in text,
            "mentions_event_idx": "VIRTIO_F_RING_EVENT_IDX" in text,
        }
    return out


def main() -> None:
    errors: list[str] = []

    w7_md = read_text(W7_VIRTIO_CONTRACT_MD)
    windows_md = read_text(WINDOWS_DEVICE_CONTRACT_MD)

    try:
        manifest = json.loads(read_text(WINDOWS_DEVICE_CONTRACT_JSON))
    except json.JSONDecodeError as e:
        fail(f"invalid JSON in {WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}: {e}")

    contract_rev = parse_contract_revision_id(w7_md)
    contract_ids = parse_w7_contract_pci_identities(w7_md)
    contract_queues = parse_w7_contract_queue_sizes(w7_md)

    # ---------------------------------------------------------------------
    # 0) Fixed virtio-pci invariants that must never drift.
    # ---------------------------------------------------------------------
    for name, ids in contract_ids.items():
        if ids.vendor_id != VIRTIO_PCI_VENDOR_ID:
            errors.append(
                format_error(
                    f"{name}: contract Vendor ID must be 0x{VIRTIO_PCI_VENDOR_ID:04X}:",
                    [f"got: 0x{ids.vendor_id:04X}"],
                )
            )
        if ids.subsystem_vendor_id != VIRTIO_PCI_VENDOR_ID:
            errors.append(
                format_error(
                    f"{name}: contract Subsystem Vendor ID must be 0x{VIRTIO_PCI_VENDOR_ID:04X}:",
                    [f"got: 0x{ids.subsystem_vendor_id:04X}"],
                )
            )

        base_name = name.split(" (", 1)[0]
        virtio_type = VIRTIO_DEVICE_TYPE_IDS.get(base_name)
        if virtio_type is not None:
            expected_device_id = VIRTIO_PCI_DEVICE_ID_BASE + virtio_type
            if ids.device_id != expected_device_id:
                errors.append(
                    format_error(
                        f"{name}: contract PCI Device ID must follow modern virtio-pci formula (0x1040 + virtio_device_type):",
                        [
                            f"virtio_device_type: {virtio_type}",
                            f"expected: 0x{expected_device_id:04X}",
                            f"got: 0x{ids.device_id:04X}",
                        ],
                    )
                )

    # ---------------------------------------------------------------------
    # 1) windows-device-contract.md table must match authoritative contract.
    # ---------------------------------------------------------------------
    table_ids = parse_windows_device_contract_table(windows_md)
    for row_name, contract in contract_ids.items():
        doc = table_ids.get(row_name)
        if not doc:
            continue
        if (doc.vendor_id, doc.device_id) != (contract.vendor_id, contract.device_id):
            errors.append(
                format_error(
                    f"{row_name}: Vendor/Device ID mismatch between docs:",
                    [
                        f"{W7_VIRTIO_CONTRACT_MD.as_posix()}: {contract.vendor_device_str()}",
                        f"{WINDOWS_DEVICE_CONTRACT_MD.as_posix()}: {doc.vendor_device_str()}",
                    ],
                )
            )
        if (doc.subsystem_vendor_id, doc.subsystem_device_id) != (
            contract.subsystem_vendor_id,
            contract.subsystem_device_id,
        ):
            errors.append(
                format_error(
                    f"{row_name}: Subsystem Vendor/Device ID mismatch between docs:",
                    [
                        f"{W7_VIRTIO_CONTRACT_MD.as_posix()}: {contract.subsys_str()}",
                        f"{WINDOWS_DEVICE_CONTRACT_MD.as_posix()}: {doc.subsys_str()}",
                    ],
                )
            )
        if doc.revision_id != contract.revision_id or contract.revision_id != contract_rev:
            errors.append(
                format_error(
                    f"{row_name}: Revision ID mismatch (contract major version is encoded in PCI Revision ID):",
                    [
                        f"{W7_VIRTIO_CONTRACT_MD.as_posix()}: 0x{contract.revision_id:02X} (header says 0x{contract_rev:02X})",
                        f"{WINDOWS_DEVICE_CONTRACT_MD.as_posix()}: 0x{doc.revision_id:02X}",
                    ],
                )
            )

    # ---------------------------------------------------------------------
    # 2) windows-device-contract.json must match Vendor/Device ID + SUBSYS IDs.
    # ---------------------------------------------------------------------
    devices = manifest.get("devices")
    if not isinstance(devices, list):
        fail(f"'devices' must be a list in {WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}")

    for device_name in ("virtio-blk", "virtio-net", "virtio-snd", "virtio-input"):
        entry = _find_manifest_device(devices, device_name)
        vendor = _parse_manifest_hex_u16(entry, "pci_vendor_id", device=device_name)
        dev_id = _parse_manifest_hex_u16(entry, "pci_device_id", device=device_name)
        expected_type = VIRTIO_DEVICE_TYPE_IDS[device_name]
        expected_modern_id = VIRTIO_PCI_DEVICE_ID_BASE + expected_type
        if vendor != VIRTIO_PCI_VENDOR_ID:
            errors.append(
                format_error(
                    f"{device_name}: manifest pci_vendor_id must be 0x{VIRTIO_PCI_VENDOR_ID:04X}:",
                    [
                        f"expected: 0x{VIRTIO_PCI_VENDOR_ID:04X}",
                        f"got: 0x{vendor:04X}",
                    ],
                )
            )
        if dev_id != expected_modern_id:
            errors.append(
                format_error(
                    f"{device_name}: manifest pci_device_id must follow modern virtio-pci formula (0x1040 + virtio_device_type):",
                    [
                        f"virtio_device_type: {expected_type}",
                        f"expected: 0x{expected_modern_id:04X}",
                        f"got: 0x{dev_id:04X}",
                    ],
                )
            )

        # virtio-input is represented as one manifest entry but covers two subsystem IDs.
        if device_name == "virtio-input":
            contract_any = contract_ids["virtio-input (keyboard)"]
            expected_subsys = [
                contract_ids["virtio-input (keyboard)"].subsystem_device_id,
                contract_ids["virtio-input (mouse)"].subsystem_device_id,
            ]
        else:
            contract_any = contract_ids[device_name]
            expected_subsys = [contract_any.subsystem_device_id]

        if (vendor, dev_id) != (contract_any.vendor_id, contract_any.device_id):
            errors.append(
                format_error(
                    f"{device_name}: Vendor/Device ID mismatch between AERO-W7-VIRTIO and windows-device-contract.json:",
                    [
                        f"{W7_VIRTIO_CONTRACT_MD.as_posix()}: {contract_any.vendor_device_str()}",
                        f"{WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}: {vendor:04X}:{dev_id:04X}",
                    ],
                )
            )

        patterns = _require_str_list(entry, "hardware_id_patterns", device=device_name)
        base_hwid = f"PCI\\VEN_{contract_any.vendor_id:04X}&DEV_{contract_any.device_id:04X}"
        if not any(p.upper() == base_hwid.upper() for p in patterns):
            errors.append(
                format_error(
                    f"{device_name}: manifest is missing the base VEN/DEV hardware ID pattern:",
                    [
                        f"expected: {base_hwid}",
                        f"got: {patterns}",
                    ],
                )
            )

        transitional_patterns = [
            pat
            for pat in patterns
            if re.search(
                r"PCI\\VEN_1AF4&DEV_(?:10[0-3][0-9A-Fa-f]|100[0-9A-Fa-f])",
                pat,
                flags=re.I,
            )
        ]
        if transitional_patterns and not windows_contract_marks_transitional_ids_out_of_scope(windows_md):
            errors.append(
                format_error(
                    f"{device_name}: manifest contains transitional virtio-pci IDs, but docs do not explicitly mark them out-of-scope:",
                    [
                        f"patterns: {transitional_patterns}",
                        f"hint: add an explicit 'out of scope' compatibility note to {WINDOWS_DEVICE_CONTRACT_MD.as_posix()}",
                    ],
                )
            )

        for subsys_device in expected_subsys:
            if not _manifest_contains_subsys(patterns, contract_any.subsystem_vendor_id, subsys_device):
                errors.append(
                    format_error(
                        f"{device_name}: manifest is missing the expected SUBSYS-qualified hardware ID:",
                        [
                            f"expected SUBSYS: SUBSYS_{subsys_device:04X}{contract_any.subsystem_vendor_id:04X}",
                            f"got: {patterns}",
                        ],
                    )
                )

        # If any pattern revision-qualifies, it must match the contract major.
        has_rev_qualifier = False
        for pat in patterns:
            m = re.search(r"&REV_(?P<rev>[0-9A-Fa-f]{2})", pat)
            if not m:
                continue
            has_rev_qualifier = True
            rev = int(m.group("rev"), 16)
            if rev != contract_rev:
                errors.append(
                    format_error(
                        f"{device_name}: manifest REV_ qualifier mismatch:",
                        [
                            f"expected: REV_{contract_rev:02X}",
                            f"got: {pat}",
                        ],
                    )
                )

        # virtio_device_type must be stable.
        if entry.get("virtio_device_type") != expected_type:
            errors.append(
                format_error(
                    f"{device_name}: virtio_device_type mismatch in {WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}:",
                    [
                        f"expected: {expected_type}",
                        f"got: {entry.get('virtio_device_type')!r}",
                    ],
                )
            )

        if not has_rev_qualifier:
            errors.append(
                format_error(
                    f"{device_name}: manifest must include at least one revision-qualified hardware ID pattern:",
                    [
                        f"expected at least one pattern containing: &REV_{contract_rev:02X}",
                        f"got: {patterns}",
                    ],
                )
            )

    # ---------------------------------------------------------------------
    # 3) Feature bit guardrails: docs must agree on Win7 contract v1 ring features.
    # ---------------------------------------------------------------------
    contract_bits = parse_contract_feature_bits(w7_md, file=W7_VIRTIO_CONTRACT_MD)
    windows_bits = parse_windows_device_contract_feature_bits(windows_md, file=WINDOWS_DEVICE_CONTRACT_MD)
    for name, bit in contract_bits.items():
        if windows_bits.get(name) != bit:
            errors.append(
                format_error(
                    f"feature bit mismatch for {name}:",
                    [
                        f"{W7_VIRTIO_CONTRACT_MD.as_posix()}: bit {bit}",
                        f"{WINDOWS_DEVICE_CONTRACT_MD.as_posix()}: bit {windows_bits.get(name)}",
                    ],
                )
            )

    # Sanity-check the v1 invariants we care about.
    if contract_bits["VIRTIO_F_VERSION_1"] != 32:
        errors.append(
            f"{W7_VIRTIO_CONTRACT_MD.as_posix()}: expected VIRTIO_F_VERSION_1 to be bit 32, got {contract_bits['VIRTIO_F_VERSION_1']}"
        )
    if contract_bits["VIRTIO_F_RING_INDIRECT_DESC"] != 28:
        errors.append(
            f"{W7_VIRTIO_CONTRACT_MD.as_posix()}: expected VIRTIO_F_RING_INDIRECT_DESC to be bit 28, got {contract_bits['VIRTIO_F_RING_INDIRECT_DESC']}"
        )
    if contract_bits["VIRTIO_F_RING_EVENT_IDX"] != 29:
        errors.append(
            f"{W7_VIRTIO_CONTRACT_MD.as_posix()}: expected VIRTIO_F_RING_EVENT_IDX to be bit 29, got {contract_bits['VIRTIO_F_RING_EVENT_IDX']}"
        )

    # ---------------------------------------------------------------------
    # 4) Emulator conformance (lightweight source parsing; no builds).
    # ---------------------------------------------------------------------
    if AERO_DEVICES_PCI_PROFILE_RS.exists():
        profiles = parse_emulator_pci_profiles(AERO_DEVICES_PCI_PROFILE_RS)
        profile_map = {
            "virtio-blk": profiles["VIRTIO_BLK"],
            "virtio-net": profiles["VIRTIO_NET"],
            "virtio-snd": profiles["VIRTIO_SND"],
            "virtio-input (keyboard)": profiles["VIRTIO_INPUT_KEYBOARD"],
            "virtio-input (mouse)": profiles["VIRTIO_INPUT_MOUSE"],
        }
        for name, contract in contract_ids.items():
            prof = profile_map.get(name)
            if not prof:
                continue
            if (prof.vendor_id, prof.device_id) != (contract.vendor_id, contract.device_id):
                errors.append(
                    format_error(
                        f"{name}: emulator PCI profile Vendor/Device mismatch ({AERO_DEVICES_PCI_PROFILE_RS.as_posix()}):",
                        [
                            f"expected: {contract.vendor_device_str()}",
                            f"got: {prof.vendor_device_str()}",
                        ],
                    )
                )
            if (prof.subsystem_vendor_id, prof.subsystem_device_id) != (
                contract.subsystem_vendor_id,
                contract.subsystem_device_id,
            ):
                errors.append(
                    format_error(
                        f"{name}: emulator PCI profile subsystem mismatch ({AERO_DEVICES_PCI_PROFILE_RS.as_posix()}):",
                        [
                            f"expected: {contract.subsys_str()}",
                            f"got: {prof.subsys_str()}",
                        ],
                    )
                )
            if prof.revision_id != contract_rev:
                errors.append(
                    format_error(
                        f"{name}: emulator PCI profile revision mismatch ({AERO_DEVICES_PCI_PROFILE_RS.as_posix()}):",
                        [
                            f"expected: 0x{contract_rev:02X}",
                            f"got: 0x{prof.revision_id:02X}",
                        ],
                    )
                )

    # Queue sizes and ring feature bits cross-check against aero-virtio device models.
    aero_virtio_queues = parse_aero_virtio_queue_max_sizes()
    for device_name, contract_q in contract_queues.items():
        model_q = aero_virtio_queues.get(device_name)
        if not model_q:
            continue
        for idx, size in contract_q.items():
            if model_q.get(idx) != size:
                errors.append(
                    format_error(
                        f"{device_name}: virtqueue size mismatch between contract and aero-virtio device model:",
                        [
                            f"{W7_VIRTIO_CONTRACT_MD.as_posix()}: queue {idx} size {size}",
                            f"{AERO_VIRTIO_DEVICE_SOURCES[device_name].as_posix()}: queue {idx} size {model_q.get(idx)}",
                        ],
                    )
                )

    aero_virtio_features = parse_aero_virtio_device_feature_usage()
    for device_name, usage in aero_virtio_features.items():
        if not usage["has_version_1"] or not usage["has_indirect_desc"]:
            errors.append(
                format_error(
                    f"{device_name}: aero-virtio device_features is missing required Win7 contract v1 ring feature(s):",
                    [
                        f"expected: VIRTIO_F_VERSION_1 and VIRTIO_F_RING_INDIRECT_DESC to be present in {AERO_VIRTIO_DEVICE_SOURCES[device_name].as_posix()}",
                    ],
                )
            )
        if usage["mentions_event_idx"]:
            errors.append(
                format_error(
                    f"{device_name}: aero-virtio device_features mentions VIRTIO_F_RING_EVENT_IDX, but contract v1 forbids offering EVENT_IDX:",
                    [f"file: {AERO_VIRTIO_DEVICE_SOURCES[device_name].as_posix()}"],
                )
            )

    if errors:
        print("\n\n".join(errors), file=sys.stderr)
        raise SystemExit(1)


if __name__ == "__main__":
    main()
