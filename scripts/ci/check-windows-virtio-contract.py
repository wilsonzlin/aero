#!/usr/bin/env python3
"""
Validate Aero's Windows 7 virtio device contract wiring end-to-end.

This is a deterministic CI check that ensures the canonical device contract manifest
(`docs/windows-device-contract.json`) is consistent with:

  - Windows driver INFs (HWID matches, service names, and strict contract-v1 gating)
  - The emulator's canonical PCI profiles (`crates/devices/src/pci/profile.rs`)
  - Generated Guest Tools config (`guest-tools/config/devices.cmd`)

Run locally:
  python3 scripts/ci/check-windows-virtio-contract.py

Fix (regenerates only the derived Guest Tools devices.cmd file):
  python3 scripts/ci/check-windows-virtio-contract.py --fix
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import xml.etree.ElementTree as ET
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable, Optional


REPO_ROOT = Path(__file__).resolve().parents[2]

DEFAULT_CONTRACT_PATH = REPO_ROOT / "docs/windows-device-contract.json"
DEFAULT_PROFILE_RS_PATH = REPO_ROOT / "crates/devices/src/pci/profile.rs"

GUEST_TOOLS_DEVICES_CMD = REPO_ROOT / "guest-tools/config/devices.cmd"
GUEST_TOOLS_DEVICES_CMD_GENERATOR = REPO_ROOT / "scripts/ci/gen-guest-tools-devices-cmd.py"


PCI_HWID_RE = re.compile(
    r"^PCI\\VEN_(?P<ven>[0-9A-Fa-f]{4})&DEV_(?P<dev>[0-9A-Fa-f]{4})"
    r"(?:&SUBSYS_(?P<subsys>[0-9A-Fa-f]{8}))?"
    r"(?:&REV_(?P<rev>[0-9A-Fa-f]{2}))?$"
)


def _require_dict(value: Any, ctx: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise ValueError(f"Expected object for {ctx}, got {type(value).__name__}")
    return value


def _require_str(value: Any, ctx: str) -> str:
    if not isinstance(value, str) or not value:
        raise ValueError(f"Expected non-empty string for {ctx}, got {value!r}")
    return value


def _require_list(value: Any, ctx: str) -> list[Any]:
    if not isinstance(value, list):
        raise ValueError(f"Expected array for {ctx}, got {type(value).__name__}")
    return list(value)


def _parse_hex_u16(value: Any, ctx: str) -> int:
    s = _require_str(value, ctx=ctx)
    if not re.fullmatch(r"0x[0-9A-Fa-f]{1,4}", s):
        raise ValueError(f"{ctx}: expected 0x-prefixed u16 hex string, got {s!r}")
    n = int(s, 16)
    if not (0 <= n <= 0xFFFF):
        raise ValueError(f"{ctx}: out of range for u16: {s!r}")
    return n


def _parse_int(value: Any, ctx: str) -> int:
    if not isinstance(value, int):
        raise ValueError(f"{ctx}: expected integer, got {value!r}")
    return value


@dataclass(frozen=True)
class ParsedPciHwid:
    raw: str
    ven: int
    dev: int
    subsystem_vendor: Optional[int]
    subsystem_id: Optional[int]
    rev: Optional[int]


def _parse_pci_hwid(pattern: str, ctx: str) -> ParsedPciHwid:
    m = PCI_HWID_RE.fullmatch(pattern)
    if not m:
        raise ValueError(
            f"{ctx}: invalid PCI HWID pattern format: {pattern!r} (expected PCI\\\\VEN_xxxx&DEV_yyyy[&SUBSYS_zzzzzzzz][&REV_rr])"
        )
    ven = int(m.group("ven"), 16)
    dev = int(m.group("dev"), 16)
    subsys_raw = m.group("subsys")
    rev_raw = m.group("rev")
    subsystem_vendor: Optional[int] = None
    subsystem_id: Optional[int] = None
    if subsys_raw is not None:
        subsystem_id = int(subsys_raw[0:4], 16)
        subsystem_vendor = int(subsys_raw[4:8], 16)
    rev = int(rev_raw, 16) if rev_raw is not None else None
    return ParsedPciHwid(
        raw=pattern,
        ven=ven,
        dev=dev,
        subsystem_vendor=subsystem_vendor,
        subsystem_id=subsystem_id,
        rev=rev,
    )


def _strip_inf_comment(line: str) -> str:
    # INF comments start at ';' unless inside quotes.
    out_chars: list[str] = []
    in_quote = False
    for ch in line:
        if ch == '"':
            in_quote = not in_quote
        if ch == ";" and not in_quote:
            break
        out_chars.append(ch)
    return "".join(out_chars).rstrip("\r\n")


def _extract_inf_pci_hwids(inf_text: str) -> set[str]:
    hwids: set[str] = set()
    for raw_line in inf_text.splitlines():
        line = _strip_inf_comment(raw_line).strip()
        if not line:
            continue
        # Common model lines are comma-separated.
        for part in (p.strip() for p in line.split(",")):
            if part.upper().startswith("PCI\\VEN_"):
                hwids.add(part)
    return hwids


def _extract_inf_addservice_names(inf_text: str) -> set[str]:
    # We only need the service name token before the first comma.
    names: set[str] = set()
    for raw_line in inf_text.splitlines():
        line = _strip_inf_comment(raw_line).strip()
        if not line:
            continue
        if not line.lower().startswith("addservice"):
            continue
        # Accept both "AddService =" and "AddService=" forms.
        _, _, rest = line.partition("=")
        rest = rest.strip()
        if not rest:
            continue
        service_name = rest.split(",", 1)[0].strip()
        # INF syntax allows string values to be quoted; accept both:
        #   AddService = foo, ...
        #   AddService = "foo", ...
        if service_name.startswith('"') and service_name.endswith('"') and len(service_name) >= 2:
            service_name = service_name[1:-1].strip()
        if service_name:
            names.add(service_name)
    return names


def _extract_inf_ntmpdriver_values(inf_text: str) -> set[str]:
    values: set[str] = set()
    for raw_line in inf_text.splitlines():
        line = _strip_inf_comment(raw_line).strip()
        if not line:
            continue
        if "ntmpdriver" not in line.lower():
            continue
        parts = [p.strip() for p in line.split(",")]
        if len(parts) < 5:
            continue
        if parts[0].upper() != "HKR":
            continue
        value_name = parts[2].strip('"').upper()
        if value_name != "NTMPDRIVER":
            continue
        raw_value = parts[4].strip()
        raw_value = raw_value.strip('"')
        if raw_value:
            values.add(raw_value)
    return values


def _find_files_named(root: Path, file_name: str) -> list[Path]:
    if not root.exists():
        return []
    return sorted([p for p in root.rglob(file_name) if p.is_file()])


def _resolve_inf_path(
    *,
    inf_name: str,
    expected_vendor_id: int,
    expected_device_id: int,
    expected_service_name: str,
) -> Path:
    candidates: list[Path] = []
    for search_root in (REPO_ROOT / "drivers", REPO_ROOT / "windows-drivers"):
        candidates.extend(_find_files_named(search_root, inf_name))
    candidates = sorted(set(candidates))
    if not candidates:
        raise FileNotFoundError(f"INF not found in-tree: {inf_name!r}")

    matching: list[Path] = []
    for path in candidates:
        text = path.read_text(encoding="utf-8", errors="replace")
        hwids = _extract_inf_pci_hwids(text)
        ok = False
        for hwid in hwids:
            m = PCI_HWID_RE.match(hwid)
            if not m:
                continue
            if int(m.group("ven"), 16) == expected_vendor_id and int(m.group("dev"), 16) == expected_device_id:
                ok = True
                break
        if not ok:
            continue
        matching.append(path)

    if not matching:
        # We found the name but none appear to bind to the expected IDs.
        raise FileNotFoundError(
            f"INF {inf_name!r} exists, but none of the candidates bind to VEN_{expected_vendor_id:04X}&DEV_{expected_device_id:04X}.\n"
            f"Candidates:\n  - " + "\n  - ".join(p.as_posix() for p in candidates)
        )

    if len(matching) > 1:
        # Break ties by matching the contract service name.
        filtered: list[Path] = []
        for path in matching:
            text = path.read_text(encoding="utf-8", errors="replace")
            services = {s.lower() for s in _extract_inf_addservice_names(text)}
            if expected_service_name.lower() in services:
                filtered.append(path)
        if len(filtered) == 1:
            return filtered[0]
        matching = filtered or matching
        raise FileExistsError(
            f"INF name {inf_name!r} is ambiguous for VEN_{expected_vendor_id:04X}&DEV_{expected_device_id:04X}.\n"
            f"Matching candidates:\n  - " + "\n  - ".join(p.as_posix() for p in matching)
        )

    return matching[0]


def _find_vcxproj_for_inf(inf_path: Path) -> Optional[Path]:
    # Look for a vcxproj near the INF and confirm it references the INF file.
    inf_name = inf_path.name
    candidate_dirs: list[Path] = [inf_path.parent, *list(inf_path.parents)[:3]]
    vcxproj_candidates: list[Path] = []
    for d in candidate_dirs:
        for p in sorted(d.glob("*.vcxproj")):
            try:
                text = p.read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            if inf_name in text:
                vcxproj_candidates.append(p)
    vcxproj_candidates = sorted(set(vcxproj_candidates))
    if not vcxproj_candidates:
        return None
    if len(vcxproj_candidates) > 1:
        raise FileExistsError(
            f"Multiple vcxproj files reference {inf_path.as_posix()}:\n  - "
            + "\n  - ".join(p.as_posix() for p in vcxproj_candidates)
        )
    return vcxproj_candidates[0]


def _read_vcxproj_target_binary(vcxproj_path: Path) -> str:
    raw = vcxproj_path.read_text(encoding="utf-8", errors="replace")
    ns = {"m": "http://schemas.microsoft.com/developer/msbuild/2003"}
    root = ET.fromstring(raw)

    def collect(tag: str) -> list[tuple[str, str]]:
        vals: list[tuple[str, str]] = []
        for pg in root.findall("m:PropertyGroup", ns):
            condition = pg.attrib.get("Condition", "").strip()
            elem = pg.find(f"m:{tag}", ns)
            if elem is None or elem.text is None:
                continue
            val = elem.text.strip()
            if not val:
                continue
            vals.append((condition, val))
        return vals

    target_names = collect("TargetName")
    target_exts = collect("TargetExt")

    def pick_unconditional(pairs: list[tuple[str, str]]) -> Optional[str]:
        uncond = [v for (cond, v) in pairs if not cond]
        if len(uncond) == 1:
            return uncond[0]
        if len(uncond) > 1:
            raise ValueError(f"{vcxproj_path.as_posix()}: multiple unconditional values for {pairs!r}")
        return None

    target_name = pick_unconditional(target_names)
    if target_name is None:
        # If no unconditional TargetName is present, fall back to a non-Legacy one.
        non_legacy = [v for (cond, v) in target_names if "legacy" not in cond.lower()]
        if len(non_legacy) == 1:
            target_name = non_legacy[0]
        elif len(non_legacy) > 1:
            target_name = non_legacy[0]
        else:
            raise ValueError(f"{vcxproj_path.as_posix()}: could not determine TargetName")

    target_ext = pick_unconditional(target_exts) or ".sys"
    if not target_ext.startswith("."):
        target_ext = "." + target_ext
    return f"{target_name}{target_ext}"


def _run_guest_tools_devices_cmd_check(*, fix: bool) -> tuple[bool, str]:
    if not GUEST_TOOLS_DEVICES_CMD_GENERATOR.exists():
        return False, f"Missing generator wrapper: {GUEST_TOOLS_DEVICES_CMD_GENERATOR.as_posix()}"

    check_cmd = [sys.executable, str(GUEST_TOOLS_DEVICES_CMD_GENERATOR), "--check"]
    proc = subprocess.run(check_cmd, text=True, capture_output=True)
    if proc.returncode == 0:
        return True, ""

    if not fix:
        out = (proc.stdout or "") + (proc.stderr or "")
        return False, out.strip() or "guest-tools/config/devices.cmd is out of date"

    # Fix: regenerate, then re-check so we still fail for non-generation issues.
    regen = subprocess.run([sys.executable, str(GUEST_TOOLS_DEVICES_CMD_GENERATOR)], text=True, capture_output=True)
    if regen.returncode != 0:
        out = (regen.stdout or "") + (regen.stderr or "")
        return False, out.strip() or "failed to regenerate guest-tools/config/devices.cmd"

    proc2 = subprocess.run(check_cmd, text=True, capture_output=True)
    if proc2.returncode != 0:
        out = (proc2.stdout or "") + (proc2.stderr or "")
        return False, out.strip() or "guest-tools/config/devices.cmd still out of date after regeneration"

    return True, ""


def _parse_rust_int(expr: str, constants: dict[str, int], ctx: str) -> int:
    expr = expr.strip()
    if expr in constants:
        return constants[expr]
    if re.fullmatch(r"0x[0-9a-fA-F_]+", expr):
        return int(expr.replace("_", ""), 16)
    if re.fullmatch(r"[0-9_]+", expr):
        return int(expr.replace("_", ""), 10)
    raise ValueError(f"{ctx}: unsupported Rust integer expression: {expr!r}")


def _extract_rust_const_ints(profile_rs_text: str) -> dict[str, int]:
    # Very small, intentionally limited "parser" for this file:
    # only handles `pub const NAME: u16 = 0x...;`-style definitions used for IDs.
    consts: dict[str, int] = {}
    const_re = re.compile(
        r"^\s*pub const (?P<name>[A-Z0-9_]+):\s*u(?P<bits>8|16|32|64)\s*=\s*(?P<value>0x[0-9a-fA-F_]+|[0-9_]+)\s*;\s*$"
    )
    for line in profile_rs_text.splitlines():
        m = const_re.match(line)
        if not m:
            continue
        name = m.group("name")
        value = m.group("value")
        consts[name] = int(value.replace("_", ""), 0)
    return consts


def _extract_rust_block(text: str, *, needle: str, ctx: str) -> str:
    idx = text.find(needle)
    if idx < 0:
        raise ValueError(f"{ctx}: missing block: {needle!r}")
    brace_idx = text.find("{", idx)
    if brace_idx < 0:
        raise ValueError(f"{ctx}: missing '{{' after {needle!r}")
    depth = 0
    for i in range(brace_idx, len(text)):
        if text[i] == "{":
            depth += 1
        elif text[i] == "}":
            depth -= 1
            if depth == 0:
                return text[brace_idx + 1 : i]
    raise ValueError(f"{ctx}: unterminated '{{' block for {needle!r}")


@dataclass(frozen=True)
class PciProfileKey:
    vendor_id: int
    device_id: int
    subsystem_vendor_id: int
    subsystem_id: int
    revision_id: int


@dataclass(frozen=True)
class ParsedPciDeviceProfile:
    const_name: str
    name: str
    key: PciProfileKey
    bars_ref: str
    caps_ref: str


def _parse_pci_device_profile(
    *,
    text: str,
    const_name: str,
    constants: dict[str, int],
) -> ParsedPciDeviceProfile:
    needle = f"pub const {const_name}: PciDeviceProfile = PciDeviceProfile"
    block = _extract_rust_block(text, needle=needle, ctx=DEFAULT_PROFILE_RS_PATH.as_posix())

    def field(name: str) -> str:
        m = re.search(rf"\b{name}\s*:\s*(?P<expr>[^,]+),", block)
        if not m:
            raise ValueError(f"{DEFAULT_PROFILE_RS_PATH.as_posix()}:{const_name}: missing field {name!r}")
        return m.group("expr").strip()

    name_expr = field("name")
    m_name = re.fullmatch(r"\"(?P<s>.*)\"", name_expr)
    if not m_name:
        raise ValueError(f"{DEFAULT_PROFILE_RS_PATH.as_posix()}:{const_name}: unsupported name expr: {name_expr!r}")

    vendor_id = _parse_rust_int(field("vendor_id"), constants, ctx=f"{const_name}.vendor_id")
    device_id = _parse_rust_int(field("device_id"), constants, ctx=f"{const_name}.device_id")
    subsystem_vendor_id = _parse_rust_int(
        field("subsystem_vendor_id"), constants, ctx=f"{const_name}.subsystem_vendor_id"
    )
    subsystem_id = _parse_rust_int(field("subsystem_id"), constants, ctx=f"{const_name}.subsystem_id")
    revision_id = _parse_rust_int(field("revision_id"), constants, ctx=f"{const_name}.revision_id")

    bars_ref = field("bars")
    caps_ref = field("capabilities")

    return ParsedPciDeviceProfile(
        const_name=const_name,
        name=m_name.group("s"),
        key=PciProfileKey(
            vendor_id=vendor_id,
            device_id=device_id,
            subsystem_vendor_id=subsystem_vendor_id,
            subsystem_id=subsystem_id,
            revision_id=revision_id,
        ),
        bars_ref=bars_ref,
        caps_ref=caps_ref,
    )


def _parse_rust_u8_array(text: str, const_name: str) -> list[int]:
    # Matches:
    #   pub const VIRTIO_CAP_COMMON: [u8; 14] = [
    #       16, 1, ...
    #   ];
    pattern = re.compile(
        rf"pub const {re.escape(const_name)}:\s*\[u8;\s*\d+\]\s*=\s*\[(?P<body>.*?)\];",
        re.DOTALL,
    )
    m = pattern.search(text)
    if not m:
        raise ValueError(f"{DEFAULT_PROFILE_RS_PATH.as_posix()}: missing u8 array const {const_name}")
    body = m.group("body")
    values: list[int] = []
    for token in body.replace("\n", " ").split(","):
        tok = token.strip()
        if not tok:
            continue
        values.append(int(tok, 0))
    return values


def _le_u32(bytes4: Iterable[int]) -> int:
    b = list(bytes4)
    if len(b) != 4:
        raise ValueError("expected 4 bytes")
    return b[0] | (b[1] << 8) | (b[2] << 16) | (b[3] << 24)


@dataclass(frozen=True)
class DecodedVirtioCap:
    cfg_type: int
    bar: int
    offset: int
    length: int
    notify_off_multiplier: Optional[int]


def _decode_virtio_vendor_cap(payload: list[int], ctx: str) -> DecodedVirtioCap:
    # virtio_pci_cap minus cap_vndr/cap_next (those are emitted by VendorSpecificCapability).
    if len(payload) < 14:
        raise ValueError(f"{ctx}: payload too short ({len(payload)} bytes)")
    cap_len = payload[0]
    expected_total_len = 2 + len(payload)
    if cap_len != expected_total_len:
        raise ValueError(f"{ctx}: cap_len={cap_len} but payload implies total len {expected_total_len}")
    cfg_type = payload[1]
    bar = payload[2]
    # payload[3] = id (unused), payload[4..6] = padding/reserved.
    offset = _le_u32(payload[6:10])
    length = _le_u32(payload[10:14])
    notify_mult: Optional[int] = None
    if len(payload) >= 18:
        notify_mult = _le_u32(payload[14:18])
    return DecodedVirtioCap(cfg_type=cfg_type, bar=bar, offset=offset, length=length, notify_off_multiplier=notify_mult)


def _check_profiles_against_contract(
    *,
    contract_devices: dict[str, "ContractDevice"],
    errors: list[str],
) -> None:
    profile_path = DEFAULT_PROFILE_RS_PATH
    try:
        text = profile_path.read_text(encoding="utf-8", errors="replace")
    except OSError as e:
        errors.append(f"profile.rs: failed to read {profile_path.as_posix()}: {e}")
        return

    constants = _extract_rust_const_ints(text)

    # Validate the shared virtio BAR + caps match the contract v1 fixed layout.
    virtio_bars_re = re.compile(
        r"pub const VIRTIO_BARS:\s*\[PciBarProfile;\s*\d+\]\s*=\s*\[\s*PciBarProfile::mem(?P<bits>32|64)\(\s*(?P<index>\d+)\s*,\s*(?P<size>0x[0-9a-fA-F_]+|\d+)\s*,\s*(?P<pref>true|false)\s*\)\s*\];"
    )
    m = virtio_bars_re.search(text)
    if not m:
        errors.append(f"profile.rs: missing/unsupported VIRTIO_BARS definition in {profile_path.as_posix()}")
    else:
        bar_bits = int(m.group("bits"))
        bar_index = int(m.group("index"))
        bar_size = int(m.group("size").replace("_", ""), 0)
        prefetch = m.group("pref") == "true"
        if bar_bits != 64:
            errors.append(
                f"profile.rs: VIRTIO_BARS expected mem64 BAR0 size 0x4000 prefetchable=false, got mem{bar_bits}"
            )
        if bar_index != 0 or bar_size != 0x4000 or prefetch:
            errors.append(
                f"profile.rs: VIRTIO_BARS expected BAR0 size 0x4000 prefetchable=false, got index={bar_index} size=0x{bar_size:X} prefetchable={prefetch}"
            )

    try:
        caps = {
            "COMMON": _decode_virtio_vendor_cap(_parse_rust_u8_array(text, "VIRTIO_CAP_COMMON"), "VIRTIO_CAP_COMMON"),
            "NOTIFY": _decode_virtio_vendor_cap(_parse_rust_u8_array(text, "VIRTIO_CAP_NOTIFY"), "VIRTIO_CAP_NOTIFY"),
            "ISR": _decode_virtio_vendor_cap(_parse_rust_u8_array(text, "VIRTIO_CAP_ISR"), "VIRTIO_CAP_ISR"),
            "DEVICE": _decode_virtio_vendor_cap(_parse_rust_u8_array(text, "VIRTIO_CAP_DEVICE"), "VIRTIO_CAP_DEVICE"),
        }
    except ValueError as e:
        errors.append(f"profile.rs: {e}")
        caps = {}

    expected_caps = {
        "COMMON": (1, 0x0000, 0x0100, None),
        "NOTIFY": (2, 0x1000, 0x0100, 4),
        "ISR": (3, 0x2000, 0x0020, None),
        "DEVICE": (4, 0x3000, 0x0100, None),
    }
    for name, exp in expected_caps.items():
        if name not in caps:
            continue
        cap = caps[name]
        cfg_type, offset, length, mult = exp
        if cap.cfg_type != cfg_type or cap.bar != 0 or cap.offset != offset or cap.length != length or cap.notify_off_multiplier != mult:
            errors.append(
                "profile.rs: "
                + f"{name} cap mismatch: expected cfg_type={cfg_type} bar=0 offset=0x{offset:04X} len=0x{length:X} mult={mult}, "
                + f"got cfg_type={cap.cfg_type} bar={cap.bar} offset=0x{cap.offset:04X} len=0x{cap.length:X} mult={cap.notify_off_multiplier}"
            )

    # Gather canonical profile names.
    canonical_re = re.compile(
        r"pub const CANONICAL_IO_DEVICES:\s*&\[\s*PciDeviceProfile\s*\]\s*=\s*&\[\s*(?P<body>.*?)\s*\];",
        re.DOTALL,
    )
    m_canon = canonical_re.search(text)
    if not m_canon:
        errors.append(f"profile.rs: missing CANONICAL_IO_DEVICES list in {profile_path.as_posix()}")
        return

    canonical_names: list[str] = []
    for raw_line in m_canon.group("body").splitlines():
        line = raw_line.split("//", 1)[0].strip()
        if not line:
            continue
        if not line.endswith(","):
            errors.append(
                f"profile.rs: unsupported CANONICAL_IO_DEVICES formatting (expected trailing comma): {raw_line!r}"
            )
            continue
        canonical_names.append(line[:-1].strip())

    # Parse the canonical profiles.
    parsed_profiles: list[ParsedPciDeviceProfile] = []
    for const_name in canonical_names:
        if const_name == "VIRTIO_INPUT":
            # Deprecated alias; canonical list currently uses *_KEYBOARD/_MOUSE.
            continue
        if f"pub const {const_name}: PciDeviceProfile" not in text:
            # Non-PciDeviceProfile entries might sneak in; ignore.
            continue
        try:
            parsed_profiles.append(_parse_pci_device_profile(text=text, const_name=const_name, constants=constants))
        except ValueError as e:
            errors.append(str(e))

    profiles_by_key: dict[PciProfileKey, ParsedPciDeviceProfile] = {p.key: p for p in parsed_profiles}

    # Enforce that virtio devices use the shared bars/caps and contract v1 revision ID.
    for p in parsed_profiles:
        if p.key.vendor_id != 0x1AF4:
            continue
        if p.key.revision_id != 1:
            errors.append(f"profile.rs:{p.const_name}: virtio profiles must use revision_id=1 (contract v1), got {p.key.revision_id}")
        if p.bars_ref != "&VIRTIO_BARS":
            errors.append(f"profile.rs:{p.const_name}: virtio profiles must use bars: &VIRTIO_BARS, got {p.bars_ref!r}")
        if p.caps_ref != "&VIRTIO_CAPS":
            errors.append(
                f"profile.rs:{p.const_name}: virtio profiles must use capabilities: &VIRTIO_CAPS, got {p.caps_ref!r}"
            )

    # Compute expected PCI identities from the contract.
    for device_name, dev in sorted(contract_devices.items(), key=lambda kv: kv[0]):
        if not dev.is_virtio:
            continue

        expected_pairs: set[tuple[int, int]] = set()
        for hwid in dev.parsed_hwids:
            if hwid.subsystem_id is not None and hwid.subsystem_vendor is not None and hwid.rev == 0x01:
                expected_pairs.add((hwid.subsystem_vendor, hwid.subsystem_id))

        if not expected_pairs:
            errors.append(f"[{device_name}] contract JSON must include at least one SUBSYS_...&REV_01 hardware_id_patterns entry")
            continue

        for (subsys_vendor, subsys_id) in sorted(expected_pairs):
            key = PciProfileKey(
                vendor_id=dev.pci_vendor_id,
                device_id=dev.pci_device_id,
                subsystem_vendor_id=subsys_vendor,
                subsystem_id=subsys_id,
                revision_id=1,
            )
            if key not in profiles_by_key:
                errors.append(
                    f"[{device_name}] missing canonical PCI profile for "
                    f"{dev.pci_vendor_id:04X}:{dev.pci_device_id:04X} SUBSYS {subsys_id:04X}{subsys_vendor:04X} REV_01 in {profile_path.as_posix()}"
                )


@dataclass
class ContractDevice:
    name: str
    pci_vendor_id: int
    pci_device_id: int
    pci_device_id_transitional: Optional[int]
    hardware_id_patterns: list[str]
    driver_service_name: str
    inf_name: str
    virtio_device_type: Optional[int]
    parsed_hwids: list[ParsedPciHwid]

    @property
    def is_virtio(self) -> bool:
        return self.name.startswith("virtio-")


def _load_contract(contract_path: Path) -> dict[str, ContractDevice]:
    data = json.loads(contract_path.read_text(encoding="utf-8"))
    root = _require_dict(data, ctx=contract_path.as_posix())
    devices_value = _require_list(root.get("devices"), ctx=f"{contract_path.as_posix()}: devices")

    devices: dict[str, ContractDevice] = {}
    for idx, entry_any in enumerate(devices_value):
        entry = _require_dict(entry_any, ctx=f"devices[{idx}]")
        name = _require_str(entry.get("device"), ctx=f"devices[{idx}].device")
        if name in devices:
            raise ValueError(f"{contract_path.as_posix()}: duplicate device entry: {name!r}")

        vendor_id = _parse_hex_u16(entry.get("pci_vendor_id"), ctx=f"[{name}].pci_vendor_id")
        device_id = _parse_hex_u16(entry.get("pci_device_id"), ctx=f"[{name}].pci_device_id")
        pci_device_id_transitional: Optional[int] = None
        if "pci_device_id_transitional" in entry:
            pci_device_id_transitional = _parse_hex_u16(
                entry.get("pci_device_id_transitional"),
                ctx=f"[{name}].pci_device_id_transitional",
            )

        hwids_raw = entry.get("hardware_id_patterns")
        hwids_list = _require_list(hwids_raw, ctx=f"[{name}].hardware_id_patterns")
        hwids: list[str] = []
        for j, h in enumerate(hwids_list):
            hwids.append(_require_str(h, ctx=f"[{name}].hardware_id_patterns[{j}]"))

        driver_service_name = _require_str(entry.get("driver_service_name"), ctx=f"[{name}].driver_service_name")
        inf_name = _require_str(entry.get("inf_name"), ctx=f"[{name}].inf_name")

        virtio_device_type: Optional[int] = None
        if name.startswith("virtio-"):
            virtio_device_type = _parse_int(entry.get("virtio_device_type"), ctx=f"[{name}].virtio_device_type")

        parsed_hwids: list[ParsedPciHwid] = []
        for j, hwid in enumerate(hwids):
            parsed_hwids.append(_parse_pci_hwid(hwid, ctx=f"[{name}].hardware_id_patterns[{j}]"))

        devices[name] = ContractDevice(
            name=name,
            pci_vendor_id=vendor_id,
            pci_device_id=device_id,
            pci_device_id_transitional=pci_device_id_transitional,
            hardware_id_patterns=hwids,
            driver_service_name=driver_service_name,
            inf_name=inf_name,
            virtio_device_type=virtio_device_type,
            parsed_hwids=parsed_hwids,
        )

    return devices


def _validate_contract_devices(devices: dict[str, ContractDevice], errors: list[str]) -> None:
    for name, dev in sorted(devices.items(), key=lambda kv: kv[0]):
        # Validate JSON device IDs and HWID patterns agree.
        expected_prefix = f"PCI\\VEN_{dev.pci_vendor_id:04X}&DEV_{dev.pci_device_id:04X}"

        for hwid in dev.parsed_hwids:
            if hwid.ven != dev.pci_vendor_id or hwid.dev != dev.pci_device_id:
                errors.append(
                    f"[{name}] hardware_id_patterns contains VEN_{hwid.ven:04X}&DEV_{hwid.dev:04X}, "
                    f"but device entry declares VEN_{dev.pci_vendor_id:04X}&DEV_{dev.pci_device_id:04X}: {hwid.raw!r}"
                )
            if not hwid.raw.upper().startswith(expected_prefix.upper()):
                errors.append(
                    f"[{name}] invalid HWID pattern (expected to start with {expected_prefix!r}): {hwid.raw!r}"
                )

        if dev.is_virtio:
            if dev.pci_vendor_id != 0x1AF4:
                errors.append(f"[{name}] virtio device must use pci_vendor_id=0x1AF4, got 0x{dev.pci_vendor_id:04X}")
            if dev.virtio_device_type is None:
                errors.append(f"[{name}] virtio device is missing virtio_device_type")
                continue

            expected_device_id = 0x1040 + dev.virtio_device_type
            if dev.pci_device_id != expected_device_id:
                errors.append(
                    f"[{name}] pci_device_id mismatch for virtio modern ID space: expected 0x{expected_device_id:04X} "
                    f"(0x1040 + virtio_device_type={dev.virtio_device_type}), got 0x{dev.pci_device_id:04X}"
                )

            # Transitional virtio-pci device IDs are out of scope for the contract v1 binding rules
            # (modern-only + REV_01), but we still record the corresponding ID as metadata for
            # documentation and for derived tooling.
            expected_transitional = 0x1000 + (dev.virtio_device_type - 1)
            if dev.pci_device_id_transitional is None:
                errors.append(
                    f"[{name}] missing pci_device_id_transitional (expected 0x{expected_transitional:04X} = 0x1000 + (virtio_device_type - 1))"
                )
            elif dev.pci_device_id_transitional != expected_transitional:
                errors.append(
                    f"[{name}] pci_device_id_transitional mismatch: expected 0x{expected_transitional:04X} "
                    f"(0x1000 + (virtio_device_type={dev.virtio_device_type} - 1)), got 0x{dev.pci_device_id_transitional:04X}"
                )

            short = expected_prefix
            rev = f"{expected_prefix}&REV_01"
            patterns_upper = {p.upper() for p in dev.hardware_id_patterns}
            if short.upper() not in patterns_upper:
                errors.append(f"[{name}] hardware_id_patterns is missing short form HWID: {short!r}")
            if rev.upper() not in patterns_upper:
                errors.append(f"[{name}] hardware_id_patterns is missing contract-v1 REV_01 HWID: {rev!r}")
        else:
            if dev.pci_device_id_transitional is not None:
                errors.append(
                    f"[{name}] pci_device_id_transitional is only valid for virtio-* devices (found: 0x{dev.pci_device_id_transitional:04X})"
                )


def _validate_inf_bindings(devices: dict[str, ContractDevice], errors: list[str]) -> None:
    for name, dev in sorted(devices.items(), key=lambda kv: kv[0]):
        try:
            inf_path = _resolve_inf_path(
                inf_name=dev.inf_name,
                expected_vendor_id=dev.pci_vendor_id,
                expected_device_id=dev.pci_device_id,
                expected_service_name=dev.driver_service_name,
            )
        except (OSError, ValueError) as e:
            errors.append(f"[{name}] {e}")
            continue

        inf_text = inf_path.read_text(encoding="utf-8", errors="replace")
        inf_hwids = _extract_inf_pci_hwids(inf_text)
        if not inf_hwids:
            errors.append(f"[{name}] INF has no active PCI HWID matches: {inf_path.as_posix()}")
            continue

        contract_hwids_upper = {h.upper() for h in dev.hardware_id_patterns}
        for hwid in sorted(inf_hwids):
            hwid_upper = hwid.upper()
            if hwid_upper not in contract_hwids_upper:
                errors.append(
                    f"[{name}] INF matches HWID not present in docs/windows-device-contract.json.hardware_id_patterns:\n"
                    f"  INF: {inf_path.as_posix()}\n"
                    f"  HWID: {hwid!r}"
                )
                continue

            try:
                parsed = _parse_pci_hwid(hwid, ctx=f"{inf_path.as_posix()}")
            except ValueError as e:
                errors.append(f"[{name}] {e}")
                continue

            if parsed.ven != dev.pci_vendor_id or parsed.dev != dev.pci_device_id:
                errors.append(
                    f"[{name}] INF binds {parsed.ven:04X}:{parsed.dev:04X} but contract declares {dev.pci_vendor_id:04X}:{dev.pci_device_id:04X}:\n"
                    f"  INF: {inf_path.as_posix()}\n"
                    f"  HWID: {hwid!r}"
                )

            if dev.is_virtio and dev.virtio_device_type is not None:
                transitional = 0x1000 + (dev.virtio_device_type - 1)
                if parsed.dev == transitional:
                    errors.append(
                        f"[{name}] INF must not match transitional virtio-pci device ID DEV_{transitional:04X}:\n"
                        f"  INF: {inf_path.as_posix()}\n"
                        f"  HWID: {hwid!r}"
                    )
                if parsed.rev != 0x01:
                    errors.append(
                        f"[{name}] INF must gate contract-v1 binding by PCI revision (REV_01):\n"
                        f"  INF: {inf_path.as_posix()}\n"
                        f"  HWID: {hwid!r}\n"
                        f"  Expected: include '&REV_01' and avoid rev-less matches"
                    )

        # Validate AddService matches the contract service name.
        addservice_names = {s.lower() for s in _extract_inf_addservice_names(inf_text)}
        if dev.driver_service_name.lower() not in addservice_names:
            errors.append(
                f"[{name}] INF does not install the expected service {dev.driver_service_name!r} via AddService:\n"
                f"  INF: {inf_path.as_posix()}\n"
                f"  Found: {sorted(addservice_names) if addservice_names else '(none)'}"
            )

        # If the INF uses NTMPDriver, ensure it matches the MSBuild project's target binary.
        ntmp_values = _extract_inf_ntmpdriver_values(inf_text)
        if ntmp_values:
            try:
                vcxproj = _find_vcxproj_for_inf(inf_path)
                if vcxproj is None:
                    errors.append(
                        f"[{name}] INF sets NTMPDriver but no referencing .vcxproj was found near {inf_path.as_posix()}"
                    )
                else:
                    expected_bin = _read_vcxproj_target_binary(vcxproj)
                    for v in sorted(ntmp_values):
                        if v.lower() != expected_bin.lower():
                            errors.append(
                                f"[{name}] NTMPDriver mismatch:\n"
                                f"  INF: {inf_path.as_posix()}\n"
                                f"  vcxproj: {vcxproj.as_posix()}\n"
                                f"  Expected: {expected_bin!r}\n"
                                f"  Found: {v!r}"
                            )
            except (ValueError, FileExistsError) as e:
                errors.append(f"[{name}] {e}")


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--contract",
        type=Path,
        default=DEFAULT_CONTRACT_PATH,
        help="Path to docs/windows-device-contract.json (default: repo copy).",
    )
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--check", action="store_true", help="Check only (default).")
    mode.add_argument(
        "--fix",
        action="store_true",
        help="Regenerate derived artifacts (currently only guest-tools/config/devices.cmd) before checking.",
    )
    args = parser.parse_args(argv)

    errors: list[str] = []

    try:
        devices = _load_contract(args.contract)
    except (OSError, ValueError, json.JSONDecodeError) as e:
        print(f"error: failed to load contract {args.contract.as_posix()}: {e}", file=sys.stderr)
        return 2

    _validate_contract_devices(devices, errors)
    _validate_inf_bindings(devices, errors)
    _check_profiles_against_contract(contract_devices=devices, errors=errors)

    ok, guest_tools_msg = _run_guest_tools_devices_cmd_check(fix=args.fix)
    if not ok:
        errors.append("guest-tools/config/devices.cmd: " + guest_tools_msg)

    if errors:
        for e in errors:
            print(f"error: {e}", file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
