#!/usr/bin/env python3
"""
Guardrail: prevent drift between the definitive Windows 7 virtio contract
(`docs/windows7-virtio-driver-contract.md`, Contract ID: AERO-W7-VIRTIO) and the
Windows device/driver binding docs/manifest (`docs/windows-device-contract.*`),
and ensure the canonical in-tree Windows 7 virtio driver INFs remain aligned with
the contract identity policy (modern IDs + revision gating).

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
WINDOWS_DEVICE_CONTRACT_VIRTIO_WIN_JSON = REPO_ROOT / "docs/windows-device-contract-virtio-win.json"

# The Windows 7 virtio test harness docs/log strings should always reference the
# canonical INF basenames shipped by this repo. When we rename driver packages,
# it's easy to update the INFs but forget to update these examples.
WIN7_VIRTIO_TESTS_ROOT = REPO_ROOT / "drivers/windows7/tests"
INSTRUCTIONS_ROOT = REPO_ROOT / "instructions"
DEPRECATED_WIN7_TEST_INF_BASENAMES: tuple[str, ...] = (
    # Pre-rename INF basenames.
    "aerovblk.inf",
    "aerovnet.inf",
    # Old virtio-snd INF basename (hyphenated); canonical is now aero_virtio_snd.inf.
    "aero-virtio-snd.inf",
    # Old virtio-input INF basename; canonical is now aero_virtio_input.inf.
    "virtio-input.inf",
)

# Guest Tools packager specs that hardcode expected HWID regexes. These should
# stay aligned with the canonical Windows device contract; otherwise Guest Tools
# builds can silently start rejecting/accepting the wrong drivers.
AERO_GUEST_TOOLS_PACKAGING_SPECS: tuple[Path, ...] = (
    REPO_ROOT / "tools/packaging/specs/win7-aero-guest-tools.json",
    REPO_ROOT / "tools/packaging/specs/win7-aero-virtio.json",
    REPO_ROOT / "tools/packaging/specs/win7-virtio-win.json",
    REPO_ROOT / "tools/packaging/specs/win7-virtio-full.json",
)

PACKAGING_SPEC_DRIVER_TO_CONTRACT_DEVICE: Mapping[str, str] = {
    # Canonical in-repo driver directory names.
    "virtio-blk": "virtio-blk",
    "virtio-net": "virtio-net",
    "virtio-input": "virtio-input",
    "virtio-snd": "virtio-snd",
    # In-tree driver/service names.
    "aerovblk": "virtio-blk",
    "aerovnet": "virtio-net",
    "virtioinput": "virtio-input",
    "aeroviosnd": "virtio-snd",
    "aero_virtio_blk": "virtio-blk",
    "aero_virtio_net": "virtio-net",
    "aero_virtio_input": "virtio-input",
    "aero_virtio_snd": "virtio-snd",
    # virtio-win payload driver directory names.
    "viostor": "virtio-blk",
    "netkvm": "virtio-net",
    "vioinput": "virtio-input",
    "viosnd": "virtio-snd",
    # Non-virtio driver directory names.
    "aerogpu": "aero-gpu",
}

# Canonical in-tree Win7 driver INFs that are expected to follow AERO-W7-VIRTIO v1
# identity policy (modern IDs + contract major version encoded in PCI Revision ID).
WIN7_VIRTIO_DRIVER_INFS: Mapping[str, Path] = {
    "virtio-blk": REPO_ROOT / "drivers/windows7/virtio-blk/inf/aero_virtio_blk.inf",
    "virtio-net": REPO_ROOT / "drivers/windows7/virtio-net/inf/aero_virtio_net.inf",
    "virtio-snd": REPO_ROOT / "drivers/windows7/virtio-snd/inf/aero_virtio_snd.inf",
    "virtio-input": REPO_ROOT / "drivers/windows7/virtio-input/inf/aero_virtio_input.inf",
}

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

SUPPORTED_VIRTIO_DEVICES = tuple(VIRTIO_DEVICE_TYPE_IDS.keys())

AERO_DEVICES_PCI_PROFILE_RS = REPO_ROOT / "crates/devices/src/pci/profile.rs"
AERO_VIRTIO_DEVICE_SOURCES: Mapping[str, Path] = {
    "virtio-blk": REPO_ROOT / "crates/aero-virtio/src/devices/blk.rs",
    "virtio-net": REPO_ROOT / "crates/aero-virtio/src/devices/net.rs",
    "virtio-input": REPO_ROOT / "crates/aero-virtio/src/devices/input.rs",
    "virtio-snd": REPO_ROOT / "crates/aero-virtio/src/devices/snd.rs",
}

# Canonical in-tree Windows driver INFs for the Aero Win7 virtio contract.
# Keeping these in sync prevents "driver installs but doesn't bind" regressions.
AERO_VIRTIO_INF_SOURCES: Mapping[str, Path] = {
    "virtio-blk": REPO_ROOT / "drivers/windows7/virtio-blk/inf/aero_virtio_blk.inf",
    "virtio-net": REPO_ROOT / "drivers/windows7/virtio-net/inf/aero_virtio_net.inf",
    "virtio-snd": REPO_ROOT / "drivers/windows7/virtio-snd/inf/aero_virtio_snd.inf",
    "virtio-input": REPO_ROOT / "drivers/windows7/virtio-input/inf/aero_virtio_input.inf",
}

AERO_VIRTIO_PCI_IDENTITY_HEADER = REPO_ROOT / "drivers/win7/virtio/virtio-core/portable/virtio_pci_identity.h"
AERO_VIRTIO_BLK_DRIVER_HEADER = REPO_ROOT / "drivers/windows7/virtio-blk/include/aero_virtio_blk.h"
AERO_VIRTIO_NET_DRIVER_HEADER = REPO_ROOT / "drivers/windows7/virtio-net/include/aero_virtio_net.h"
AERO_VIRTIO_INPUT_DRIVER_HEADER = REPO_ROOT / "drivers/windows7/virtio-input/src/virtio_input.h"
AERO_VIRTIO_PCI_MODERN_TRANSPORT_H = REPO_ROOT / "drivers/windows/virtio/pci-modern/virtio_pci_modern_transport.h"
AERO_VIRTIO_PCI_MODERN_TRANSPORT_C = REPO_ROOT / "drivers/windows/virtio/pci-modern/virtio_pci_modern_transport.c"


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


def strip_inf_comment_lines(text: str) -> str:
    """Remove full-line INF comments (`; ...`) from `text`."""

    return "\n".join(line for line in text.splitlines() if not line.lstrip().startswith(";"))


def parse_contract_major_version(md: str) -> int:
    m = re.search(r"^\*\*Contract version:\*\*\s*`(?P<major>\d+)\.", md, flags=re.M)
    if not m:
        fail(f"could not parse contract major version from {W7_VIRTIO_CONTRACT_MD.as_posix()}")
    return int(m.group("major"), 10)


def scan_text_tree_for_substrings(root: Path, needles: Iterable[str]) -> list[str]:
    hits: list[str] = []
    needles = tuple(needles)
    for path in sorted(root.rglob("*")):
        if not path.is_file():
            continue
        try:
            text = path.read_text(encoding="utf-8", errors="replace")
        except OSError:
            continue
        if not any(needle in text for needle in needles):
            continue
        for line_no, line in enumerate(text.splitlines(), start=1):
            for needle in needles:
                if needle in line:
                    hits.append(f"{path.as_posix()}:{line_no}: contains {needle!r}")
    return hits


def parse_c_define_hex(text: str, name: str, *, file: Path) -> int | None:
    m = re.search(rf"^\s*#define\s+{re.escape(name)}\s+(?P<hex>0x[0-9A-Fa-f]+)", text, flags=re.M)
    if not m:
        return None
    try:
        return int(m.group("hex"), 16)
    except ValueError:
        fail(f"{file.as_posix()}: invalid hex literal in #define {name}")


@dataclass(frozen=True)
class VirtioPciModernLayout:
    bar0_required_size: int
    common_offset: int
    common_len: int
    notify_offset: int
    notify_len: int
    isr_offset: int
    isr_len: int
    device_offset: int
    device_len: int
    notify_off_multiplier: int


def parse_contract_fixed_mmio_layout(md: str) -> VirtioPciModernLayout:
    # BAR0 size is described in §1.2.
    m = re.search(r"^\s*-\s+\*\*BAR0:\*\*.*?size\s+\*\*(?P<size>0x[0-9A-Fa-f]+)\s+bytes\*\*", md, flags=re.M)
    if not m:
        fail(f"{W7_VIRTIO_CONTRACT_MD.as_posix()}: could not parse BAR0 size in §1.2")
    bar0_size = int(m.group("size"), 16)

    # Offsets/lengths are fixed in §1.4.
    section = _extract_section(
        md,
        start=r"^###\s+1\.4\s+Fixed MMIO layout used by all Aero virtio devices",
        end=r"^###\s+1\.5\s+",
        file=W7_VIRTIO_CONTRACT_MD,
        what="section 1.4 (fixed MMIO layout)",
    )

    layout: dict[int, tuple[int, int]] = {}
    for row in re.finditer(
        r"^\|\s*(?P<label>[^|]+?)\s*\|\s*(?P<cfg_type>\d+)\s*\|\s*(?P<bar>\d+)\s*\|\s*`(?P<offset>0x[0-9A-Fa-f]+)`\s*\|\s*`(?P<len>0x[0-9A-Fa-f]+)`\s*\|",
        section,
        flags=re.M,
    ):
        cfg_type = int(row.group("cfg_type"))
        bar = int(row.group("bar"))
        if bar != 0:
            fail(
                f"{W7_VIRTIO_CONTRACT_MD.as_posix()}: expected BAR=0 for virtio-pci modern caps in §1.4, got {bar} for cfg_type={cfg_type}"
            )
        layout[cfg_type] = (int(row.group("offset"), 16), int(row.group("len"), 16))

    missing = [cfg for cfg in (1, 2, 3, 4) if cfg not in layout]
    if missing:
        fail(
            format_error(
                f"{W7_VIRTIO_CONTRACT_MD.as_posix()}: could not parse all required virtio cap rows from §1.4 table:",
                [f"missing cfg_type: {cfg}" for cfg in missing],
            )
        )

    # notify_off_multiplier is described in §1.6.
    m = re.search(r"notify_off_multiplier\s*=\s*(?P<mul>\d+)", md, flags=re.I)
    if not m:
        fail(f"{W7_VIRTIO_CONTRACT_MD.as_posix()}: could not parse notify_off_multiplier from §1.6")
    notify_mul = int(m.group("mul"))

    return VirtioPciModernLayout(
        bar0_required_size=bar0_size,
        common_offset=layout[1][0],
        common_len=layout[1][1],
        notify_offset=layout[2][0],
        notify_len=layout[2][1],
        isr_offset=layout[3][0],
        isr_len=layout[3][1],
        device_offset=layout[4][0],
        device_len=layout[4][1],
        notify_off_multiplier=notify_mul,
    )


def parse_driver_virtio_pci_modern_layout(*, contract_major: int) -> VirtioPciModernLayout:
    del contract_major  # reserved for future multi-version layout checks

    header_text = read_text(AERO_VIRTIO_PCI_MODERN_TRANSPORT_H)
    bar0_required = parse_c_define_hex(
        header_text,
        "VIRTIO_PCI_MODERN_TRANSPORT_BAR0_REQUIRED_LEN",
        file=AERO_VIRTIO_PCI_MODERN_TRANSPORT_H,
    )
    if bar0_required is None:
        fail(
            f"{AERO_VIRTIO_PCI_MODERN_TRANSPORT_H.as_posix()}: missing VIRTIO_PCI_MODERN_TRANSPORT_BAR0_REQUIRED_LEN"
        )

    src = read_text(AERO_VIRTIO_PCI_MODERN_TRANSPORT_C)

    def enum_hex(name: str) -> int:
        m = re.search(rf"\b{re.escape(name)}\s*=\s*(?P<hex>0x[0-9A-Fa-f]+)", src)
        if not m:
            fail(f"{AERO_VIRTIO_PCI_MODERN_TRANSPORT_C.as_posix()}: missing enum constant {name}")
        return int(m.group("hex"), 16)

    def enum_dec(name: str) -> int:
        m = re.search(rf"\b{re.escape(name)}\s*=\s*(?P<dec>\d+)", src)
        if not m:
            fail(f"{AERO_VIRTIO_PCI_MODERN_TRANSPORT_C.as_posix()}: missing enum constant {name}")
        return int(m.group("dec"), 10)

    return VirtioPciModernLayout(
        bar0_required_size=bar0_required,
        common_offset=enum_hex("AERO_W7_VIRTIO_COMMON_OFF"),
        common_len=enum_hex("AERO_W7_VIRTIO_COMMON_MIN_LEN"),
        notify_offset=enum_hex("AERO_W7_VIRTIO_NOTIFY_OFF"),
        notify_len=enum_hex("AERO_W7_VIRTIO_NOTIFY_MIN_LEN"),
        isr_offset=enum_hex("AERO_W7_VIRTIO_ISR_OFF"),
        isr_len=enum_hex("AERO_W7_VIRTIO_ISR_MIN_LEN"),
        device_offset=enum_hex("AERO_W7_VIRTIO_DEVICE_OFF"),
        device_len=enum_hex("AERO_W7_VIRTIO_DEVICE_MIN_LEN"),
        notify_off_multiplier=enum_dec("AERO_W7_VIRTIO_NOTIFY_MULTIPLIER"),
    )


def parse_hex(value: str) -> int:
    try:
        return int(value, 16)
    except ValueError:
        fail(f"expected hex literal, got: {value!r}")


def parse_inf_hardware_ids(path: Path) -> set[str]:
    """
    Extract the set of active PCI Hardware IDs referenced by an INF.

    We intentionally keep parsing lightweight (line-based) because INFs are simple
    and we only need to guard against identity drift in the Models sections.
    Comment lines (starting with ';' after optional whitespace) are ignored.
    """

    text = read_text(path)
    out: set[str] = set()
    for raw in text.splitlines():
        line = raw.strip()
        if not line:
            continue
        if line.startswith(";"):
            continue
        # Strip inline comments.
        if ";" in line:
            line = line.split(";", 1)[0].rstrip()
            if not line:
                continue
        parts = [p.strip() for p in line.split(",")]
        if not parts:
            continue
        candidate = parts[-1]
        if candidate.upper().startswith("PCI\\VEN_"):
            out.add(candidate)
    return out


_PCI_HARDWARE_ID_RE = re.compile(r"^PCI\\VEN_(?P<ven>[0-9A-Fa-f]{4})&DEV_(?P<dev>[0-9A-Fa-f]{4})")


def parse_pci_vendor_device_from_hwid(hwid: str) -> tuple[int, int] | None:
    m = _PCI_HARDWARE_ID_RE.match(hwid)
    if not m:
        return None
    return int(m.group("ven"), 16), int(m.group("dev"), 16)


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

@dataclass(frozen=True)
class WindowsDeviceContractRow:
    identity: PciIdentity
    class_code: tuple[int, int, int]
    driver_service_name: str
    inf_name: str


@dataclass(frozen=True)
class EmulatorPciProfile:
    identity: PciIdentity
    bdf: tuple[int, int, int]
    class_code: tuple[int, int, int]
    header_type: int


def format_pci_class_code(code: tuple[int, int, int]) -> str:
    base, sub, prog_if = code
    return f"{base:02X}/{sub:02X}/{prog_if:02X}"


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

def parse_w7_contract_pci_identification_tables(
    md: str,
) -> tuple[int, int, int, Mapping[str, int], Mapping[str, int], Mapping[str, int]]:
    """
    Parse the summary PCI identification tables in AERO-W7-VIRTIO §1.1.

    These tables are normative and frequently edited; we validate them against
    the per-device PCI ID blocks in §3 so the contract doc cannot drift
    internally.
    """

    section = _extract_section(
        md,
        start=r"^###\s+1\.1\s+PCI identification",
        end=r"^###\s+1\.2\s+",
        file=W7_VIRTIO_CONTRACT_MD,
        what="section 1.1 (PCI identification)",
    )

    def require_hex(label: str, pattern: str) -> int:
        m = re.search(pattern, section, flags=re.M)
        if not m:
            fail(f"could not parse {label} from {W7_VIRTIO_CONTRACT_MD.as_posix()} section 1.1")
        return int(m.group("hex"), 16)

    vendor_id = require_hex("Vendor ID", r"^\*\*Vendor ID:\*\*\s*`(?P<hex>0x[0-9A-Fa-f]+)`")
    revision_id = require_hex(
        "PCI Revision ID", r"^\*\*PCI Revision ID:\*\*\s*`(?P<hex>0x[0-9A-Fa-f]+)`"
    )
    subsystem_vendor_id = require_hex(
        "Subsystem Vendor ID", r"^\*\*Subsystem Vendor ID:\*\*\s*`(?P<hex>0x[0-9A-Fa-f]+)`"
    )

    device_ids_section = _extract_subsection(
        section, heading=r"####\s+1\.1\.1\s+PCI Device IDs", file=W7_VIRTIO_CONTRACT_MD
    )
    virtio_device_types: dict[str, int] = {}
    pci_device_ids: dict[str, int] = {}
    for m in re.finditer(
        r"^\|\s*(?P<device>virtio-[a-z0-9-]+)\s*\|\s*(?P<virtio_id>\d+)\s*\|\s*`(?P<pci_id>0x[0-9A-Fa-f]+)`\s*\|",
        device_ids_section,
        flags=re.M,
    ):
        device = m.group("device")
        virtio_id = int(m.group("virtio_id"))
        pci_id = int(m.group("pci_id"), 16)
        if device in virtio_device_types:
            fail(
                f"{W7_VIRTIO_CONTRACT_MD.as_posix()}: duplicate device row {device!r} in table 1.1.1"
            )
        virtio_device_types[device] = virtio_id
        pci_device_ids[device] = pci_id

    if not virtio_device_types:
        fail(
            f"could not parse any rows from {W7_VIRTIO_CONTRACT_MD.as_posix()} table 1.1.1 "
            "(expected a markdown table listing virtio device IDs)"
        )

    subsys_section = _extract_subsection(
        section, heading=r"####\s+1\.1\.2\s+Subsystem IDs", file=W7_VIRTIO_CONTRACT_MD
    )
    subsys_ids: dict[str, int] = {}
    for m in re.finditer(
        r"^\|\s*(?P<instance>virtio-[^|]+?)\s*\|\s*`(?P<subsys_id>0x[0-9A-Fa-f]+)`\s*\|",
        subsys_section,
        flags=re.M,
    ):
        instance = m.group("instance").strip()
        subsys_id = int(m.group("subsys_id"), 16)
        if instance in subsys_ids:
            fail(
                f"{W7_VIRTIO_CONTRACT_MD.as_posix()}: duplicate instance row {instance!r} in table 1.1.2"
            )
        subsys_ids[instance] = subsys_id

    if not subsys_ids:
        fail(
            f"could not parse any rows from {W7_VIRTIO_CONTRACT_MD.as_posix()} table 1.1.2 "
            "(expected a markdown table listing subsystem device IDs)"
        )

    return vendor_id, subsystem_vendor_id, revision_id, virtio_device_types, pci_device_ids, subsys_ids


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


def parse_windows_device_contract_table(md: str) -> Mapping[str, WindowsDeviceContractRow]:
    expected_rows = {
        "virtio-blk",
        "virtio-net",
        "virtio-snd",
        "virtio-input (keyboard)",
        "virtio-input (mouse)",
    }
    out: dict[str, WindowsDeviceContractRow] = {}
    extra: list[str] = []
    for line in md.splitlines():
        if not line.lstrip().startswith("|"):
            continue
        if line.strip().startswith("| Device |"):
            continue
        parts = [p.strip() for p in line.strip().strip("|").split("|")]
        if len(parts) < 6:
            continue

        name = parts[0]
        if name.startswith("virtio-") and name not in expected_rows:
            extra.append(name)
            continue
        if name not in expected_rows:
            continue

        def backticked(cell: str, *, context: str) -> str:
            m = re.search(r"`([^`]+)`", cell)
            if not m:
                fail(f"{WINDOWS_DEVICE_CONTRACT_MD.as_posix()}: could not parse {context} from row {name!r}")
            return m.group(1).strip()

        pci_col = parts[1]
        subsys_col = parts[2]
        class_col = parts[3]
        service_col = parts[4]
        inf_col = parts[5]

        pci_vendor_s, pci_device_s = backticked(pci_col, context="PCI Vendor:Device").split(":")
        subsys_vendor_s, subsys_device_s = backticked(subsys_col, context="Subsystem Vendor:Device").split(":")
        class_code_s = backticked(class_col, context="Class code")
        class_parts = class_code_s.split("/")
        if len(class_parts) != 3:
            fail(
                f"{WINDOWS_DEVICE_CONTRACT_MD.as_posix()}: could not parse class code from row {name!r} "
                "(expected a backticked value like `01/00/00`)"
            )
        try:
            class_code = (int(class_parts[0], 16), int(class_parts[1], 16), int(class_parts[2], 16))
        except ValueError:
            fail(
                f"{WINDOWS_DEVICE_CONTRACT_MD.as_posix()}: could not parse class code from row {name!r} "
                f"(got {class_code_s!r})"
            )
        rev_m = re.search(r"REV\s*`0x(?P<rev>[0-9A-Fa-f]{2})`", pci_col)
        if not rev_m:
            fail(
                f"{WINDOWS_DEVICE_CONTRACT_MD.as_posix()}: could not parse revision ID from row {name!r} "
                "(expected '(REV `0x01`)')"
            )
        out[name] = WindowsDeviceContractRow(
            identity=PciIdentity(
                vendor_id=parse_hex(pci_vendor_s),
                device_id=parse_hex(pci_device_s),
                subsystem_vendor_id=parse_hex(subsys_vendor_s),
                subsystem_device_id=parse_hex(subsys_device_s),
                revision_id=int(rev_m.group("rev"), 16),
            ),
            class_code=class_code,
            driver_service_name=backticked(service_col, context="Windows service"),
            inf_name=backticked(inf_col, context="INF name"),
        )

    if extra:
        fail(
            format_error(
                f"{WINDOWS_DEVICE_CONTRACT_MD.as_posix()}: found virtio rows that are not covered by AERO-W7-VIRTIO v1 checks:",
                [f"unexpected row: {name!r}" for name in sorted(extra)],
            )
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


def _find_manifest_device(
    devices: list[object], name: str, *, file: Path = WINDOWS_DEVICE_CONTRACT_JSON
) -> dict[str, object]:
    for device in devices:
        if isinstance(device, dict) and device.get("device") == name:
            return device
    fail(f"device entry {name!r} not found in {file.as_posix()}")


def _parse_manifest_hex_u16(
    entry: Mapping[str, object], field: str, *, device: str, file: Path = WINDOWS_DEVICE_CONTRACT_JSON
) -> int:
    raw = entry.get(field)
    if not isinstance(raw, str):
        fail(f"{file.as_posix()}: {device} field {field!r} must be a hex string like '0x1AF4'")
    try:
        if not raw.lower().startswith("0x"):
            fail(
                f"{file.as_posix()}: {device} field {field!r} must be a 0x-prefixed hex string like '0x1AF4' "
                f"(got {raw!r})"
            )
        return int(raw, 16)
    except ValueError:
        fail(f"{file.as_posix()}: {device} field {field!r} has invalid hex string: {raw!r}")


def _require_str(
    entry: Mapping[str, object], field: str, *, device: str, file: Path = WINDOWS_DEVICE_CONTRACT_JSON
) -> str:
    raw = entry.get(field)
    if not isinstance(raw, str) or not raw.strip():
        fail(f"{file.as_posix()}: {device} field {field!r} must be a non-empty string")
    return raw.strip()


def _require_str_list(
    entry: Mapping[str, object], field: str, *, device: str, file: Path = WINDOWS_DEVICE_CONTRACT_JSON
) -> list[str]:
    raw = entry.get(field)
    if not isinstance(raw, list) or not all(isinstance(p, str) for p in raw):
        fail(f"{file.as_posix()}: {device} field {field!r} must be list[str]")
    return list(raw)


def _manifest_contains_subsys(patterns: list[str], subsys_vendor: int, subsys_device: int) -> bool:
    needle = f"SUBSYS_{subsys_device:04X}{subsys_vendor:04X}"
    return any(needle in p.upper() for p in patterns)


def _manifest_contains_exact(patterns: list[str], expected: str) -> bool:
    return any(p.upper() == expected.upper() for p in patterns)


def _normalize_hwid_patterns(patterns: list[str]) -> list[str]:
    # Normalize for case-insensitive, order-insensitive comparisons.
    return sorted({p.upper() for p in patterns})


def hwid_to_packaging_spec_regex(hwid: str) -> str:
    """
    Convert a literal HWID (e.g. `PCI\\VEN_1AF4&DEV_1042`) into the regex string
    expected by `tools/packaging/specs/*.json`.

    Packaging specs match HWIDs using regexes, so literal backslashes must be
    escaped (`\\`).
    """

    return hwid.replace("\\", "\\\\")


@dataclass
class PackagingSpecDriver:
    name: str
    expected_hardware_ids: list[str]


def normalize_packaging_spec_driver_name(name: str) -> str:
    # Keep behaviour aligned with `tools/packaging/aero_packager/src/spec.rs`.
    if name.lower() == "aero-gpu":
        return "aerogpu"
    return name


def parse_packaging_spec_drivers(spec: Mapping[str, object], *, file: Path) -> list[PackagingSpecDriver]:
    raw_drivers = spec.get("drivers", [])
    if not isinstance(raw_drivers, list):
        fail(f"{file.as_posix()}: 'drivers' must be a list")

    raw_required = spec.get("required_drivers", [])
    if not isinstance(raw_required, list):
        fail(f"{file.as_posix()}: 'required_drivers' must be a list")

    out: list[PackagingSpecDriver] = []
    index_by_name: dict[str, int] = {}

    def merge_entry(raw: object, *, field: str, idx: int) -> None:
        if not isinstance(raw, dict):
            fail(f"{file.as_posix()}: {field}[{idx}] must be an object")
        name_raw = raw.get("name")
        if not isinstance(name_raw, str) or not name_raw.strip():
            fail(f"{file.as_posix()}: {field}[{idx}].name must be a non-empty string")
        name = normalize_packaging_spec_driver_name(name_raw.strip())
        hwids_raw = raw.get("expected_hardware_ids", [])
        if not isinstance(hwids_raw, list) or not all(isinstance(v, str) and v.strip() for v in hwids_raw):
            fail(f"{file.as_posix()}: driver {name!r} expected_hardware_ids must be list[str]")
        hwids = [v.strip() for v in hwids_raw]

        key = name.lower()
        existing_idx = index_by_name.get(key)
        if existing_idx is None:
            index_by_name[key] = len(out)
            out.append(PackagingSpecDriver(name=name, expected_hardware_ids=hwids))
            return

        existing = out[existing_idx].expected_hardware_ids
        for hwid in hwids:
            if hwid not in existing:
                existing.append(hwid)

    for i, drv in enumerate(raw_drivers):
        merge_entry(drv, field="drivers", idx=i)
    for i, drv in enumerate(raw_required):
        merge_entry(drv, field="required_drivers", idx=i)

    return out


def parse_manifest_device_map(manifest: Mapping[str, object], *, file: Path) -> dict[str, dict[str, object]]:
    devices = manifest.get("devices")
    if not isinstance(devices, list):
        fail(f"'devices' must be a list in {file.as_posix()}")

    out: dict[str, dict[str, object]] = {}
    for i, raw in enumerate(devices):
        if not isinstance(raw, dict):
            fail(f"{file.as_posix()}: devices[{i}] must be an object")
        name = raw.get("device")
        if not isinstance(name, str) or not name:
            fail(f"{file.as_posix()}: devices[{i}].device must be a non-empty string")
        if name in out:
            fail(f"{file.as_posix()}: duplicate device entry: {name!r}")
        out[name] = raw
    return out


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


def parse_emulator_pci_profiles(path: Path) -> Mapping[str, EmulatorPciProfile]:
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

    profiles: dict[str, EmulatorPciProfile] = {}
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
            # Capture the full RHS expression (greedy within the line). Some fields (like
            # `class: PciClassCode::new(0x02, 0x00, 0x00),`) contain commas, so a simple
            # `[^,]+` parser would truncate.
            fm = re.search(
                rf"^\s*{re.escape(field_name)}:\s*(?P<expr>.+),\s*(?://.*)?$",
                body,
                flags=re.M,
            )
            if not fm:
                fail(f"{path.as_posix()}: profile {profile_name} missing field {field_name}")
            return fm.group("expr").strip()

        class_expr = field("class")
        class_m = re.search(
            r"^PciClassCode::new\(\s*(?P<base>[^,]+)\s*,\s*(?P<sub>[^,]+)\s*,\s*(?P<prog>[^)]+?)\s*\)$",
            class_expr,
        )
        if not class_m:
            fail(
                f"{path.as_posix()}: profile {profile_name} has unsupported class expression {class_expr!r} "
                "(expected PciClassCode::new(base, sub, prog_if))"
            )
        class_code = (
            eval_u8(class_m.group("base")),
            eval_u8(class_m.group("sub")),
            eval_u8(class_m.group("prog")),
        )

        bdf_expr = field("bdf")
        bdf_m = re.search(
            r"^PciBdf::new\(\s*(?P<bus>[^,]+)\s*,\s*(?P<dev>[^,]+)\s*,\s*(?P<fun>[^)]+)\s*\)$",
            bdf_expr,
        )
        if not bdf_m:
            fail(
                f"{path.as_posix()}: profile {profile_name} has unsupported bdf expression {bdf_expr!r} "
                "(expected PciBdf::new(bus, device, function))"
            )
        bdf = (
            eval_u8(bdf_m.group("bus")),
            eval_u8(bdf_m.group("dev")),
            eval_u8(bdf_m.group("fun")),
        )
        profiles[profile_name] = EmulatorPciProfile(
            identity=PciIdentity(
                vendor_id=eval_u16(field("vendor_id")),
                device_id=eval_u16(field("device_id")),
                subsystem_vendor_id=eval_u16(field("subsystem_vendor_id")),
                subsystem_device_id=eval_u16(field("subsystem_id")),
                revision_id=eval_u8(field("revision_id")),
            ),
            bdf=bdf,
            class_code=class_code,
            header_type=eval_u8(field("header_type")),
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

    contract_major = parse_contract_major_version(w7_md)
    contract_rev = parse_contract_revision_id(w7_md)
    contract_ids = parse_w7_contract_pci_identities(w7_md)
    contract_queues = parse_w7_contract_queue_sizes(w7_md)
    (
        contract_vendor_id,
        contract_subsystem_vendor_id,
        contract_revision_id,
        contract_virtio_types,
        contract_pci_device_ids,
        contract_subsystem_ids,
    ) = parse_w7_contract_pci_identification_tables(w7_md)

    deprecated_hits: list[str] = []
    deprecated_hits.extend(scan_text_tree_for_substrings(WIN7_VIRTIO_TESTS_ROOT, DEPRECATED_WIN7_TEST_INF_BASENAMES))
    # The per-workstream onboarding docs under `instructions/` also frequently mention
    # driver package/INF names. Keep them aligned with the canonical `aero_virtio_*.inf`
    # basenames so new contributors don't cargo-cult old names into code or scripts.
    deprecated_hits.extend(scan_text_tree_for_substrings(INSTRUCTIONS_ROOT, DEPRECATED_WIN7_TEST_INF_BASENAMES))
    if deprecated_hits:
        errors.append(
            format_error(
                "Docs/tests reference deprecated Win7 virtio INF basenames. Update docs/examples to use canonical aero_virtio_*.inf files:",
                deprecated_hits,
            )
        )

    # Ensure the contract doesn't silently grow new virtio devices without also
    # extending this checker.
    unknown_contract_devices = sorted(set(contract_virtio_types) - set(SUPPORTED_VIRTIO_DEVICES))
    if unknown_contract_devices:
        errors.append(
            format_error(
                "AERO-W7-VIRTIO table 1.1.1 contains virtio devices not covered by this CI check:",
                [f"unexpected device: {name!r}" for name in unknown_contract_devices],
            )
        )

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

    if contract_vendor_id != VIRTIO_PCI_VENDOR_ID:
        errors.append(
            format_error(
                "AERO-W7-VIRTIO §1.1 Vendor ID mismatch:",
                [
                    f"expected: 0x{VIRTIO_PCI_VENDOR_ID:04X}",
                    f"got: 0x{contract_vendor_id:04X}",
                ],
            )
        )
    if contract_subsystem_vendor_id != VIRTIO_PCI_VENDOR_ID:
        errors.append(
            format_error(
                "AERO-W7-VIRTIO §1.1 Subsystem Vendor ID mismatch:",
                [
                    f"expected: 0x{VIRTIO_PCI_VENDOR_ID:04X}",
                    f"got: 0x{contract_subsystem_vendor_id:04X}",
                ],
            )
        )
    if contract_revision_id != contract_rev:
        errors.append(
            format_error(
                "AERO-W7-VIRTIO §1.1 PCI Revision ID mismatch:",
                [
                    f"expected (from contract header): 0x{contract_rev:02X}",
                    f"got (from §1.1): 0x{contract_revision_id:02X}",
                ],
            )
        )

    for device, expected_type in VIRTIO_DEVICE_TYPE_IDS.items():
        actual = contract_virtio_types.get(device)
        if actual is None:
            errors.append(
                format_error(
                    "AERO-W7-VIRTIO table 1.1.1 is missing a required virtio device:",
                    [f"missing: {device!r}"],
                )
            )
            continue
        if actual != expected_type:
            errors.append(
                format_error(
                    f"AERO-W7-VIRTIO table 1.1.1 virtio device id mismatch for {device}:",
                    [
                        f"expected: {expected_type}",
                        f"got: {actual}",
                    ],
                )
            )

        actual_pci_id = contract_pci_device_ids.get(device)
        if actual_pci_id is None:
            errors.append(
                format_error(
                    "AERO-W7-VIRTIO table 1.1.1 is missing a required PCI Device ID entry:",
                    [f"missing PCI device id for: {device!r}"],
                )
            )
        else:
            expected_pci_id = VIRTIO_PCI_DEVICE_ID_BASE + expected_type
            if actual_pci_id != expected_pci_id:
                errors.append(
                    format_error(
                        f"AERO-W7-VIRTIO table 1.1.1 PCI Device ID mismatch for {device}:",
                        [
                            f"expected: 0x{expected_pci_id:04X}",
                            f"got: 0x{actual_pci_id:04X}",
                        ],
                    )
                )

    for instance, expected_subsys in (
        ("virtio-net", contract_ids["virtio-net"].subsystem_device_id),
        ("virtio-blk", contract_ids["virtio-blk"].subsystem_device_id),
        ("virtio-snd", contract_ids["virtio-snd"].subsystem_device_id),
        ("virtio-input (keyboard)", contract_ids["virtio-input (keyboard)"].subsystem_device_id),
        ("virtio-input (mouse)", contract_ids["virtio-input (mouse)"].subsystem_device_id),
    ):
        actual = contract_subsystem_ids.get(instance)
        if actual is None:
            errors.append(
                format_error(
                    "AERO-W7-VIRTIO table 1.1.2 is missing a required subsystem ID row:",
                    [f"missing: {instance!r}"],
                )
            )
            continue
        if actual != expected_subsys:
            errors.append(
                format_error(
                    f"AERO-W7-VIRTIO table 1.1.2 subsystem ID mismatch for {instance}:",
                    [
                        f"expected (from §3 PCI IDs): 0x{expected_subsys:04X}",
                        f"got (from table):          0x{actual:04X}",
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
        if (doc.identity.vendor_id, doc.identity.device_id) != (contract.vendor_id, contract.device_id):
            errors.append(
                format_error(
                    f"{row_name}: Vendor/Device ID mismatch between docs:",
                    [
                        f"{W7_VIRTIO_CONTRACT_MD.as_posix()}: {contract.vendor_device_str()}",
                        f"{WINDOWS_DEVICE_CONTRACT_MD.as_posix()}: {doc.identity.vendor_device_str()}",
                    ],
                )
            )
        if (doc.identity.subsystem_vendor_id, doc.identity.subsystem_device_id) != (
            contract.subsystem_vendor_id,
            contract.subsystem_device_id,
        ):
            errors.append(
                format_error(
                    f"{row_name}: Subsystem Vendor/Device ID mismatch between docs:",
                    [
                        f"{W7_VIRTIO_CONTRACT_MD.as_posix()}: {contract.subsys_str()}",
                        f"{WINDOWS_DEVICE_CONTRACT_MD.as_posix()}: {doc.identity.subsys_str()}",
                    ],
                )
            )
        if doc.identity.revision_id != contract.revision_id or contract.revision_id != contract_rev:
            errors.append(
                format_error(
                    f"{row_name}: Revision ID mismatch (contract major version is encoded in PCI Revision ID):",
                    [
                        f"{W7_VIRTIO_CONTRACT_MD.as_posix()}: 0x{contract.revision_id:02X} (header says 0x{contract_rev:02X})",
                        f"{WINDOWS_DEVICE_CONTRACT_MD.as_posix()}: 0x{doc.identity.revision_id:02X}",
                    ],
                )
            )

    # ---------------------------------------------------------------------
    # 2) windows-device-contract.json must match Vendor/Device ID + SUBSYS IDs.
    # ---------------------------------------------------------------------
    devices = manifest.get("devices")
    if not isinstance(devices, list):
        fail(f"'devices' must be a list in {WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}")

    extra_manifest_virtio = sorted(
        {
            entry.get("device")
            for entry in devices
            if isinstance(entry, dict)
            and isinstance(entry.get("device"), str)
            and entry.get("device", "").startswith("virtio-")
            and entry.get("device") not in SUPPORTED_VIRTIO_DEVICES
        }
    )
    if extra_manifest_virtio:
        errors.append(
            format_error(
                f"{WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}: manifest contains virtio devices not covered by AERO-W7-VIRTIO v1 checks:",
                [f"unexpected device: {name!r}" for name in extra_manifest_virtio],
            )
        )

    for device_name in SUPPORTED_VIRTIO_DEVICES:
        entry = _find_manifest_device(devices, device_name)
        vendor = _parse_manifest_hex_u16(entry, "pci_vendor_id", device=device_name)
        dev_id = _parse_manifest_hex_u16(entry, "pci_device_id", device=device_name)
        service_name = _require_str(entry, "driver_service_name", device=device_name)
        inf_name = _require_str(entry, "inf_name", device=device_name)
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

        # Service/INF names must match the human-readable device contract.
        if device_name == "virtio-input":
            kbd_row = table_ids["virtio-input (keyboard)"]
            mouse_row = table_ids["virtio-input (mouse)"]
            if kbd_row.driver_service_name != mouse_row.driver_service_name or kbd_row.inf_name != mouse_row.inf_name:
                errors.append(
                    format_error(
                        "virtio-input: windows-device-contract.md rows disagree on service/INF name:",
                        [
                            f"keyboard row: service={kbd_row.driver_service_name!r} inf={kbd_row.inf_name!r}",
                            f"mouse row:    service={mouse_row.driver_service_name!r} inf={mouse_row.inf_name!r}",
                        ],
                    )
                )
            expected_service = kbd_row.driver_service_name
            expected_inf = kbd_row.inf_name
        else:
            row = table_ids[device_name]
            expected_service = row.driver_service_name
            expected_inf = row.inf_name

        if service_name != expected_service:
            errors.append(
                format_error(
                    f"{device_name}: driver_service_name mismatch between windows-device-contract.md and manifest:",
                    [
                        f"{WINDOWS_DEVICE_CONTRACT_MD.as_posix()}: {expected_service!r}",
                        f"{WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}: {service_name!r}",
                    ],
                )
            )
        if inf_name != expected_inf:
            errors.append(
                format_error(
                    f"{device_name}: inf_name mismatch between windows-device-contract.md and manifest:",
                    [
                        f"{WINDOWS_DEVICE_CONTRACT_MD.as_posix()}: {expected_inf!r}",
                        f"{WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}: {inf_name!r}",
                    ],
                )
            )

        patterns = _require_str_list(entry, "hardware_id_patterns", device=device_name)
        base_hwid = f"PCI\\VEN_{contract_any.vendor_id:04X}&DEV_{contract_any.device_id:04X}"
        base_hwid_upper = base_hwid.upper()
        if not _manifest_contains_exact(patterns, base_hwid):
            errors.append(
                format_error(
                    f"{device_name}: manifest is missing the base VEN/DEV hardware ID pattern:",
                    [
                        f"expected: {base_hwid}",
                        f"got: {patterns}",
                    ],
                )
            )

        expected_rev_fragment = f"REV_{contract_rev:02X}"
        strict_rev_hwid = f"{base_hwid}&{expected_rev_fragment}"
        if not _manifest_contains_exact(patterns, strict_rev_hwid):
            errors.append(
                format_error(
                    f"{device_name}: manifest is missing the strict REV-qualified hardware ID pattern (required for automation / contract major safety):",
                    [
                        f"expected: {strict_rev_hwid}",
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
            else:
                expected_subsys_fragment = f"SUBSYS_{subsys_device:04X}{contract_any.subsystem_vendor_id:04X}"
                if not any(
                    pat.upper().startswith(base_hwid_upper)
                    and expected_subsys_fragment in pat.upper()
                    and expected_rev_fragment in pat.upper()
                    for pat in patterns
                ):
                    errors.append(
                        format_error(
                            f"{device_name}: manifest is missing a revision-qualified SUBSYS hardware ID:",
                            [
                                f"expected at least one pattern containing: {base_hwid}&{expected_subsys_fragment}&{expected_rev_fragment}",
                                f"got: {patterns}",
                            ],
                        )
                    )

            strict_subsys_rev = (
                f"{base_hwid}&SUBSYS_{subsys_device:04X}{contract_any.subsystem_vendor_id:04X}&REV_{contract_rev:02X}"
            )
            if not _manifest_contains_exact(patterns, strict_subsys_rev):
                errors.append(
                    format_error(
                        f"{device_name}: manifest is missing the strict SUBSYS+REV-qualified hardware ID pattern (preferred for automation / avoiding false positives):",
                        [
                            f"expected: {strict_subsys_rev}",
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

        # -----------------------------------------------------------------
        # 2.1) Canonical driver INF must bind to the same VEN/DEV and service.
        # -----------------------------------------------------------------
        inf_path = AERO_VIRTIO_INF_SOURCES.get(device_name)
        if inf_path is None:
            errors.append(
                format_error(
                    f"{device_name}: missing INF path mapping in {Path(__file__).as_posix()}:",
                    [
                        f"expected an entry in AERO_VIRTIO_INF_SOURCES for {device_name!r}",
                    ],
                )
            )
        else:
            inf_text = strip_inf_comment_lines(read_text(inf_path))
            inf_upper = inf_text.upper()

            if inf_path.name != expected_inf:
                errors.append(
                    format_error(
                        f"{device_name}: canonical INF path does not match expected INF file name from contract:",
                        [
                            f"expected: {expected_inf!r}",
                            f"got: {inf_path.as_posix()}",
                        ],
                    )
                )

            if base_hwid_upper not in inf_upper:
                errors.append(
                    format_error(
                        f"{device_name}: driver INF does not contain the expected canonical VEN/DEV hardware ID:",
                        [
                            f"expected to find: {base_hwid}",
                            f"file: {inf_path.as_posix()}",
                            ],
                        )
                    )

            if re.search(
                r"PCI\\VEN_1AF4&DEV_(?:10[0-3][0-9A-Fa-f]|100[0-9A-Fa-f])",
                inf_text,
                flags=re.I,
            ):
                errors.append(
                    format_error(
                        f"{device_name}: canonical INF contains transitional virtio-pci device IDs (out of scope for AERO-W7-VIRTIO):",
                        [
                            f"file: {inf_path.as_posix()}",
                            "hint: keep transitional IDs in separate opt-in legacy INFs only",
                        ],
                    )
                )

            # If the INF revision-qualifies the HWID, ensure it matches the contract major.
            for line in inf_text.splitlines():
                if base_hwid_upper not in line.upper():
                    continue
                m = re.search(r"&REV_(?P<rev>[0-9A-Fa-f]{2})", line)
                if not m:
                    continue
                rev = int(m.group("rev"), 16)
                if rev != contract_rev:
                    errors.append(
                        format_error(
                            f"{device_name}: driver INF REV_ qualifier mismatch:",
                            [
                                f"expected: REV_{contract_rev:02X}",
                                f"got: {line.strip()}",
                                f"file: {inf_path.as_posix()}",
                            ],
                        )
                    )

            if not re.search(
                # INF syntax allows string tokens to be quoted; accept both:
                #   AddService = foo, ...
                #   AddService = "foo", ...
                rf'^\s*AddService\s*=\s*"?{re.escape(expected_service)}"?\s*(?:,|$)',
                inf_text,
                flags=re.I | re.M,
            ):
                errors.append(
                    format_error(
                        f"{device_name}: driver INF does not install the expected Windows service name:",
                        [
                            f"expected AddService = {expected_service}",
                            f"file: {inf_path.as_posix()}",
                        ],
                    )
                )

    # ---------------------------------------------------------------------
    # 2.2) Canonical Windows driver source constants must match the contract.
    # ---------------------------------------------------------------------
    pci_identity_text = read_text(AERO_VIRTIO_PCI_IDENTITY_HEADER)
    pci_vendor = parse_c_define_hex(
        pci_identity_text, "VIRTIO_PCI_IDENTITY_VENDOR_ID_VIRTIO", file=AERO_VIRTIO_PCI_IDENTITY_HEADER
    )
    if pci_vendor is None or pci_vendor != VIRTIO_PCI_VENDOR_ID:
        errors.append(
            format_error(
                "virtio_pci_identity.h: VIRTIO_PCI_IDENTITY_VENDOR_ID_VIRTIO mismatch:",
                [
                    f"expected: 0x{VIRTIO_PCI_VENDOR_ID:04X}",
                    f"got: {pci_vendor!r}",
                    f"file: {AERO_VIRTIO_PCI_IDENTITY_HEADER.as_posix()}",
                ],
            )
        )

    pci_modern_base = parse_c_define_hex(
        pci_identity_text, "VIRTIO_PCI_IDENTITY_DEVICE_ID_MODERN_BASE", file=AERO_VIRTIO_PCI_IDENTITY_HEADER
    )
    if pci_modern_base is None or pci_modern_base != VIRTIO_PCI_DEVICE_ID_BASE:
        errors.append(
            format_error(
                "virtio_pci_identity.h: VIRTIO_PCI_IDENTITY_DEVICE_ID_MODERN_BASE mismatch:",
                [
                    f"expected: 0x{VIRTIO_PCI_DEVICE_ID_BASE:04X}",
                    f"got: {pci_modern_base!r}",
                    f"file: {AERO_VIRTIO_PCI_IDENTITY_HEADER.as_posix()}",
                ],
            )
        )

    contract_rev_macro = f"VIRTIO_PCI_IDENTITY_AERO_CONTRACT_V{contract_major}_REVISION_ID"
    pci_contract_rev = parse_c_define_hex(pci_identity_text, contract_rev_macro, file=AERO_VIRTIO_PCI_IDENTITY_HEADER)
    if pci_contract_rev is None:
        errors.append(
            format_error(
                f"virtio_pci_identity.h: missing required contract revision macro {contract_rev_macro}:",
                [
                    f"file: {AERO_VIRTIO_PCI_IDENTITY_HEADER.as_posix()}",
                    "hint: add a macro like 'VIRTIO_PCI_IDENTITY_AERO_CONTRACT_V1_REVISION_ID 0x01u'",
                ],
            )
        )
    elif pci_contract_rev != contract_rev:
        errors.append(
            format_error(
                f"virtio_pci_identity.h: {contract_rev_macro} mismatch:",
                [
                    f"expected: 0x{contract_rev:02X}",
                    f"got: 0x{pci_contract_rev:02X}",
                    f"file: {AERO_VIRTIO_PCI_IDENTITY_HEADER.as_posix()}",
                ],
            )
        )

    blk_header = read_text(AERO_VIRTIO_BLK_DRIVER_HEADER)
    for macro, expected in (
        ("AEROVBLK_PCI_VENDOR_ID", VIRTIO_PCI_VENDOR_ID),
        ("AEROVBLK_PCI_DEVICE_ID", contract_ids["virtio-blk"].device_id),
        ("AEROVBLK_VIRTIO_PCI_REVISION_ID", contract_rev),
    ):
        val = parse_c_define_hex(blk_header, macro, file=AERO_VIRTIO_BLK_DRIVER_HEADER)
        if val is None or val != expected:
            errors.append(
                format_error(
                    f"virtio-blk driver header mismatch for {macro}:",
                    [
                        f"expected: 0x{expected:04X}",
                        f"got: {val!r}",
                        f"file: {AERO_VIRTIO_BLK_DRIVER_HEADER.as_posix()}",
                    ],
                )
            )

    net_header = read_text(AERO_VIRTIO_NET_DRIVER_HEADER)
    for macro, expected in (
        ("AEROVNET_VENDOR_ID", VIRTIO_PCI_VENDOR_ID),
        ("AEROVNET_PCI_DEVICE_ID", contract_ids["virtio-net"].device_id),
        ("AEROVNET_PCI_REVISION_ID", contract_rev),
    ):
        val = parse_c_define_hex(net_header, macro, file=AERO_VIRTIO_NET_DRIVER_HEADER)
        if val is None or val != expected:
            errors.append(
                format_error(
                    f"virtio-net driver header mismatch for {macro}:",
                    [
                        f"expected: 0x{expected:04X}",
                        f"got: {val!r}",
                        f"file: {AERO_VIRTIO_NET_DRIVER_HEADER.as_posix()}",
                    ],
                )
            )

    input_header = read_text(AERO_VIRTIO_INPUT_DRIVER_HEADER)
    for macro, expected in (
        ("VIOINPUT_PCI_SUBSYSTEM_ID_KEYBOARD", contract_ids["virtio-input (keyboard)"].subsystem_device_id),
        ("VIOINPUT_PCI_SUBSYSTEM_ID_MOUSE", contract_ids["virtio-input (mouse)"].subsystem_device_id),
    ):
        val = parse_c_define_hex(input_header, macro, file=AERO_VIRTIO_INPUT_DRIVER_HEADER)
        if val is None or val != expected:
            errors.append(
                format_error(
                    f"virtio-input driver header mismatch for {macro}:",
                    [
                        f"expected: 0x{expected:04X}",
                        f"got: {val!r}",
                        f"file: {AERO_VIRTIO_INPUT_DRIVER_HEADER.as_posix()}",
                    ],
                )
            )

    # ---------------------------------------------------------------------
    # 2.3) Guest Tools packaging specs must stay aligned with device contract HWIDs.
    # ---------------------------------------------------------------------
    contract_device_map = parse_manifest_device_map(manifest, file=WINDOWS_DEVICE_CONTRACT_JSON)
    allowed_hwid_regexes_by_device: dict[str, set[str]] = {}
    for name, entry in contract_device_map.items():
        patterns = _require_str_list(entry, "hardware_id_patterns", device=name)
        allowed_hwid_regexes_by_device[name] = {hwid_to_packaging_spec_regex(p).upper() for p in patterns}

    for spec_path in AERO_GUEST_TOOLS_PACKAGING_SPECS:
        try:
            raw = json.loads(read_text(spec_path))
        except json.JSONDecodeError as e:
            fail(f"invalid JSON in {spec_path.as_posix()}: {e}")
        if not isinstance(raw, dict):
            fail(f"{spec_path.as_posix()}: packaging spec must be a JSON object")

        for drv in parse_packaging_spec_drivers(raw, file=spec_path):
            if not drv.expected_hardware_ids:
                continue

            device = PACKAGING_SPEC_DRIVER_TO_CONTRACT_DEVICE.get(drv.name.lower())
            if device is None:
                errors.append(
                    format_error(
                        f"{spec_path.as_posix()}: cannot validate expected_hardware_ids for unknown driver {drv.name!r}:",
                        [
                            "hint: extend PACKAGING_SPEC_DRIVER_TO_CONTRACT_DEVICE in scripts/ci/check-windows7-virtio-contract-consistency.py",
                            "expected_hardware_ids:",
                            *[f"- {json.dumps(h)}" for h in drv.expected_hardware_ids],
                        ],
                    )
                )
                continue

            allowed = allowed_hwid_regexes_by_device.get(device)
            if allowed is None:
                errors.append(
                    format_error(
                        f"{spec_path.as_posix()}: driver {drv.name!r} references unknown contract device {device!r}:",
                        [
                            f"known contract devices: {sorted(allowed_hwid_regexes_by_device)}",
                        ],
                    )
                )
                continue

            invalid = [h for h in drv.expected_hardware_ids if h.upper() not in allowed]
            if invalid:
                allowed_sorted = sorted(allowed)
                errors.append(
                    format_error(
                        f"{spec_path.as_posix()}: driver {drv.name!r} expected_hardware_ids must match {WINDOWS_DEVICE_CONTRACT_JSON.as_posix()} patterns for {device}:",
                        [
                            "unexpected expected_hardware_ids:",
                            *[f"- {json.dumps(h)}" for h in invalid],
                            "allowed patterns:",
                            *[f"- {json.dumps(h)}" for h in allowed_sorted],
                        ],
                    )
                )

    # Optional variant: a contract file intended for binding to virtio-win drivers
    # (different service/INF names) while keeping emulator-presented HWIDs stable.
    # If present, keep it aligned with the canonical device contract.
    if WINDOWS_DEVICE_CONTRACT_VIRTIO_WIN_JSON.exists():
        try:
            virtio_win_manifest = json.loads(read_text(WINDOWS_DEVICE_CONTRACT_VIRTIO_WIN_JSON))
        except json.JSONDecodeError as e:
            fail(f"invalid JSON in {WINDOWS_DEVICE_CONTRACT_VIRTIO_WIN_JSON.as_posix()}: {e}")

        base_schema = manifest.get("schema_version")
        virtio_win_schema = virtio_win_manifest.get("schema_version")
        if base_schema != virtio_win_schema:
            errors.append(
                format_error(
                    "windows-device-contract-virtio-win.json schema_version must match windows-device-contract.json:",
                    [
                        f"{WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}: {base_schema!r}",
                        f"{WINDOWS_DEVICE_CONTRACT_VIRTIO_WIN_JSON.as_posix()}: {virtio_win_schema!r}",
                    ],
                )
            )

        base_version = manifest.get("contract_version")
        virtio_win_version = virtio_win_manifest.get("contract_version")
        if base_version != virtio_win_version:
            errors.append(
                format_error(
                    "windows-device-contract-virtio-win.json contract_version must match windows-device-contract.json:",
                    [
                        f"{WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}: {base_version!r}",
                        f"{WINDOWS_DEVICE_CONTRACT_VIRTIO_WIN_JSON.as_posix()}: {virtio_win_version!r}",
                    ],
                )
            )

        base_manifest_devices = parse_manifest_device_map(manifest, file=WINDOWS_DEVICE_CONTRACT_JSON)
        virtio_win_devices = parse_manifest_device_map(virtio_win_manifest, file=WINDOWS_DEVICE_CONTRACT_VIRTIO_WIN_JSON)

        if set(base_manifest_devices) != set(virtio_win_devices):
            missing = sorted(set(base_manifest_devices) - set(virtio_win_devices))
            extra = sorted(set(virtio_win_devices) - set(base_manifest_devices))
            errors.append(
                format_error(
                    "windows-device-contract-virtio-win.json must mirror windows-device-contract.json device entries:",
                    [
                        f"missing from virtio-win: {missing}" if missing else "missing from virtio-win: (none)",
                        f"extra in virtio-win:    {extra}" if extra else "extra in virtio-win:    (none)",
                    ],
                )
            )

        expected_virtio_win_bindings: Mapping[str, tuple[str, str]] = {
            "virtio-blk": ("viostor", "viostor.inf"),
            "virtio-net": ("netkvm", "netkvm.inf"),
            "virtio-input": ("vioinput", "vioinput.inf"),
            "virtio-snd": ("viosnd", "viosnd.inf"),
        }

        for name in sorted(set(base_manifest_devices) & set(virtio_win_devices)):
            base_entry = base_manifest_devices[name]
            win_entry = virtio_win_devices[name]

            base_vendor = _parse_manifest_hex_u16(base_entry, "pci_vendor_id", device=name)
            base_dev = _parse_manifest_hex_u16(base_entry, "pci_device_id", device=name)
            win_vendor = _parse_manifest_hex_u16(
                win_entry, "pci_vendor_id", device=name, file=WINDOWS_DEVICE_CONTRACT_VIRTIO_WIN_JSON
            )
            win_dev = _parse_manifest_hex_u16(
                win_entry, "pci_device_id", device=name, file=WINDOWS_DEVICE_CONTRACT_VIRTIO_WIN_JSON
            )
            if (base_vendor, base_dev) != (win_vendor, win_dev):
                errors.append(
                    format_error(
                        f"{name}: PCI Vendor/Device mismatch between contract variants:",
                        [
                            f"{WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}: {base_vendor:04X}:{base_dev:04X}",
                            f"{WINDOWS_DEVICE_CONTRACT_VIRTIO_WIN_JSON.as_posix()}: {win_vendor:04X}:{win_dev:04X}",
                        ],
                    )
                )

            base_patterns = _normalize_hwid_patterns(
                _require_str_list(base_entry, "hardware_id_patterns", device=name)
            )
            win_patterns = _normalize_hwid_patterns(
                _require_str_list(
                    win_entry,
                    "hardware_id_patterns",
                    device=name,
                    file=WINDOWS_DEVICE_CONTRACT_VIRTIO_WIN_JSON,
                )
            )
            if base_patterns != win_patterns:
                errors.append(
                    format_error(
                        f"{name}: hardware_id_patterns must be identical between contract variants:",
                        [
                            f"{WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}: {base_patterns}",
                            f"{WINDOWS_DEVICE_CONTRACT_VIRTIO_WIN_JSON.as_posix()}: {win_patterns}",
                        ],
                    )
                )

            if base_entry.get("virtio_device_type") != win_entry.get("virtio_device_type"):
                errors.append(
                    format_error(
                        f"{name}: virtio_device_type mismatch between contract variants:",
                        [
                            f"{WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}: {base_entry.get('virtio_device_type')!r}",
                            f"{WINDOWS_DEVICE_CONTRACT_VIRTIO_WIN_JSON.as_posix()}: {win_entry.get('virtio_device_type')!r}",
                        ],
                    )
                )

            if name in expected_virtio_win_bindings:
                expected_service, expected_inf = expected_virtio_win_bindings[name]
                actual_service = _require_str(
                    win_entry, "driver_service_name", device=name, file=WINDOWS_DEVICE_CONTRACT_VIRTIO_WIN_JSON
                )
                actual_inf = _require_str(win_entry, "inf_name", device=name, file=WINDOWS_DEVICE_CONTRACT_VIRTIO_WIN_JSON)
                if actual_service.lower() != expected_service.lower():
                    errors.append(
                        format_error(
                            f"{name}: unexpected virtio-win service name in {WINDOWS_DEVICE_CONTRACT_VIRTIO_WIN_JSON.as_posix()}:",
                            [
                                f"expected: {expected_service}",
                                f"got: {actual_service}",
                            ],
                        )
                    )
                if actual_inf.lower() != expected_inf.lower():
                    errors.append(
                        format_error(
                            f"{name}: unexpected virtio-win INF name in {WINDOWS_DEVICE_CONTRACT_VIRTIO_WIN_JSON.as_posix()}:",
                            [
                                f"expected: {expected_inf}",
                                f"got: {actual_inf}",
                            ],
                        )
                    )
            else:
                base_service = _require_str(base_entry, "driver_service_name", device=name)
                win_service = _require_str(
                    win_entry, "driver_service_name", device=name, file=WINDOWS_DEVICE_CONTRACT_VIRTIO_WIN_JSON
                )
                if base_service != win_service:
                    errors.append(
                        format_error(
                            f"{name}: driver_service_name must match between contract variants:",
                            [
                                f"{WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}: {base_service}",
                                f"{WINDOWS_DEVICE_CONTRACT_VIRTIO_WIN_JSON.as_posix()}: {win_service}",
                            ],
                        )
                    )

                base_inf = _require_str(base_entry, "inf_name", device=name)
                win_inf = _require_str(win_entry, "inf_name", device=name, file=WINDOWS_DEVICE_CONTRACT_VIRTIO_WIN_JSON)
                if base_inf != win_inf:
                    errors.append(
                        format_error(
                            f"{name}: inf_name must match between contract variants:",
                            [
                                f"{WINDOWS_DEVICE_CONTRACT_JSON.as_posix()}: {base_inf}",
                                f"{WINDOWS_DEVICE_CONTRACT_VIRTIO_WIN_JSON.as_posix()}: {win_inf}",
                            ],
                        )
                    )
    contract_mmio = parse_contract_fixed_mmio_layout(w7_md)
    driver_mmio = parse_driver_virtio_pci_modern_layout(contract_major=contract_major)
    if contract_mmio != driver_mmio:
        diffs: list[str] = []
        for field in contract_mmio.__dataclass_fields__:
            a = getattr(contract_mmio, field)
            b = getattr(driver_mmio, field)
            if a == b:
                continue
            if field == "notify_off_multiplier":
                diffs.append(f"{field}: contract={a} driver={b}")
            else:
                diffs.append(f"{field}: contract=0x{a:04X} driver=0x{b:04X}")

        errors.append(
            format_error(
                "virtio-pci modern fixed MMIO layout mismatch between contract and Windows transport:",
                [
                    f"{W7_VIRTIO_CONTRACT_MD.as_posix()}: §1.2/§1.4/§1.6",
                    f"{AERO_VIRTIO_PCI_MODERN_TRANSPORT_H.as_posix()}",
                    f"{AERO_VIRTIO_PCI_MODERN_TRANSPORT_C.as_posix()}",
                    *diffs,
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
            if (prof.identity.vendor_id, prof.identity.device_id) != (contract.vendor_id, contract.device_id):
                errors.append(
                    format_error(
                        f"{name}: emulator PCI profile Vendor/Device mismatch ({AERO_DEVICES_PCI_PROFILE_RS.as_posix()}):",
                        [
                            f"expected: {contract.vendor_device_str()}",
                            f"got: {prof.identity.vendor_device_str()}",
                        ],
                    )
                )
            if (prof.identity.subsystem_vendor_id, prof.identity.subsystem_device_id) != (
                contract.subsystem_vendor_id,
                contract.subsystem_device_id,
            ):
                errors.append(
                    format_error(
                        f"{name}: emulator PCI profile subsystem mismatch ({AERO_DEVICES_PCI_PROFILE_RS.as_posix()}):",
                        [
                            f"expected: {contract.subsys_str()}",
                            f"got: {prof.identity.subsys_str()}",
                        ],
                    )
                )
            if prof.identity.revision_id != contract_rev:
                errors.append(
                    format_error(
                        f"{name}: emulator PCI profile revision mismatch ({AERO_DEVICES_PCI_PROFILE_RS.as_posix()}):",
                        [
                            f"expected: 0x{contract_rev:02X}",
                            f"got: 0x{prof.identity.revision_id:02X}",
                        ],
                    )
                )
            doc = table_ids.get(name)
            if doc and doc.class_code != prof.class_code:
                errors.append(
                    format_error(
                        f"{name}: class code mismatch between docs and emulator PCI profile:",
                        [
                            f"{WINDOWS_DEVICE_CONTRACT_MD.as_posix()}: {format_pci_class_code(doc.class_code)}",
                            f"{AERO_DEVICES_PCI_PROFILE_RS.as_posix()}: {format_pci_class_code(prof.class_code)}",
                        ],
                    )
                )

            expected_header = 0x80 if name == "virtio-input (keyboard)" else 0x00
            if prof.header_type != expected_header:
                errors.append(
                    format_error(
                        f"{name}: PCI header_type mismatch in emulator PCI profile:",
                        [
                            f"expected: 0x{expected_header:02X}",
                            f"got: 0x{prof.header_type:02X}",
                            f"file: {AERO_DEVICES_PCI_PROFILE_RS.as_posix()}",
                        ],
                    )
                )

        # virtio-input must be exposed as a multi-function PCI device: keyboard on function 0
        # (with the multi-function bit set) and mouse on function 1.
        kbd = profile_map.get("virtio-input (keyboard)")
        mouse = profile_map.get("virtio-input (mouse)")
        if kbd and mouse:
            kb_bus, kb_dev, kb_fun = kbd.bdf
            ms_bus, ms_dev, ms_fun = mouse.bdf
            if (kb_bus, kb_dev) != (ms_bus, ms_dev) or kb_fun != 0 or ms_fun != 1:
                errors.append(
                    format_error(
                        "virtio-input: emulator PCI profiles must be multi-function (same bus/device; keyboard fn0; mouse fn1):",
                        [
                            f"keyboard bdf: {kb_bus:02x}:{kb_dev:02x}.{kb_fun}",
                            f"mouse bdf:    {ms_bus:02x}:{ms_dev:02x}.{ms_fun}",
                            f"file: {AERO_DEVICES_PCI_PROFILE_RS.as_posix()}",
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

    # ---------------------------------------------------------------------
    # 5) Win7 driver INFs must follow strict contract-v1 identity policy.
    #
    # The AERO-W7-VIRTIO contract major version is encoded in PCI Revision ID.
    # Some drivers additionally enforce this at runtime, but if the INF is not
    # revision-gated Windows can still install the driver against a non-contract
    # device and then fail to start (Code 10). Enforce REV gating here to avoid
    # "driver installs but won't start" confusion.
    # ---------------------------------------------------------------------
    for device_name, inf_path in WIN7_VIRTIO_DRIVER_INFS.items():
        if not inf_path.exists():
            errors.append(f"missing expected Win7 virtio driver INF: {inf_path.as_posix()}")
            continue

        hwids = parse_inf_hardware_ids(inf_path)
        if not hwids:
            errors.append(f"{inf_path.as_posix()}: could not find any active PCI hardware IDs (PCI\\VEN_... lines)")
            continue

        # Guard against accidental transitional virtio-pci IDs in INFs.
        transitional_hwids: list[str] = []
        for hwid in hwids:
            parsed = parse_pci_vendor_device_from_hwid(hwid)
            if not parsed:
                continue
            ven, dev = parsed
            if ven != VIRTIO_PCI_VENDOR_ID:
                continue
            if VIRTIO_PCI_TRANSITIONAL_DEVICE_ID_MIN <= dev <= VIRTIO_PCI_TRANSITIONAL_DEVICE_ID_MAX:
                transitional_hwids.append(hwid)
        if transitional_hwids:
            errors.append(
                format_error(
                    f"{inf_path.as_posix()}: INF references transitional virtio-pci device IDs (out of scope for AERO-W7-VIRTIO v1):",
                    [f"hwid: {h}" for h in sorted(transitional_hwids)],
                )
            )

        # virtio-input uses one INF for both keyboard and mouse functions.
        if device_name == "virtio-input":
            contract_any = contract_ids["virtio-input (keyboard)"]
        else:
            contract_any = contract_ids[device_name]

        base_hwid = f"PCI\\VEN_{contract_any.vendor_id:04X}&DEV_{contract_any.device_id:04X}"
        strict_hwid = f"{base_hwid}&REV_{contract_rev:02X}"
        hwids_upper = {h.upper() for h in hwids}
        if strict_hwid.upper() not in hwids_upper:
            errors.append(
                format_error(
                    f"{inf_path.as_posix()}: missing strict REV-qualified hardware ID (required for contract major safety):",
                    [
                        f"expected: {strict_hwid}",
                        f"got: {sorted(hwids)}",
                    ],
                )
            )

        relevant = [h for h in hwids if h.upper().startswith(base_hwid.upper())]
        missing_rev: list[str] = []
        wrong_rev: list[str] = []
        for hwid in relevant:
            m = re.search(r"&REV_(?P<rev>[0-9A-Fa-f]{2})", hwid, flags=re.I)
            if not m:
                missing_rev.append(hwid)
                continue
            rev = int(m.group("rev"), 16)
            if rev != contract_rev:
                wrong_rev.append(hwid)

        if missing_rev:
            errors.append(
                format_error(
                    f"{inf_path.as_posix()}: INF matches {base_hwid} without revision gating (must require REV_{contract_rev:02X}):",
                    [f"hwid: {h}" for h in sorted(missing_rev)],
                )
            )
        if wrong_rev:
            errors.append(
                format_error(
                    f"{inf_path.as_posix()}: INF REV_ qualifier does not match contract major version:",
                    [
                        f"expected: REV_{contract_rev:02X}",
                        *[f"hwid: {h}" for h in sorted(wrong_rev)],
                    ],
                )
            )

    if errors:
        print("\n\n".join(errors), file=sys.stderr)
        raise SystemExit(1)


if __name__ == "__main__":
    main()
