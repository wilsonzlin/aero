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

import difflib
import json
import re
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path, PureWindowsPath
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
WIN7_TEST_IMAGE_PS1 = WIN7_VIRTIO_TESTS_ROOT / "host-harness/New-AeroWin7TestImage.ps1"
WIN7_VIRTIO_GUEST_SELFTEST_MAIN_CPP = WIN7_VIRTIO_TESTS_ROOT / "guest-selftest/src/main.cpp"
INSTRUCTIONS_ROOT = REPO_ROOT / "instructions"
DEPRECATED_WIN7_TEST_INF_BASENAMES: tuple[str, ...] = (
    # Pre-rename INF basenames.
    "aerovblk.inf",
    "aerovnet.inf",
    # Old virtio-snd INF basename (hyphenated); canonical is now aero_virtio_snd.inf.
    "aero-virtio-snd.inf",
    # virtio-input retains a legacy filename alias, but tests/instructions should avoid spelling out the
    # legacy basename (it encourages cargo-culting old names). If the alias is needed in those docs, refer
    # to it generically (e.g. "the `*.inf.disabled` file; drop the `.disabled` suffix to enable").
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

# Minimum MessageNumberLimit per device so the driver can use a dedicated config
# interrupt plus at least one interrupt per critical virtqueue.
#
# These are intentionally conservative lower bounds; INFs may request a larger
# "future-proof" number and Windows may still grant fewer messages at runtime.
WIN7_VIRTIO_INF_MIN_MESSAGE_NUMBER_LIMIT: Mapping[str, int] = {
    "virtio-blk": 2,  # config + queue0
    "virtio-net": 3,  # config + rx + tx
    "virtio-input": 3,  # config + eventq + statusq
    "virtio-snd": 5,  # config + 4 queues
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

# Transitional / legacy driver packages are out-of-scope for AERO-W7-VIRTIO v1,
# but we still keep them consistent with in-guest tooling (selftest) so that
# optional transitional paths remain debuggable.
AERO_VIRTIO_SND_LEGACY_INF = REPO_ROOT / "drivers/windows7/virtio-snd/inf/aero-virtio-snd-legacy.inf"


def fail(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(1)


def format_error(header: str, lines: Iterable[str]) -> str:
    body = "\n".join(f"  {line}" for line in lines)
    return f"{header}\n{body}"


def read_text(path: Path) -> str:
    try:
        data = path.read_bytes()
    except FileNotFoundError:
        fail(f"missing required file: {path.as_posix()}")

    # Prefer strict UTF-8 for repository text, but allow UTF-16 (a common INF
    # encoding) when detected.
    #
    # Note: UTF-16LE without a BOM will decode as UTF-8 without error but produce
    # NUL-padded text. Detect that via byte-level heuristics.

    # BOM handling.
    if data.startswith(b"\xff\xfe") or data.startswith(b"\xfe\xff"):
        try:
            return data.decode("utf-16").lstrip("\ufeff")
        except UnicodeDecodeError:
            fail(f"{path.as_posix()}: failed to decode file as UTF-16 (has BOM)")

    # Try UTF-8 first.
    utf8_text: str | None
    try:
        utf8_text = data.decode("utf-8")
    except UnicodeDecodeError:
        utf8_text = None
    else:
        # Fast-path: no NULs means we almost certainly decoded correctly.
        if "\x00" not in utf8_text:
            # Be tolerant of UTF-8 BOMs produced by some editors/tools.
            return utf8_text.lstrip("\ufeff")

    # Heuristic detection for UTF-16 without BOM.
    # Plain ASCII UTF-16LE will have many 0x00 bytes at odd indices; UTF-16BE will
    # have many 0x00 bytes at even indices.
    sample = data[:4096]
    sample_len = len(sample)
    even_zeros = sum(1 for i, b in enumerate(sample) if b == 0 and (i % 2) == 0)
    odd_zeros = sum(1 for i, b in enumerate(sample) if b == 0 and (i % 2) == 1)

    enc: str | None = None
    if sample_len >= 2:
        even_ratio = even_zeros / sample_len
        odd_ratio = odd_zeros / sample_len
        if (odd_zeros > (even_zeros * 4 + 10)) or (odd_ratio > 0.2 and even_ratio < 0.05):
            enc = "utf-16-le"
        elif (even_zeros > (odd_zeros * 4 + 10)) or (even_ratio > 0.2 and odd_ratio < 0.05):
            enc = "utf-16-be"

    if enc is not None:
        try:
            return data.decode(enc).lstrip("\ufeff")
        except UnicodeDecodeError:
            fail(f"{path.as_posix()}: failed to decode file as {enc} (heuristic)")

    if utf8_text is not None:
        fail(f"{path.as_posix()}: decoded as UTF-8 but contained NUL bytes (likely UTF-16 without BOM)")
    fail(f"{path.as_posix()}: failed to decode file as UTF-8 (unexpected encoding)")


def strip_inf_comment_lines(text: str) -> str:
    """
    Remove INF comments from `text`.

    - Drops full-line comments (optional whitespace then ';').
    - Strips inline comments using the same quote-aware rules as the other INF
      helpers in this script (semicolons inside quoted strings are data).
    """

    out: list[str] = []
    for raw in text.splitlines():
        line = _strip_inf_inline_comment(raw).strip()
        if not line:
            continue
        out.append(line)
    return "\n".join(out)

def inf_functional_bytes(path: Path) -> bytes:
    """
    Return the INF content starting from the first section header line.

    This intentionally ignores the leading comment/banner block so a legacy alias
    INF can use a different filename header while still enforcing byte-for-byte
    equality of all functional sections/keys.
    """

    data = path.read_bytes()
    lines = data.splitlines(keepends=True)

    def _first_nonblank_ascii_byte(*, line: bytes, first_line: bool) -> int | None:
        """
        Return the first meaningful ASCII byte on a line, ignoring whitespace and NULs.

        This makes the section-header scan robust to UTF-16LE/BE INFs (with or
        without a BOM), where each ASCII character is separated by a NUL byte.
        """

        if first_line:
            # Strip BOMs for *detection only*. Returned content still includes them.
            if line.startswith(b"\xef\xbb\xbf"):
                line = line[3:]
            elif line.startswith(b"\xff\xfe") or line.startswith(b"\xfe\xff"):
                line = line[2:]

        for b in line:
            if b in (0x00, 0x09, 0x0A, 0x0D, 0x20):  # NUL, tab, LF, CR, space
                continue
            return b
        return None

    for i, line in enumerate(lines):
        first = _first_nonblank_ascii_byte(line=line, first_line=(i == 0))
        if first is None:
            continue

        # First section header (e.g. "[Version]") starts the functional region.
        if first == ord("["):
            return b"".join(lines[i:])

        # Ignore leading comments.
        if first == ord(";"):
            continue

        # Unexpected preamble content (not comment, not blank, not section):
        # treat it as functional to avoid masking drift.
        return b"".join(lines[i:])

    raise RuntimeError(f"{path}: could not find a section header (e.g. [Version])")


def check_inf_alias_drift(*, canonical: Path, alias: Path, repo_root: Path, label: str) -> str | None:
    """
    Compare the canonical INF against its legacy filename alias.

    Policy: the alias INF is a filename alias only. From the first section header
    (typically `[Version]`) onward, it must be byte-for-byte identical to the
    canonical INF. Only the leading banner/comment block may differ.
    """

    try:
        canonical_body = inf_functional_bytes(canonical)
    except Exception as e:
        return f"{label}: failed to read canonical INF functional bytes: {e}"

    try:
        alias_body = inf_functional_bytes(alias)
    except Exception as e:
        return f"{label}: failed to read alias INF functional bytes: {e}"

    if canonical_body == alias_body:
        return None

    canonical_label = str(canonical.relative_to(repo_root))
    alias_label = str(alias.relative_to(repo_root))

    def _decode_lines_for_diff(data: bytes) -> list[str]:
        if data.startswith(b"\xff\xfe") or data.startswith(b"\xfe\xff"):
            text = data.decode("utf-16", errors="replace").lstrip("\ufeff")
        elif data.startswith(b"\xef\xbb\xbf"):
            text = data.decode("utf-8-sig", errors="replace")
        else:
            text = data.decode("utf-8", errors="replace")
            # If this was UTF-16 without a BOM, it will look like NUL-padded UTF-8.
            if "\x00" in text:
                text = text.replace("\x00", "")
        return text.splitlines(keepends=True)

    canonical_lines = _decode_lines_for_diff(canonical_body)
    alias_lines = _decode_lines_for_diff(alias_body)

    diff = difflib.unified_diff(
        canonical_lines,
        alias_lines,
        fromfile=canonical_label,
        tofile=alias_label,
        lineterm="\n",
    )

    return f"{label}: INF alias drift detected (expected byte-identical from the first section header onward):\n" + "".join(
        diff
    )


def strip_inf_sections_bytes(data: bytes, *, drop_sections: set[str]) -> bytes:
    """Remove entire INF sections (including their headers) by name (case-insensitive).

    This is a byte-preserving transform: it does not normalize whitespace or line
    endings. It is used to allow controlled divergence in specific sections
    (e.g. virtio-input models sections) while still enforcing byte-for-byte
    identity everywhere else.
    """

    drop = {s.lower() for s in drop_sections}
    out: list[bytes] = []
    skipping = False

    for line in data.splitlines(keepends=True):
        # Support UTF-16LE/BE INFs by stripping NUL bytes for detection only.
        line_ascii = line.replace(b"\x00", b"")
        stripped = line_ascii.lstrip(b" \t")
        if stripped.startswith(b"[") and b"]" in stripped:
            end = stripped.find(b"]")
            name = stripped[1:end].strip().decode("utf-8", errors="replace").lower()
            skipping = name in drop
        if skipping:
            continue
        out.append(line)

    return b"".join(out)


def check_inf_alias_drift_excluding_sections_bytes(
    *, canonical: Path, alias: Path, repo_root: Path, label: str, drop_sections: set[str]
) -> str | None:
    """
    Compare the canonical INF against its legacy filename alias while excluding specific sections.

    Policy: from the first section header onward, the alias must be byte-for-byte
    identical to the canonical INF outside the excluded sections. Only the
    leading banner/comment block may differ.
    """

    try:
        canonical_body = strip_inf_sections_bytes(inf_functional_bytes(canonical), drop_sections=drop_sections)
        alias_body = strip_inf_sections_bytes(inf_functional_bytes(alias), drop_sections=drop_sections)
    except Exception as e:
        return f"{label}: failed to read INF functional bytes: {e}"

    if canonical_body == alias_body:
        return None

    canonical_label = str(canonical.relative_to(repo_root))
    alias_label = str(alias.relative_to(repo_root))

    def _decode_lines_for_diff(data: bytes) -> list[str]:
        if data.startswith(b"\xff\xfe") or data.startswith(b"\xfe\xff"):
            text = data.decode("utf-16", errors="replace").lstrip("\ufeff")
        elif data.startswith(b"\xef\xbb\xbf"):
            text = data.decode("utf-8-sig", errors="replace")
        else:
            text = data.decode("utf-8", errors="replace")
            # If this was UTF-16 without a BOM, it will look like NUL-padded UTF-8.
            if "\x00" in text:
                text = text.replace("\x00", "")
        return text.splitlines(keepends=True)

    ignored = sorted(drop_sections)
    ignored_line = f"Ignored sections: {ignored}\n\n" if ignored else ""

    diff = difflib.unified_diff(
        _decode_lines_for_diff(canonical_body),
        _decode_lines_for_diff(alias_body),
        fromfile=canonical_label,
        tofile=alias_label,
        lineterm="\n",
    )

    return f"{label}: INF alias drift detected outside ignored sections:\n" + ignored_line + "".join(diff)


def parse_contract_major_version(md: str) -> int:
    m = re.search(r"^\*\*Contract version:\*\*\s*`(?P<major>\d+)\.", md, flags=re.M)
    if not m:
        fail(f"could not parse contract major version from {W7_VIRTIO_CONTRACT_MD.as_posix()}")
    return int(m.group("major"), 10)


def scan_text_tree_for_substrings(root: Path, needles: Iterable[str]) -> list[str]:
    hits: list[str] = []
    # Windows filenames are case-insensitive, and docs often vary casing. Treat the
    # needle scan as case-insensitive so we don't accidentally miss deprecated INF
    # basenames due to casing differences (e.g. `AEROVBLK.INF` vs `aerovblk.inf`).
    needles = tuple(needles)
    needles_lower = tuple(n.lower() for n in needles)
    for path in sorted(root.rglob("*")):
        if not path.is_file():
            continue
        try:
            data = path.read_bytes()
        except OSError:
            continue
        # Best-effort decoding: many Windows-authored scripts are UTF-16LE (sometimes
        # without a BOM). We want this doc/test scanning guardrail to still catch
        # deprecated INF basenames in those files.
        if data.startswith(b"\xff\xfe") or data.startswith(b"\xfe\xff"):
            try:
                text = data.decode("utf-16", errors="replace").lstrip("\ufeff")
            except UnicodeDecodeError:
                continue
        else:
            text = data.decode("utf-8", errors="replace")
            if "\x00" in text:
                sample = data[:4096]
                sample_len = len(sample)
                even_zeros = sum(1 for i, b in enumerate(sample) if b == 0 and (i % 2) == 0)
                odd_zeros = sum(1 for i, b in enumerate(sample) if b == 0 and (i % 2) == 1)
                enc: str | None = None
                if sample_len >= 2:
                    even_ratio = even_zeros / sample_len
                    odd_ratio = odd_zeros / sample_len
                    if (odd_zeros > (even_zeros * 4 + 10)) or (odd_ratio > 0.2 and even_ratio < 0.05):
                        enc = "utf-16-le"
                    elif (even_zeros > (odd_zeros * 4 + 10)) or (even_ratio > 0.2 and odd_ratio < 0.05):
                        enc = "utf-16-be"
                if enc is not None:
                    try:
                        text = data.decode(enc, errors="replace").lstrip("\ufeff")
                    except UnicodeDecodeError:
                        pass
        text_lower = text.lower()
        if not any(needle in text_lower for needle in needles_lower):
            continue
        for line_no, line in enumerate(text.splitlines(), start=1):
            line_lower = line.lower()
            for needle, needle_lower in zip(needles, needles_lower):
                if needle_lower in line_lower:
                    hits.append(f"{path.as_posix()}:{line_no}: contains {needle!r}")
    return hits


def parse_powershell_array_strings(*, text: str, var_name: str, file: Path) -> list[str]:
    """
    Parse a simple PowerShell array assignment like:

        $VarName = @(
          "foo",
          "bar"
        )

    Returns the list of string literals (without quotes).

    This intentionally only supports a constrained subset of PowerShell syntax
    sufficient for CI guardrails (not a full PS parser).
    """

    start_re = re.compile(rf"^\s*\${re.escape(var_name)}\s*=\s*@\(\s*$", flags=re.I)
    end_re = re.compile(r"^\s*\)\s*$")
    simple_str_re = re.compile(r'^\s*(?P<q>["\'])(?P<val>[^"\']+)(?P=q)\s*,?\s*(?:#.*)?$')

    values: list[str] = []
    in_list = False
    start_line: int | None = None

    for line_no, raw in enumerate(text.splitlines(), start=1):
        if not in_list:
            if start_re.match(raw):
                in_list = True
                start_line = line_no
            continue

        if end_re.match(raw):
            return values

        stripped = raw.strip()
        if not stripped or stripped.startswith("#"):
            continue

        m = simple_str_re.match(raw)
        if m:
            values.append(m.group("val"))
            continue

        # Support multiple string literals on one line (unusual but valid PowerShell).
        code = raw.split("#", 1)[0]
        found = [m.group("val") for m in re.finditer(r'(?P<q>["\'])(?P<val>[^"\']+)(?P=q)', code)]
        if found:
            values.extend(found)
            continue

        raise ValueError(
            f"{file.as_posix()}:{line_no}: could not parse PowerShell array element for ${var_name}: {raw!r}"
        )

    if in_list:
        raise ValueError(f"{file.as_posix()}:{start_line}: PowerShell array for ${var_name} is missing a closing ')'")
    raise ValueError(f"{file.as_posix()}: missing required PowerShell array assignment for ${var_name}")


def inf_allowlist_entry_basename(entry: str) -> str:
    # Allowlist entries can be either INF basenames or paths under -DriversDir.
    # Normalize into a Windows basename for comparisons.
    return PureWindowsPath(entry.replace("/", "\\")).name


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


@dataclass(frozen=True)
class LocatedString:
    value: str
    file: Path
    line_no: int
    line: str

    def format_location(self) -> str:
        return f"{self.file.as_posix()}:{self.line_no}: {self.line.strip()}"


def parse_inf_addservice_entries(path: Path) -> list[LocatedString]:
    """
    Parse `AddService = <name>, ...` lines from an INF.

    This is intentionally lightweight (line-based, comment-tolerant) and only
    intended for contract/CI drift checks.
    """

    text = read_text(path)
    out: list[LocatedString] = []
    for line_no, raw in enumerate(text.splitlines(), start=1):
        active = _strip_inf_inline_comment(raw).strip()
        if not active:
            continue

        m = re.match(r'^\s*AddService\s*=\s*"?([^",\s]+)"?\s*(?:,|$)', active, flags=re.I)
        if not m:
            continue
        out.append(LocatedString(value=m.group(1), file=path, line_no=line_no, line=raw.rstrip()))

    return out


def parse_guest_selftest_expected_service_names_text(*, text: str, file: Path) -> Mapping[str, LocatedString]:
    """
    Extract hardcoded expected Windows service names from the Win7 guest selftest.

    We keep parsing intentionally regex-based (no C++ parser) but reasonably
    specific to the current source patterns so drift is caught early.
    """

    patterns: Mapping[str, re.Pattern[str]] = {
        # virtio-net binding check inside VirtioNetTest().
        # Accept either `kExpectedService = L"..."` or `kExpectedService[] = L"..."`.
        "virtio-net": re.compile(r'\bkExpectedService\b\s*(?:\[\s*\])?\s*=\s*L"(?P<svc>[^"]+)"'),
        # virtio-snd modern / transitional service name expectations.
        "virtio-snd": re.compile(
            r'\bkVirtioSndExpectedServiceModern\b\s*(?:\[\s*\])?\s*=\s*L"(?P<svc>[^"]+)"'
        ),
        "virtio-snd-transitional": re.compile(
            r'\bkVirtioSndExpectedServiceTransitional\b\s*(?:\[\s*\])?\s*=\s*L"(?P<svc>[^"]+)"'
        ),
        # virtio-input expected service name (PCI binding validation).
        # Accept both pointer + array style declarations:
        #   static constexpr const wchar_t* kVirtioInputExpectedService = L"...";
        #   static constexpr const wchar_t kVirtioInputExpectedService[] = L"...";
        "virtio-input": re.compile(r'\bkVirtioInputExpectedService\b\s*(?:\[\s*\])?\s*=\s*L"(?P<svc>[^"]+)"'),
    }

    out: dict[str, LocatedString] = {}
    for line_no, line in enumerate(text.splitlines(), start=1):
        for key, pat in patterns.items():
            if key in out:
                continue
            m = pat.search(line)
            if not m:
                continue
            out[key] = LocatedString(value=m.group("svc"), file=file, line_no=line_no, line=line.rstrip())
    return out


def parse_guest_selftest_expected_service_names(path: Path) -> Mapping[str, LocatedString]:
    return parse_guest_selftest_expected_service_names_text(text=read_text(path), file=path)


def _self_test_parse_guest_selftest_expected_service_names() -> None:
    sample = r"""
static const wchar_t kExpectedService[] = L"aero_virtio_net";
static constexpr const wchar_t* kVirtioSndExpectedServiceModern = L"aero_virtio_snd";
static constexpr const wchar_t* kVirtioSndExpectedServiceTransitional = L"aeroviosnd_legacy";
static constexpr const wchar_t* kVirtioInputExpectedService = L"aero_virtio_input";
"""
    parsed = parse_guest_selftest_expected_service_names_text(text=sample, file=Path("<unit-test>"))
    expected = {
        "virtio-net": "aero_virtio_net",
        "virtio-snd": "aero_virtio_snd",
        "virtio-snd-transitional": "aeroviosnd_legacy",
        "virtio-input": "aero_virtio_input",
    }
    for key, value in expected.items():
        got = parsed.get(key)
        if got is None:
            fail(f"internal unit-test failed: expected guest-selftest to contain {key!r} service name")
        if got.value != value:
            fail(
                f"internal unit-test failed: guest-selftest service name mismatch for {key}: expected {value!r}, got {got.value!r}"
            )


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
        line = _strip_inf_inline_comment(raw).strip()
        if not line:
            continue
        parts = _split_inf_csv_fields(line)
        # Model lines can optionally include additional IDs after the primary HWID.
        # Collect every comma-separated field that looks like a PCI HWID so we don't
        # accidentally miss the strict REV-qualified match (and so we can fail fast
        # if any extra IDs violate revision gating policy).
        for part in parts:
            token = _unquote_inf_token(part).strip()
            if token.upper().startswith("PCI\\VEN_"):
                out.add(token)
    return out


@dataclass(frozen=True)
class InfModelEntry:
    section: str
    device_desc: str
    install_section: str
    hardware_id: str
    raw_line: str


def parse_inf_model_entries(path: Path) -> list[InfModelEntry]:
    """
    Parse INF model entries of the form:

        %Some.DeviceDesc% = Some_Install.Section, PCI\\VEN_....

    We keep parsing intentionally lightweight and tolerant: this is only used for
    contract drift/guardrail checks (not a full INF parser).
    """

    text = read_text(path)
    entries: list[InfModelEntry] = []
    current_section: str | None = None
    for raw in text.splitlines():
        line = _strip_inf_inline_comment(raw).strip()
        if not line:
            continue
        m = re.match(r"^\[(?P<section>[^\]]+)\]\s*$", line)
        if m:
            current_section = m.group("section").strip()
            continue
        if current_section is None:
            continue
        # We only care about "models" style entries that reference a PCI HWID.
        if "=" not in line:
            continue
        device_desc, rhs = (s.strip() for s in line.split("=", 1))
        if not device_desc or not rhs:
            continue
        rhs_parts = [p.strip() for p in _split_inf_csv_fields(rhs) if p.strip()]
        if not rhs_parts:
            continue
        install = _unquote_inf_token(rhs_parts[0]).strip()
        hwid = next(
            (_unquote_inf_token(p).strip() for p in rhs_parts[1:] if _unquote_inf_token(p).strip().upper().startswith("PCI\\VEN_")),
            None,
        )
        if not hwid:
            continue
        entries.append(
            InfModelEntry(
                section=current_section,
                device_desc=device_desc,
                install_section=install,
                hardware_id=hwid,
                raw_line=line,
            )
        )
    return entries


def parse_inf_string_keys(path: Path) -> set[str]:
    """
    Extract the set of keys defined in the INF's [Strings] section.

    This is intentionally a minimal parser: it is only used for CI drift checks
    (we just need to know whether a %Token% referenced by a model line is
    actually defined).
    """

    text = read_text(path)
    keys: set[str] = set()
    current_section: str | None = None
    for raw in text.splitlines():
        line = _strip_inf_inline_comment(raw).strip()
        if not line:
            continue
        m = re.match(r"^\[(?P<section>[^\]]+)\]\s*$", line)
        if m:
            current_section = m.group("section").strip()
            continue
        if current_section is None:
            continue
        if not current_section.lower().startswith("strings"):
            continue
        if "=" not in line:
            continue
        key = line.split("=", 1)[0].strip()
        if key:
            keys.add(key.lower())
    return keys


def parse_inf_strings_map(path: Path) -> dict[str, str]:
    """
    Parse the INF [Strings] section into a key->value mapping.

    Keys are normalized to lowercase. Values are returned unquoted (i.e. surrounding
    double-quotes are removed when present).
    """

    text = read_text(path)
    out: dict[str, str] = {}
    current_section: str | None = None
    for raw in text.splitlines():
        line = _strip_inf_inline_comment(raw).strip()
        if not line:
            continue
        m = re.match(r"^\[(?P<section>[^\]]+)\]\s*$", line)
        if m:
            current_section = m.group("section").strip()
            continue
        if current_section is None:
            continue
        if not current_section.lower().startswith("strings"):
            continue
        if "=" not in line:
            continue
        key, value = (s.strip() for s in line.split("=", 1))
        if not key:
            continue
        if value.startswith('"') and value.endswith('"') and len(value) >= 2:
            value = value[1:-1]
        out[key.lower()] = value
    return out

def _strip_inf_inline_comment(line: str) -> str:
    """
    Strip an INF inline comment.

    INF comments start with `;` and run to end-of-line. Semicolons inside a quoted
    string literal are treated as data, not a comment delimiter.
    """

    in_quotes = False
    for i, ch in enumerate(line):
        if ch == '"':
            in_quotes = not in_quotes
            continue
        if ch == ";" and not in_quotes:
            return line[:i]
    return line


def _split_inf_csv_fields(line: str) -> list[str]:
    """
    Split a single INF directive line into comma-separated fields.

    Commas inside quoted strings are preserved. Quotes are not removed.
    """

    parts: list[str] = []
    cur: list[str] = []
    in_quotes = False
    for ch in line:
        if ch == '"':
            in_quotes = not in_quotes
            cur.append(ch)
            continue
        if ch == "," and not in_quotes:
            parts.append("".join(cur).strip())
            cur = []
            continue
        cur.append(ch)
    parts.append("".join(cur).strip())
    # Drop trailing empty fields caused by a stray trailing comma.
    while parts and parts[-1] == "":
        parts.pop()
    return parts


def _self_test_inf_inline_comment_stripping() -> None:
    # Semicolons inside quoted strings are data, not comment delimiters.
    line = 'Foo = "Aero; Project"; trailing comment'
    stripped = _strip_inf_inline_comment(line).strip()
    if stripped != 'Foo = "Aero; Project"':
        fail(
            format_error(
                "internal unit-test failed: _strip_inf_inline_comment mis-handled semicolons inside quotes:",
                [f"input:    {line!r}", f"got:      {stripped!r}", 'expected: \'Foo = "Aero; Project"\''],
            )
        )

    sample_inf = r"""
[Strings]
Foo = "Aero; Project"; comment
"""
    with tempfile.TemporaryDirectory() as td:
        path = Path(td) / "sample.inf"
        path.write_text(sample_inf, encoding="utf-8")
        parsed = parse_inf_strings_map(path)
        if parsed.get("foo") != "Aero; Project":
            fail(
                format_error(
                    "internal unit-test failed: parse_inf_strings_map mis-handled semicolons inside quoted values:",
                    [f"got: {parsed.get('foo')!r}", "expected: 'Aero; Project'"],
                )
            )


def _self_test_inf_parsers_ignore_comments() -> None:
    sample_inf = r"""
[Version]
Signature="$WINDOWS NT$"

[Manufacturer]
%Mfg% = Mfg,NTx86

[Mfg.NTx86]
%Dev% = Install, "PCI\VEN_1AF4&DEV_1041&REV_01" ; trailing comment

[Install.Services]
; AddService = ignored, 0x00000002, IgnoredSection
AddService = foo, 0x00000002, FooSection ; trailing comment
AddService = "bar", 0x00000002, "Section;Name" ; comment after quoted semicolon

[Strings]
Mfg = "Aero; Project"; comment
"""
    with tempfile.TemporaryDirectory() as td:
        path = Path(td) / "sample.inf"
        path.write_text(sample_inf, encoding="utf-8")

        hwids = parse_inf_hardware_ids(path)
        expected_hwid = r"PCI\VEN_1AF4&DEV_1041&REV_01"
        if expected_hwid not in hwids:
            fail(
                format_error(
                    "internal unit-test failed: parse_inf_hardware_ids did not return expected HWID:",
                    [f"expected: {expected_hwid}", f"got: {sorted(hwids)}"],
                )
            )

        models = parse_inf_model_entries(path)
        if not any(e.hardware_id == expected_hwid for e in models):
            fail(
                format_error(
                    "internal unit-test failed: parse_inf_model_entries did not return expected model entry:",
                    [f"expected to find hardware_id {expected_hwid!r}", f"got entries: {[e.hardware_id for e in models]}"],
                )
            )

        services = [s.value for s in parse_inf_addservice_entries(path)]
        if services != ["foo", "bar"]:
            fail(
                format_error(
                    "internal unit-test failed: parse_inf_addservice_entries mismatch:",
                    [f"expected: ['foo', 'bar']", f"got:      {services!r}"],
                )
            )


def _unquote_inf_token(token: str) -> str:
    token = token.strip()
    if len(token) >= 2 and token.startswith('"') and token.endswith('"'):
        return token[1:-1]
    return token


def _normalize_inf_reg_subkey(subkey: str) -> str:
    """
    Normalize an INF AddReg subkey for robust matching.

    - trims whitespace/quotes
    - collapses repeated backslashes (some INFs mistakenly use `\\`)
    - case-folds for comparison
    """

    subkey = _unquote_inf_token(subkey).strip()
    subkey = re.sub(r"\\+", r"\\", subkey)
    return subkey.lower()


def _normalize_inf_reg_value_name(value_name: str) -> str:
    return _unquote_inf_token(value_name).strip().lower()


def _try_parse_inf_int(token: str) -> int | None:
    token = _unquote_inf_token(token).strip()
    if not token:
        return None
    try:
        # INF numeric fields are typically either:
        # - hex literals with a 0x prefix (e.g. 0x00010001)
        # - base-10 integers (sometimes with leading zeros)
        #
        # Python's int(..., 0) rejects leading-zero integers like "08", so parse
        # decimal explicitly when no prefix is present.
        if token.lower().startswith("0x"):
            return int(token, 16)
        return int(token, 10)
    except ValueError:
        return None


def _self_test_inf_int_parsing() -> None:
    # INF files sometimes use decimal values with leading zeros. Ensure we accept
    # those (Python's int(..., 0) rejects them).
    if _try_parse_inf_int("08") != 8:
        fail("internal unit-test failed: expected _try_parse_inf_int('08') == 8")
    if _try_parse_inf_int("0x08") != 8:
        fail("internal unit-test failed: expected _try_parse_inf_int('0x08') == 8")
    if _try_parse_inf_int("0x00000010") != 16:
        fail("internal unit-test failed: expected _try_parse_inf_int('0x00000010') == 16")


def _self_test_read_text_utf16() -> None:
    sample = "[Version]\nSignature=\"$WINDOWS NT$\"\n"
    with tempfile.TemporaryDirectory() as td:
        p_bom = Path(td) / "utf16-bom.txt"
        p_bom.write_bytes(b"\xff\xfe" + sample.encode("utf-16-le"))
        got_bom = read_text(p_bom)
        if got_bom != sample:
            fail(
                format_error(
                    "internal unit-test failed: read_text did not decode UTF-16LE with BOM correctly:",
                    [f"got:      {got_bom!r}", f"expected: {sample!r}"],
                )
            )

        p_no_bom = Path(td) / "utf16-no-bom.txt"
        p_no_bom.write_bytes(sample.encode("utf-16-le"))
        got_no_bom = read_text(p_no_bom)
        if got_no_bom != sample:
            fail(
                format_error(
                    "internal unit-test failed: read_text did not decode UTF-16LE without BOM correctly:",
                    [f"got:      {got_no_bom!r}", f"expected: {sample!r}"],
                )
            )


def _self_test_scan_text_tree_for_substrings_utf16() -> None:
    needle = "aerovblk.inf"
    with tempfile.TemporaryDirectory() as td:
        root = Path(td)
        # UTF-16LE without BOM (common in Windows-authored scripts).
        path = root / "sample.ps1"
        path.write_bytes(f"Write-Host '{needle}'\n".encode("utf-16-le"))

        hits = scan_text_tree_for_substrings(root, (needle,))
        if not hits:
            fail("internal unit-test failed: scan_text_tree_for_substrings did not find needle in UTF-16LE file")


def _self_test_inf_functional_bytes_utf16() -> None:
    """
    Ensure inf_functional_bytes/check_inf_alias_drift remain robust when INFs are
    UTF-16 encoded (a common Windows INF encoding).

    The INF alias drift checks are intentionally allowed to ignore only the
    leading banner/comments block. They must still find the real first section
    header in UTF-16 (with or without a BOM).
    """

    canonical_header = "; canonical banner\r\n; line2\r\n\r\n"
    alias_header = "; alias banner\r\n\r\n"
    body = "[Version]\r\nSignature=\"$WINDOWS NT$\"\r\n\r\n[Foo]\r\nBar=baz\r\n"

    with tempfile.TemporaryDirectory() as td:
        repo_root = Path(td)
        canonical = repo_root / "canonical.inf"
        alias = repo_root / "alias.inf"

        # UTF-16 with BOM.
        canonical.write_bytes((canonical_header + body).encode("utf-16"))
        alias.write_bytes((alias_header + body).encode("utf-16"))

        cb = inf_functional_bytes(canonical)
        ab = inf_functional_bytes(alias)
        if cb != ab:
            fail("internal unit-test failed: inf_functional_bytes mismatch for UTF-16 INFs with different banners")

        drift = check_inf_alias_drift(canonical=canonical, alias=alias, repo_root=repo_root, label="utf16-bom")
        if drift is not None:
            fail(format_error("internal unit-test failed: check_inf_alias_drift unexpectedly reported drift:", [drift]))

    with tempfile.TemporaryDirectory() as td:
        repo_root = Path(td)
        canonical = repo_root / "canonical-nobom.inf"
        alias = repo_root / "alias-nobom.inf"

        # UTF-16LE without BOM.
        canonical.write_bytes((canonical_header + body).encode("utf-16-le"))
        alias.write_bytes((alias_header + body).encode("utf-16-le"))

        cb = inf_functional_bytes(canonical)
        ab = inf_functional_bytes(alias)
        if cb != ab:
            fail(
                "internal unit-test failed: inf_functional_bytes mismatch for UTF-16LE (no BOM) INFs with different banners"
            )

        drift = check_inf_alias_drift(canonical=canonical, alias=alias, repo_root=repo_root, label="utf16-nobom")
        if drift is not None:
            fail(format_error("internal unit-test failed: check_inf_alias_drift unexpectedly reported drift:", [drift]))


def _self_test_inf_alias_drift_excluding_sections_utf16() -> None:
    """
    Ensure check_inf_alias_drift_excluding_sections remains robust for UTF-16 INFs.

    This drift check is used to validate legacy INF filename aliases while ignoring
    comment-only drift and normalizing section-header casing. It must behave the
    same for UTF-16 (with BOM and BOM-less) content.
    """

    canonical_header = "; canonical banner\r\n; line2\r\n\r\n"
    alias_header = "; alias banner\r\n\r\n"

    canonical_body = "[Version]\r\nSignature=\"$WINDOWS NT$\" ; comment\r\n\r\n[Foo]\r\nBar=baz ; comment\r\n"
    # Vary header casing + inline comment text; this should not count as drift.
    alias_body = "[Version]\r\nSignature=\"$WINDOWS NT$\" ; different\r\n\r\n[FOO]\r\nBar=baz ; different\r\n"

    # Introduce functional drift outside comments.
    alias_body_bad = "[Version]\r\nSignature=\"$WINDOWS NT$\" ; different\r\n\r\n[FOO]\r\nBar=qux ; different\r\n"

    def _run_case(*, encoding: str) -> None:
        with tempfile.TemporaryDirectory() as td:
            repo_root = Path(td)
            canonical = repo_root / "canonical.inf"
            alias = repo_root / "alias.inf"

            canonical.write_bytes((canonical_header + canonical_body).encode(encoding))
            alias.write_bytes((alias_header + alias_body).encode(encoding))
            drift = check_inf_alias_drift_excluding_sections(
                canonical=canonical,
                alias=alias,
                repo_root=repo_root,
                label="unit-test",
                drop_sections=set(),
            )
            if drift is not None:
                fail(
                    format_error(
                        "internal unit-test failed: check_inf_alias_drift_excluding_sections unexpectedly reported drift when only comments/casing differed:",
                        [drift],
                    )
                )

            alias.write_bytes((alias_header + alias_body_bad).encode(encoding))
            drift = check_inf_alias_drift_excluding_sections(
                canonical=canonical,
                alias=alias,
                repo_root=repo_root,
                label="unit-test",
                drop_sections=set(),
            )
            if drift is None:
                fail("internal unit-test failed: check_inf_alias_drift_excluding_sections failed to detect functional drift")

            # Regression test: semicolons inside quoted strings are data, not comment delimiters.
            # A naive comment stripper (e.g. `line.split(";", 1)`) can incorrectly ignore drift
            # after the semicolon inside quotes (false negative).
            canonical_body_quotes = "[Version]\r\nSignature=\"$WINDOWS NT$\"\r\n\r\n[Strings]\r\nFoo=\"Aero; Project\"\r\n"
            alias_body_quotes = "[Version]\r\nSignature=\"$WINDOWS NT$\"\r\n\r\n[Strings]\r\nFoo=\"Aero; Different\"\r\n"
            canonical.write_bytes((canonical_header + canonical_body_quotes).encode(encoding))
            alias.write_bytes((alias_header + alias_body_quotes).encode(encoding))
            drift = check_inf_alias_drift_excluding_sections(
                canonical=canonical,
                alias=alias,
                repo_root=repo_root,
                label="unit-test",
                drop_sections=set(),
            )
            if drift is None:
                fail(
                    "internal unit-test failed: check_inf_alias_drift_excluding_sections failed to detect drift when semicolons appear inside quoted strings"
                )

    _run_case(encoding="utf-16")
    _run_case(encoding="utf-16-le")


def _self_test_parse_queue_table_sizes() -> None:
    sample = r"""
| Queue index | Name | Direction | Queue size |
|------------|------|-----------|------------|
| 0 | `q0` | device → driver | **64** |
| 1 | `q1` | driver → device | **128** |
"""
    parsed = _parse_queue_table_sizes(sample, file=Path("<unit-test>"), context="virtio-test")
    if parsed != {0: 64, 1: 128}:
        fail(
            format_error(
                "internal unit-test failed: _parse_queue_table_sizes returned unexpected mapping:",
                [f"got: {parsed!r}", "expected: {0: 64, 1: 128}"],
            )
        )


@dataclass(frozen=True)
class InfRegDwordOccurrence:
    line_no: int
    value: int
    raw_line: str


@dataclass(frozen=True)
class InfRegLineOccurrence:
    line_no: int
    raw_line: str


@dataclass(frozen=True)
class InfMsiInterruptSettingsScan:
    interrupt_management_key_lines: tuple[InfRegLineOccurrence, ...]
    msi_supported: tuple[InfRegDwordOccurrence, ...]
    message_number_limit: tuple[InfRegDwordOccurrence, ...]


def scan_inf_msi_interrupt_settings(text: str, *, file: Path) -> tuple[InfMsiInterruptSettingsScan, list[str]]:
    """
    Scan an INF for MSI/MSI-X opt-in AddReg entries.

    We deliberately keep this lightweight and tolerant: line-based parsing that
    ignores full-line + inline comments and whitespace differences.
    """

    errors: list[str] = []

    interrupt_mgmt_lines: list[InfRegLineOccurrence] = []
    msi_supported: list[InfRegDwordOccurrence] = []
    msg_limit: list[InfRegDwordOccurrence] = []

    for line_no, raw in enumerate(text.splitlines(), start=1):
        if raw.lstrip().startswith(";"):
            continue
        line = _strip_inf_inline_comment(raw).strip()
        if not line:
            continue

        if not line.lstrip().upper().startswith("HKR"):
            continue

        parts = _split_inf_csv_fields(line)
        if not parts:
            continue
        if parts[0].strip().upper() != "HKR":
            continue

        subkey = _normalize_inf_reg_subkey(parts[1] if len(parts) > 1 else "")
        value_name = _normalize_inf_reg_value_name(parts[2] if len(parts) > 2 else "")

        if subkey == "interrupt management":
            # Prefer the recommended "key-only" form:
            #
            #   HKR, "Interrupt Management",,0x00000010
            #
            # but accept other ways of explicitly creating/touching the key.
            flags = _try_parse_inf_int(parts[3] if len(parts) > 3 else "")
            # "Equivalent key creation" forms:
            # - FLG_ADDREG_KEYONLY (0x10) (recommended)
            # - setting any value under the key (including the default value name)
            if flags is None:
                continue
            if value_name == "" and (len(parts) >= 5 or (flags & 0x10)):
                interrupt_mgmt_lines.append(InfRegLineOccurrence(line_no=line_no, raw_line=f"{file.as_posix()}:{line_no}: {line}"))
            elif value_name:
                interrupt_mgmt_lines.append(InfRegLineOccurrence(line_no=line_no, raw_line=f"{file.as_posix()}:{line_no}: {line}"))
            continue

        if subkey != "interrupt management\\messagesignaledinterruptproperties":
            continue

        if value_name not in ("msisupported", "messagenumberlimit"):
            continue

        if len(parts) < 5:
            errors.append(
                format_error(
                    f"{file.as_posix()}:{line_no}: MSI AddReg entry is missing a value field:",
                    [line],
                )
            )
            continue

        if len(parts) != 5:
            errors.append(
                format_error(
                    f"{file.as_posix()}:{line_no}: MSI AddReg entry must specify exactly one numeric value:",
                    [line],
                )
            )
            continue

        flags = _try_parse_inf_int(parts[3])
        if flags is None:
            errors.append(
                format_error(
                    f"{file.as_posix()}:{line_no}: MSI AddReg entry has a non-integer flags field:",
                    [line],
                )
            )
            continue
        # MSI opt-in keys must be written as REG_DWORD.
        if (flags & 0x00010001) != 0x00010001:
            errors.append(
                format_error(
                    f"{file.as_posix()}:{line_no}: MSI AddReg entry must specify a REG_DWORD type (0x00010001) in flags:",
                    [line],
                )
            )
            continue

        value = _try_parse_inf_int(parts[4])
        if value is None:
            errors.append(
                format_error(
                    f"{file.as_posix()}:{line_no}: MSI AddReg entry has a non-integer value:",
                    [line],
                )
            )
            continue

        occ = InfRegDwordOccurrence(line_no=line_no, value=value, raw_line=f"{file.as_posix()}:{line_no}: {line}")
        if value_name == "msisupported":
            msi_supported.append(occ)
        else:
            msg_limit.append(occ)

    return (
        InfMsiInterruptSettingsScan(
            interrupt_management_key_lines=tuple(interrupt_mgmt_lines),
            msi_supported=tuple(msi_supported),
            message_number_limit=tuple(msg_limit),
        ),
        errors,
    )


def _parse_inf_line_sections(text: str) -> dict[int, str]:
    """
    Return a mapping from (1-indexed) line number -> current INF section name (lowercase).

    Comment-only and blank lines are ignored (no mapping entry).
    """

    out: dict[int, str] = {}
    current_section: str | None = None
    for line_no, raw in enumerate(text.splitlines(), start=1):
        if raw.lstrip().startswith(";"):
            continue
        active = _strip_inf_inline_comment(raw).strip()
        if not active:
            continue
        m = re.match(r"^\[(?P<section>[^\]]+)\]\s*$", active)
        if m:
            current_section = m.group("section").strip().lower()
            continue
        if current_section is None:
            continue
        out[line_no] = current_section
    return out


def _parse_inf_section_names(text: str) -> list[str]:
    """Return the list of section names (as written, not lowercased) in `text`."""

    out: list[str] = []
    for raw in text.splitlines():
        if raw.lstrip().startswith(";"):
            continue
        active = _strip_inf_inline_comment(raw).strip()
        if not active:
            continue
        m = re.match(r"^\[(?P<section>[^\]]+)\]\s*$", active)
        if not m:
            continue
        out.append(m.group("section").strip())
    return out


def _parse_inf_referenced_addreg_sections(text: str, *, from_sections: set[str] | None = None) -> set[str]:
    """
    Return the set of AddReg section names referenced by `AddReg = ...` directives.

    Section names are returned lowercased for case-insensitive matching.
    """

    out: set[str] = set()
    current_section: str | None = None
    for raw in text.splitlines():
        if raw.lstrip().startswith(";"):
            continue
        active = _strip_inf_inline_comment(raw).strip()
        if not active:
            continue
        m = re.match(r"^\[(?P<section>[^\]]+)\]\s*$", active)
        if m:
            current_section = m.group("section").strip()
            continue
        if current_section is None:
            continue
        if from_sections is not None and current_section.lower() not in from_sections:
            continue
        m = re.match(r"^\s*AddReg\s*=\s*(?P<rhs>.+)$", active, flags=re.I)
        if not m:
            continue
        rhs = m.group("rhs").strip()
        for token in _split_inf_csv_fields(rhs):
            name = _unquote_inf_token(token).strip()
            if name:
                out.add(name.lower())
    return out


def _self_test_scan_inf_msi_interrupt_settings() -> None:
    sample = r"""
; full-line comment should be ignored:
; HKR, "Interrupt Management",,0x00000010
[Foo]
HKR, "Interrupt Management",, 16 ; inline comment should be ignored
HKR, "Interrupt Management\MessageSignaledInterruptProperties", "MSISupported", 0x00010001, 1
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MessageNumberLimit, 0x00010001, 0x8
"""
    scan, errors = scan_inf_msi_interrupt_settings(sample, file=Path("<unit-test>"))
    if errors:
        fail(format_error("internal unit-test failed: scan_inf_msi_interrupt_settings returned errors:", errors))
    if not scan.interrupt_management_key_lines:
        fail("internal unit-test failed: expected Interrupt Management key creation line to be detected")
    if {o.value for o in scan.msi_supported} != {1}:
        fail(f"internal unit-test failed: expected MSISupported=1, got: {[o.value for o in scan.msi_supported]}")
    if {o.value for o in scan.message_number_limit} != {8}:
        fail(
            f"internal unit-test failed: expected MessageNumberLimit=8, got: {[o.value for o in scan.message_number_limit]}"
        )

    # Also accept "equivalent key creation" where a value under Interrupt Management
    # is written (including the default value name), even without FLG_ADDREG_KEYONLY.
    default_value_key = r"""
[Foo]
HKR, "Interrupt Management",, 0x00010001, 0
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MSISupported, 0x00010001, 1
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MessageNumberLimit, 0x00010001, 8
"""
    scan2, errors2 = scan_inf_msi_interrupt_settings(default_value_key, file=Path("<unit-test>"))
    if errors2:
        fail(format_error("internal unit-test failed: scan_inf_msi_interrupt_settings returned errors:", errors2))
    if not scan2.interrupt_management_key_lines:
        fail("internal unit-test failed: expected default-value Interrupt Management key line to be detected")


def _self_test_validate_win7_virtio_inf_msi_settings() -> None:
    bad = r"""
[Version]
Signature="$WINDOWS NT$"

[Manufacturer]
%Mfg% = Mfg,NTx86

[Mfg.NTx86]
%Dev% = Install, PCI\VEN_1AF4&DEV_1041&REV_01

[Install.NT]
CopyFiles = Foo

; This AddReg is not referenced by the install section above.
[Unused]
AddReg = Dummy

[Dummy]
HKR, "Foo", "Bar", 0x00010001, 1

[MsiReg]
HKR, "Interrupt Management",,0x00000010
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MSISupported, 0x00010001, 1
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MessageNumberLimit, 0x00010001, 8
"""

    good = r"""
[Version]
Signature="$WINDOWS NT$"

[Manufacturer]
%Mfg% = Mfg,NTx86

[Mfg.NTx86]
%Dev% = Install, PCI\VEN_1AF4&DEV_1041&REV_01

[Install.NT]
CopyFiles = Foo

[Install.NT.HW]
AddReg = MsiReg

    [MsiReg]
    HKR, "Interrupt Management",,0x00000010
    HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MSISupported, 0x00010001, 1
    HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MessageNumberLimit, 0x00010001, 8
"""

    good_default_key = r"""
[Version]
Signature="$WINDOWS NT$"

[Manufacturer]
%Mfg% = Mfg,NTx86

[Mfg.NTx86]
%Dev% = Install, PCI\VEN_1AF4&DEV_1041&REV_01

[Install.NT]
CopyFiles = Foo

[Install.NT.HW]
AddReg = MsiReg

[MsiReg]
HKR, "Interrupt Management",,0x00010001, 0
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MSISupported, 0x00010001, 1
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MessageNumberLimit, 0x00010001, 8
"""

    missing_msi_supported = r"""
[Version]
Signature="$WINDOWS NT$"

[Manufacturer]
%Mfg% = Mfg,NTx86

[Mfg.NTx86]
%Dev% = Install, PCI\VEN_1AF4&DEV_1041&REV_01

[Install.NT.HW]
AddReg = MsiReg

[MsiReg]
HKR, "Interrupt Management",,0x00000010
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MessageNumberLimit, 0x00010001, 8
"""

    missing_message_number_limit = r"""
[Version]
Signature="$WINDOWS NT$"

[Manufacturer]
%Mfg% = Mfg,NTx86

[Mfg.NTx86]
%Dev% = Install, PCI\VEN_1AF4&DEV_1041&REV_01

[Install.NT.HW]
AddReg = MsiReg

[MsiReg]
HKR, "Interrupt Management",,0x00000010
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MSISupported, 0x00010001, 1
"""

    wrong_flags = r"""
[Version]
Signature="$WINDOWS NT$"

[Manufacturer]
%Mfg% = Mfg,NTx86

[Mfg.NTx86]
%Dev% = Install, PCI\VEN_1AF4&DEV_1041&REV_01

[Install.NT.HW]
AddReg = MsiReg

[MsiReg]
HKR, "Interrupt Management",,0x00000010
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MSISupported, 0, 1
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MessageNumberLimit, 0x00010001, 8
"""

    good_header_comments = r"""
[Version]
Signature="$WINDOWS NT$"

[Manufacturer]
%Mfg% = Mfg,NTx86

[Mfg.NTx86]
%Dev% = Install, PCI\VEN_1AF4&DEV_1041&REV_01

[Install.NT.HW] ; trailing comment after section header should be tolerated
AddReg = MsiReg

[MsiReg] ; another header comment
HKR, "Interrupt Management",,0x00000010
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MSISupported, 0x00010001, 1
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MessageNumberLimit, 0x00010001, 8
"""

    too_small_limit = r"""
[Version]
Signature="$WINDOWS NT$"

[Manufacturer]
%Mfg% = Mfg,NTx86

[Mfg.NTx86]
%Dev% = Install, PCI\VEN_1AF4&DEV_1041&REV_01

[Install.NT.HW]
AddReg = MsiReg

[MsiReg]
HKR, "Interrupt Management",,0x00000010
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MSISupported, 0x00010001, 1
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MessageNumberLimit, 0x00010001, 2
"""

    multi_value_limit = r"""
[Version]
Signature="$WINDOWS NT$"

[Manufacturer]
%Mfg% = Mfg,NTx86

[Mfg.NTx86]
%Dev% = Install, PCI\VEN_1AF4&DEV_1041&REV_01

[Install.NT.HW]
AddReg = MsiReg

[MsiReg]
HKR, "Interrupt Management",,0x00000010
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MSISupported, 0x00010001, 1
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MessageNumberLimit, 0x00010001, 8, 9
"""

    missing_interrupt_key = r"""
[Version]
Signature="$WINDOWS NT$"

[Manufacturer]
%Mfg% = Mfg,NTx86

[Mfg.NTx86]
%Dev% = Install, PCI\VEN_1AF4&DEV_1041&REV_01

[Install.NT.HW]
AddReg = MsiReg

[MsiReg]
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MSISupported, 0x00010001, 1
HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MessageNumberLimit, 0x00010001, 8
"""

    with tempfile.TemporaryDirectory() as td:
        bad_path = Path(td) / "bad.inf"
        bad_path.write_text(bad, encoding="utf-8")
        bad_errors = validate_win7_virtio_inf_msi_settings("virtio-net", bad_path)
        if not bad_errors:
            fail(
                "internal unit-test failed: validate_win7_virtio_inf_msi_settings unexpectedly passed when MSI AddReg section was not install-referenced"
            )

        good_path = Path(td) / "good.inf"
        good_path.write_text(good, encoding="utf-8")
        good_errors = validate_win7_virtio_inf_msi_settings("virtio-net", good_path)
        if good_errors:
            fail(
                format_error(
                    "internal unit-test failed: validate_win7_virtio_inf_msi_settings unexpectedly failed for a well-formed sample INF:",
                    good_errors,
                )
            )

        good_default_key_path = Path(td) / "good-default-key.inf"
        good_default_key_path.write_text(good_default_key, encoding="utf-8")
        good_default_key_errors = validate_win7_virtio_inf_msi_settings("virtio-net", good_default_key_path)
        if good_default_key_errors:
            fail(
                format_error(
                    "internal unit-test failed: validate_win7_virtio_inf_msi_settings unexpectedly failed for an INF that creates the Interrupt Management key by setting a default value:",
                    good_default_key_errors,
                )
            )

        missing_msi_supported_path = Path(td) / "missing-msisupported.inf"
        missing_msi_supported_path.write_text(missing_msi_supported, encoding="utf-8")
        missing_msi_supported_errors = validate_win7_virtio_inf_msi_settings("virtio-net", missing_msi_supported_path)
        if not any("missing MSISupported" in e for e in missing_msi_supported_errors):
            fail(
                format_error(
                    "internal unit-test failed: validate_win7_virtio_inf_msi_settings did not report MSISupported as missing:",
                    missing_msi_supported_errors or ["(no errors)"],
                )
            )

        missing_msg_limit_path = Path(td) / "missing-message-number-limit.inf"
        missing_msg_limit_path.write_text(missing_message_number_limit, encoding="utf-8")
        missing_msg_limit_errors = validate_win7_virtio_inf_msi_settings("virtio-net", missing_msg_limit_path)
        if not any("missing MessageNumberLimit" in e for e in missing_msg_limit_errors):
            fail(
                format_error(
                    "internal unit-test failed: validate_win7_virtio_inf_msi_settings did not report MessageNumberLimit as missing:",
                    missing_msg_limit_errors or ["(no errors)"],
                )
            )

        wrong_flags_path = Path(td) / "wrong-flags.inf"
        wrong_flags_path.write_text(wrong_flags, encoding="utf-8")
        wrong_flags_errors = validate_win7_virtio_inf_msi_settings("virtio-net", wrong_flags_path)
        if not wrong_flags_errors:
            fail(
                "internal unit-test failed: validate_win7_virtio_inf_msi_settings unexpectedly passed for an INF with non-DWORD MSISupported flags"
            )

        header_comments_path = Path(td) / "header-comments.inf"
        header_comments_path.write_text(good_header_comments, encoding="utf-8")
        header_comment_errors = validate_win7_virtio_inf_msi_settings("virtio-net", header_comments_path)
        if header_comment_errors:
            fail(
                format_error(
                    "internal unit-test failed: validate_win7_virtio_inf_msi_settings unexpectedly failed for an INF with trailing section header comments:",
                    header_comment_errors,
                )
            )

        too_small_path = Path(td) / "too-small.inf"
        too_small_path.write_text(too_small_limit, encoding="utf-8")
        too_small_errors = validate_win7_virtio_inf_msi_settings("virtio-net", too_small_path)
        if not too_small_errors:
            fail(
                "internal unit-test failed: validate_win7_virtio_inf_msi_settings unexpectedly passed for an INF with MessageNumberLimit below the per-device minimum"
            )

        multi_value_path = Path(td) / "multi-value.inf"
        multi_value_path.write_text(multi_value_limit, encoding="utf-8")
        multi_value_errors = validate_win7_virtio_inf_msi_settings("virtio-net", multi_value_path)
        if not multi_value_errors:
            fail(
                "internal unit-test failed: validate_win7_virtio_inf_msi_settings unexpectedly passed for an INF with multiple MessageNumberLimit values"
            )

        missing_key_path = Path(td) / "missing-interrupt-key.inf"
        missing_key_path.write_text(missing_interrupt_key, encoding="utf-8")
        missing_key_errors = validate_win7_virtio_inf_msi_settings("virtio-net", missing_key_path)
        if not missing_key_errors:
            fail(
                "internal unit-test failed: validate_win7_virtio_inf_msi_settings unexpectedly passed for an INF missing the Interrupt Management key creation line"
            )

def validate_win7_virtio_inf_msi_settings(
    device_name: str,
    inf_path: Path,
    *,
    min_message_number_limit: int | None = None,
) -> list[str]:
    """
    Ensure a canonical Win7 virtio INF keeps MSI/MSI-X opt-in settings.

    This prevents accidental removals/typos that silently drop the MSI path and
    reduce test coverage.
    """

    errors: list[str] = []
    text = read_text(inf_path)
    scan, scan_errors = scan_inf_msi_interrupt_settings(text, file=inf_path)
    errors.extend(scan_errors)

    # Guard against "dead" MSI registry settings: the HKR lines must live in a
    # section referenced by an AddReg directive, otherwise they will never be
    # applied during installation.
    any_referenced_addreg_sections = _parse_inf_referenced_addreg_sections(text)
    if not any_referenced_addreg_sections:
        errors.append(
            format_error(
                f"{inf_path.as_posix()}: INF contains no AddReg directives (MSI settings would be inert):",
                [
                    "expected at least one 'AddReg = ...' directive referencing a section that contains the MSI registry keys",
                ],
            )
        )

    # Further narrow to AddReg sections referenced from an install-section variant
    # selected by a [Models] entry. This avoids false negatives where the MSI
    # registry keys exist in a reg-add section but the driver install path no
    # longer references that section.
    install_prefixes = {
        _unquote_inf_token(e.install_section).strip().lower() for e in parse_inf_model_entries(inf_path)
    }
    defined_sections = _parse_inf_section_names(text)
    active_install_sections: set[str] = set()
    for s in defined_sections:
        s_lower = s.lower()
        if any(s_lower == p or s_lower.startswith(p + ".") for p in install_prefixes):
            active_install_sections.add(s_lower)

    referenced_addreg_sections = (
        _parse_inf_referenced_addreg_sections(text, from_sections=active_install_sections)
        if active_install_sections
        else any_referenced_addreg_sections
    )

    section_name_by_lower: dict[str, str] = {}
    for name in defined_sections:
        section_name_by_lower.setdefault(name.lower(), name)

    def _section_label(key: str) -> str:
        return section_name_by_lower.get(key.lower(), key)

    def _collect_addreg_directives(*, from_sections: set[str] | None) -> list[str]:
        directives: list[str] = []
        current: str | None = None
        for line_no, raw in enumerate(text.splitlines(), start=1):
            if raw.lstrip().startswith(";"):
                continue
            active = _strip_inf_inline_comment(raw).strip()
            if not active:
                continue
            m = re.match(r"^\[(?P<section>[^\]]+)\]\s*$", active)
            if m:
                current = m.group("section").strip().lower()
                continue
            if current is None:
                continue
            if from_sections is not None and current not in from_sections:
                continue
            m = re.match(r"^\s*AddReg\s*=\s*(?P<rhs>.+)$", active, flags=re.I)
            if not m:
                continue
            directives.append(f"{inf_path.as_posix()}:{line_no}: [{_section_label(current)}] {active}")
        return directives

    addreg_directives = _collect_addreg_directives(from_sections=active_install_sections or None)
    reachability_context = [
        "install-reachability context:",
        "install sections considered:",
        *(
            [f"- [{_section_label(s)}]" for s in sorted(active_install_sections)]
            if active_install_sections
            else ["- (none found; using all AddReg directives)"]
        ),
        "referenced AddReg sections:",
        *(
            [f"- [{_section_label(s)}]" for s in sorted(referenced_addreg_sections)]
            if referenced_addreg_sections
            else ["- (none)"]
        ),
        "AddReg directive(s):",
        *(
            [f"- {d}" for d in addreg_directives]
            if addreg_directives
            else ["- (none found)"]
        ),
    ]

    line_sections = _parse_inf_line_sections(text)

    def _is_in_referenced_addreg_section(line_no: int) -> bool:
        sect = line_sections.get(line_no)
        return sect is not None and sect in referenced_addreg_sections

    def _format_unreferenced_occurrences(what: str, occs: Iterable[object]) -> list[str]:
        lines: list[str] = []
        for occ in occs:
            # Support both InfRegLineOccurrence and InfRegDwordOccurrence.
            line_no = getattr(occ, "line_no", None)
            raw_line = getattr(occ, "raw_line", None)
            if not isinstance(line_no, int) or not isinstance(raw_line, str):
                continue
            if _is_in_referenced_addreg_section(line_no):
                continue
            sect = line_sections.get(line_no)
            if sect:
                lines.append(f"{raw_line}  (section [{_section_label(sect)}])")
            else:
                lines.append(raw_line)
        if not lines:
            return []
        return [
            f"found {what} entry/entries, but they are not in any install-referenced AddReg section:",
            *[f"- {l}" for l in lines],
        ]

    all_interrupt_key_lines = scan.interrupt_management_key_lines
    all_msi_supported_entries = scan.msi_supported
    all_msg_limit_entries = scan.message_number_limit

    interrupt_key_lines = all_interrupt_key_lines
    msi_supported_entries = all_msi_supported_entries
    msg_limit_entries = all_msg_limit_entries
    interrupt_key_lines = tuple(o for o in interrupt_key_lines if _is_in_referenced_addreg_section(o.line_no))
    msi_supported_entries = tuple(o for o in msi_supported_entries if _is_in_referenced_addreg_section(o.line_no))
    msg_limit_entries = tuple(o for o in msg_limit_entries if _is_in_referenced_addreg_section(o.line_no))

    if not interrupt_key_lines:
        errors.append(
            format_error(
                f"{inf_path.as_posix()}: missing Interrupt Management key creation AddReg entry:",
                [
                    'expected something like: HKR, "Interrupt Management",,0x00000010',
                    "hint: required for MSI/MSI-X opt-in on Windows 7",
                    *_format_unreferenced_occurrences("Interrupt Management key creation", all_interrupt_key_lines),
                    *reachability_context,
                ],
            )
        )

    if not msi_supported_entries:
        errors.append(
            format_error(
                f"{inf_path.as_posix()}: missing MSISupported AddReg entry:",
                [
                    'expected: HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MSISupported, ..., 1',
                    *_format_unreferenced_occurrences("MSISupported", all_msi_supported_entries),
                    *reachability_context,
                ],
            )
        )
    else:
        values = {o.value for o in msi_supported_entries}
        if values != {1}:
            errors.append(
                format_error(
                    f"{inf_path.as_posix()}: MSISupported must be 1:",
                    [
                        f"values found: {sorted(values)}",
                        *[o.raw_line for o in msi_supported_entries],
                        *reachability_context,
                    ],
                )
            )

    min_limit = (
        min_message_number_limit
        if min_message_number_limit is not None
        else WIN7_VIRTIO_INF_MIN_MESSAGE_NUMBER_LIMIT.get(device_name)
    )

    if not msg_limit_entries:
        errors.append(
            format_error(
                f"{inf_path.as_posix()}: missing MessageNumberLimit AddReg entry:",
                [
                    'expected: HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MessageNumberLimit, ..., <n>',
                    *_format_unreferenced_occurrences("MessageNumberLimit", all_msg_limit_entries),
                    *reachability_context,
                ],
            )
        )
    else:
        limits = {o.value for o in msg_limit_entries}
        if len(limits) != 1:
            errors.append(
                format_error(
                    f"{inf_path.as_posix()}: MessageNumberLimit must be consistent (found multiple values):",
                    [
                        f"values found: {sorted(limits)}",
                        *[o.raw_line for o in msg_limit_entries],
                        *reachability_context,
                    ],
                )
            )
        else:
            limit = next(iter(limits))
            if limit <= 0:
                errors.append(
                    format_error(
                        f"{inf_path.as_posix()}: MessageNumberLimit must be a positive integer:",
                        [
                            f"got: {limit}",
                            *[o.raw_line for o in msg_limit_entries],
                            *reachability_context,
                        ],
                    )
                )
            # PCI MSI-X Table Size max is 2048 vectors.
            if limit > 2048:
                errors.append(
                    format_error(
                        f"{inf_path.as_posix()}: MessageNumberLimit is unreasonably large:",
                        [
                            f"got: {limit}",
                            "expected: <= 2048 (PCI MSI-X Table Size limit)",
                            *[o.raw_line for o in msg_limit_entries],
                            *reachability_context,
                        ],
                    )
                )
            if min_limit is not None and limit < min_limit:
                errors.append(
                    format_error(
                        f"{inf_path.as_posix()}: MessageNumberLimit too small for {device_name}:",
                        [
                            f"minimum: {min_limit}",
                            f"got:     {limit}",
                            *[o.raw_line for o in msg_limit_entries],
                            *reachability_context,
                        ],
                    )
                )

    # Structural guardrail: ensure the MSI keys are not only present in the INF,
    # but are also reachable via Models -> DDInstall*.HW -> AddReg sections.
    #
    # This catches refactor mistakes where the AddReg section still exists (so a
    # simple scan would pass) but is no longer referenced by the install path.
    sections: dict[str, list[tuple[int, str]]] = {}
    section_names: dict[str, str] = {}
    current_section: str | None = None
    for line_no, raw in enumerate(text.splitlines(), start=1):
        if raw.lstrip().startswith(";"):
            continue
        # Section headers can legally include trailing comments (`[Foo] ; comment`).
        header = _strip_inf_inline_comment(raw).strip()
        m = re.match(r"^\s*\[(?P<section>[^\]]+)\]\s*$", header)
        if m:
            current_section = m.group("section").strip()
            key = current_section.lower()
            section_names.setdefault(key, current_section)
            sections.setdefault(key, [])
            continue
        if current_section is None:
            continue
        active = _strip_inf_inline_comment(raw).strip()
        if not active:
            continue
        sections[current_section.lower()].append((line_no, active))

    model_entries = parse_inf_model_entries(inf_path)
    install_bases = {e.install_section.strip().lower() for e in model_entries if e.install_section.strip()}

    hw_section_keys: list[str] = []
    for sect_key, name in section_names.items():
        if not name.lower().endswith(".hw"):
            continue
        if not install_bases:
            hw_section_keys.append(sect_key)
            continue
        for base in install_bases:
            if sect_key == f"{base}.hw" or (sect_key.startswith(base + ".") and sect_key.endswith(".hw")):
                hw_section_keys.append(sect_key)
                break

    # De-dupe while preserving order.
    seen_hw: set[str] = set()
    hw_section_keys = [k for k in hw_section_keys if not (k in seen_hw or seen_hw.add(k))]

    if not hw_section_keys:
        hw_sections_present = [name for name in section_names.values() if name.lower().endswith(".hw")]
        hw_sections_present.sort(key=lambda s: s.lower())
        errors.append(
            format_error(
                f"{inf_path.as_posix()}: could not locate any reachable .HW sections to validate MSI settings reachability:",
                [
                    "expected at least one [<InstallSection>*.HW] section referenced by Models.",
                    f"models install section base(s): {sorted(install_bases) if install_bases else '(none)'}",
                    "HW sections present in INF:",
                    *(hw_sections_present if hw_sections_present else ["(none)"]),
                ],
            )
        )
    else:
        for hw_key in hw_section_keys:
            hw_name = section_names.get(hw_key, hw_key)
            hw_lines = sections.get(hw_key, [])

            addreg_refs: list[str] = []
            addreg_directives: list[str] = []
            for line_no, line in hw_lines:
                m = re.match(r"^\s*AddReg\s*=\s*(?P<rhs>.+)$", line, flags=re.I)
                if not m:
                    continue
                rhs = m.group("rhs").strip()
                addreg_directives.append(f"{inf_path.as_posix()}:{line_no}: {line}")
                for part in _split_inf_csv_fields(rhs):
                    name = _unquote_inf_token(part).strip()
                    if name:
                        addreg_refs.append(name)

            # De-dupe AddReg refs while preserving order.
            seen: set[str] = set()
            addreg_refs = [r for r in addreg_refs if not (r.lower() in seen or seen.add(r.lower()))]

            if not addreg_refs:
                errors.append(
                    format_error(
                        f"{inf_path.as_posix()}: [{hw_name}] does not reference any AddReg sections (required for MSI/MSI-X opt-in):",
                        [
                            "expected an AddReg directive pointing at a section that sets:",
                            'HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MSISupported, ..., 1',
                            'HKR, "Interrupt Management\\MessageSignaledInterruptProperties", MessageNumberLimit, ..., <n>',
                        ],
                    )
                )
                continue

            missing_addreg_sections = [r for r in addreg_refs if r.lower() not in sections]
            if missing_addreg_sections:
                errors.append(
                    format_error(
                        f"{inf_path.as_posix()}: [{hw_name}] references missing AddReg section(s):",
                        [
                            *(f"missing section: [{r}]" for r in missing_addreg_sections),
                            "AddReg directive(s):",
                            *([f"- {d}" for d in addreg_directives] if addreg_directives else ["- (none found)"]),
                        ],
                    )
                )

            found: dict[str, str] = {}
            found_interrupt_key: str | None = None
            for ref in addreg_refs:
                ref_key = ref.lower()
                for line_no, line in sections.get(ref_key, []):
                    if not line.lstrip().upper().startswith("HKR"):
                        continue
                    parts = _split_inf_csv_fields(line)
                    if not parts or parts[0].strip().upper() != "HKR":
                        continue
                    subkey = _normalize_inf_reg_subkey(parts[1] if len(parts) > 1 else "")
                    if subkey == "interrupt management":
                        value_name = _normalize_inf_reg_value_name(parts[2] if len(parts) > 2 else "")
                        flags = _try_parse_inf_int(parts[3] if len(parts) > 3 else "")
                        if flags is not None and (value_name or (value_name == "" and (len(parts) >= 5 or (flags & 0x10)))):
                            found_interrupt_key = f"{inf_path.as_posix()}:{line_no}: {line}"
                    if subkey != "interrupt management\\messagesignaledinterruptproperties":
                        continue
                    value_name = _normalize_inf_reg_value_name(parts[2] if len(parts) > 2 else "")
                    if value_name in ("msisupported", "messagenumberlimit") and value_name not in found:
                        found[value_name] = f"{inf_path.as_posix()}:{line_no}: {line}"

            missing_keys = [k for k in ("msisupported", "messagenumberlimit") if k not in found]
            if found_interrupt_key is None:
                missing_keys.append("interrupt_management_key")
            if missing_keys:
                key_display = {
                    "interrupt_management_key": 'HKR, "Interrupt Management",,0x00000010',
                    "msisupported": "MSISupported",
                    "messagenumberlimit": "MessageNumberLimit",
                }
                errors.append(
                    format_error(
                        f"{inf_path.as_posix()}: [{hw_name}] MSI/MSI-X opt-in keys are missing from referenced AddReg section(s):",
                        [
                            *[f"missing key: {key_display.get(k, k)}" for k in missing_keys],
                            "expected to set both under:",
                            'HKR, "Interrupt Management\\MessageSignaledInterruptProperties", <Key>, ...',
                            'and create/touch the parent key (typically): HKR, "Interrupt Management",,0x00000010',
                            "referenced AddReg sections:",
                            *[f"- [{r}]" for r in addreg_refs],
                            "AddReg directive(s):",
                            *([f"- {d}" for d in addreg_directives] if addreg_directives else ["- (none found)"]),
                        ],
                    )
                )

    return errors


_PCI_HARDWARE_ID_RE = re.compile(r"^PCI\\VEN_(?P<ven>[0-9A-Fa-f]{4})&DEV_(?P<dev>[0-9A-Fa-f]{4})")


def parse_pci_vendor_device_from_hwid(hwid: str) -> tuple[int, int] | None:
    m = _PCI_HARDWARE_ID_RE.match(hwid)
    if not m:
        return None
    return int(m.group("ven"), 16), int(m.group("dev"), 16)


def validate_virtio_input_model_lines(
    *,
    inf_path: Path,
    strict_hwid: str,
    contract_rev: int,
    require_fallback: bool,
    errors: list[str],
) -> None:
    """
    Validate the virtio-input model line policy for the given INF.

    Policy:
    - The INF must include the SUBSYS-qualified Aero contract v1 keyboard/mouse HWIDs
      (distinct naming).
    - If `require_fallback` is true, the INF must also include the strict REV-qualified
      generic fallback HWID (no SUBSYS) equal to `strict_hwid`.
    - If `require_fallback` is false, the INF must not include that strict generic
      fallback model entry.
    - It must not include the tablet subsystem ID (`SUBSYS_00121AF4`); tablet devices
      bind via `aero_virtio_tablet.inf` (which is more specific and wins over the
      generic fallback when both are installed).
    """

    model_entries = parse_inf_model_entries(inf_path)
    string_keys = parse_inf_string_keys(inf_path)
    string_map = parse_inf_strings_map(inf_path)
    by_section: dict[str, list[InfModelEntry]] = {}
    for e in model_entries:
        by_section.setdefault(e.section.lower(), []).append(e)

    contract_rev_tag = f"&REV_{contract_rev:02X}"

    for section in ("Aero.NTx86", "Aero.NTamd64"):
        sect_entries = by_section.get(section.lower(), [])
        if not sect_entries:
            errors.append(
                f"{inf_path.as_posix()}: missing required models section [{section}] (expected virtio-input HWID bindings)."
            )
            continue

        kb = [
            e
            for e in sect_entries
            if "SUBSYS_00101AF4" in e.hardware_id.upper() and contract_rev_tag in e.hardware_id.upper()
        ]
        ms = [
            e
            for e in sect_entries
            if "SUBSYS_00111AF4" in e.hardware_id.upper() and contract_rev_tag in e.hardware_id.upper()
        ]
        fb = [e for e in sect_entries if e.hardware_id.upper() == strict_hwid.upper()]
        tablet = [e for e in sect_entries if "SUBSYS_00121AF4" in e.hardware_id.upper()]

        if tablet:
            errors.append(
                format_error(
                    f"{inf_path.as_posix()}: unexpected tablet subsystem model line(s) in [{section}] (SUBSYS_00121AF4); tablet devices must bind via aero_virtio_tablet.inf:",
                    [e.raw_line for e in tablet],
                )
            )

        if len(kb) != 1:
            errors.append(
                format_error(
                    f"{inf_path.as_posix()}: expected exactly one keyboard model line in [{section}] (SUBSYS_00101AF4 + REV):",
                    [e.raw_line for e in kb] if kb else ["(missing)"],
                )
            )
            continue
        if len(ms) != 1:
            errors.append(
                format_error(
                    f"{inf_path.as_posix()}: expected exactly one mouse model line in [{section}] (SUBSYS_00111AF4 + REV):",
                    [e.raw_line for e in ms] if ms else ["(missing)"],
                )
            )
            continue

        if require_fallback:
            if len(fb) != 1:
                errors.append(
                    format_error(
                        f"{inf_path.as_posix()}: expected exactly one fallback model line in [{section}] ({strict_hwid}):",
                        [e.raw_line for e in fb] if fb else ["(missing)"],
                    )
                )
                continue
        elif fb:
            errors.append(
                format_error(
                    f"{inf_path.as_posix()}: unexpected fallback model line(s) in [{section}] ({strict_hwid}):",
                    [e.raw_line for e in fb],
                )
            )

        kb_entry, ms_entry = kb[0], ms[0]

        # Model lines should share the same install section so behavior doesn't drift.
        expected_install = kb_entry.install_section
        if ms_entry.install_section != expected_install:
            lines = [
                f"keyboard install: {kb_entry.install_section} ({kb_entry.raw_line})",
                f"mouse install: {ms_entry.install_section} ({ms_entry.raw_line})",
            ]
            errors.append(
                format_error(
                    f"{inf_path.as_posix()}: virtio-input keyboard/mouse model lines in [{section}] must share the same install section:",
                    lines,
                )
            )

        fb_entry: InfModelEntry | None = None
        if require_fallback:
            fb_entry = fb[0]
            if fb_entry.install_section != expected_install:
                lines = [
                    f"keyboard install: {kb_entry.install_section} ({kb_entry.raw_line})",
                    f"mouse install: {ms_entry.install_section} ({ms_entry.raw_line})",
                    f"fallback install: {fb_entry.install_section} ({fb_entry.raw_line})",
                ]
                errors.append(
                    format_error(
                        f"{inf_path.as_posix()}: virtio-input model lines in [{section}] must share the same install section:",
                        lines,
                    )
                )

        # Keyboard vs mouse must have distinct DeviceDesc strings so they appear distinctly in Device Manager.
        if kb_entry.device_desc == ms_entry.device_desc:
            errors.append(
                format_error(
                    f"{inf_path.as_posix()}: virtio-input keyboard and mouse model lines in [{section}] must use distinct DeviceDesc strings:",
                    [kb_entry.raw_line, ms_entry.raw_line],
                )
            )

        # The fallback (no SUBSYS) should remain generic and not reuse the keyboard/mouse DeviceDesc.
        if fb_entry is not None and fb_entry.device_desc in (kb_entry.device_desc, ms_entry.device_desc):
            errors.append(
                format_error(
                    f"{inf_path.as_posix()}: virtio-input fallback model line in [{section}] must use a generic DeviceDesc (not the keyboard/mouse DeviceDesc):",
                    [kb_entry.raw_line, ms_entry.raw_line, fb_entry.raw_line],
                )
            )

        entries_for_strings = [kb_entry, ms_entry]
        if fb_entry is not None:
            entries_for_strings.append(fb_entry)

        for entry in entries_for_strings:
            desc = entry.device_desc.strip()
            if desc.startswith("%") and desc.endswith("%") and len(desc) > 2:
                token = desc[1:-1].strip()
                if token and token.lower() not in string_keys:
                    errors.append(
                        format_error(
                            f"{inf_path.as_posix()}: model entry references undefined [Strings] token {desc!r}:",
                            [entry.raw_line],
                        )
                    )

        def _resolve(desc: str) -> str:
            d = desc.strip()
            if d.startswith("%") and d.endswith("%") and len(d) > 2:
                key = d[1:-1].strip().lower()
                return string_map.get(key, d)
            return d

        kb_name = _resolve(kb_entry.device_desc).strip()
        ms_name = _resolve(ms_entry.device_desc).strip()

        if kb_name.lower() == ms_name.lower():
            errors.append(
                format_error(
                    f"{inf_path.as_posix()}: virtio-input keyboard and mouse DeviceDesc strings are identical in [{section}] (must differ):",
                    [kb_entry.raw_line, ms_entry.raw_line],
                )
            )

        if fb_entry is not None:
            fb_name = _resolve(fb_entry.device_desc).strip()
            if fb_name.lower() in {kb_name.lower(), ms_name.lower()}:
                errors.append(
                    format_error(
                        f"{inf_path.as_posix()}: virtio-input fallback DeviceDesc string in [{section}] must be generic (must not equal keyboard/mouse):",
                        [kb_entry.raw_line, ms_entry.raw_line, fb_entry.raw_line],
                    )
                )
def _normalized_inf_lines_without_sections(path: Path, *, drop_sections: set[str]) -> list[str]:
    """
    Normalized INF representation for drift checks:
    - ignores the leading comment/banner block (starts at the first section header,
      or the first unexpected functional line if one appears before any section header)
    - strips full-line and inline comments (INF comments start with ';' outside quoted strings)
    - drops empty lines
    - optionally removes entire sections (by name, case-insensitive)
    - normalizes section headers to lowercase (INF section names are case-insensitive)
    """

    drop = {s.lower() for s in drop_sections}
    out: list[str] = []
    current_section: str | None = None
    dropping = False

    for raw in read_text(path).splitlines():
        # Use the same quote-aware comment stripping as the INF parsers elsewhere in this
        # script (semicolons inside quoted strings are data, not comments).
        line = _strip_inf_inline_comment(raw).strip()
        if not line:
            continue

        m = re.match(r"^\[(?P<section>[^\]]+)\]\s*$", line)
        if m:
            current_section = m.group("section").strip()
            dropping = current_section.lower() in drop
            if not dropping:
                # INF section names are case-insensitive, so avoid false drift reports when
                # only the casing differs (e.g. `[Strings]` vs `[strings]`).
                out.append(f"[{current_section.lower()}]")
            continue

        if dropping:
            continue
        out.append(line)

    return out


def check_inf_alias_drift_excluding_sections(
    *,
    canonical: Path,
    alias: Path,
    repo_root: Path,
    label: str,
    drop_sections: set[str],
) -> str | None:
    """
    Compare two INFs while ignoring specific sections + comments.

    This is used when a legacy alias INF is intentionally permitted to diverge in
    specific sections, but should otherwise stay in sync with the canonical INF.
    """

    canonical_lines = _normalized_inf_lines_without_sections(canonical, drop_sections=drop_sections)
    alias_lines = _normalized_inf_lines_without_sections(alias, drop_sections=drop_sections)
    if canonical_lines == alias_lines:
        return None

    canonical_label = str(canonical.relative_to(repo_root))
    alias_label = str(alias.relative_to(repo_root))

    diff = difflib.unified_diff(
        [l + "\n" for l in canonical_lines],
        [l + "\n" for l in alias_lines],
        fromfile=canonical_label,
        tofile=alias_label,
        lineterm="\n",
    )

    ignored = sorted(drop_sections)
    header = f"{label}: INF alias drift detected"
    if ignored:
        header += " outside ignored sections"

    ignored_line = f"Ignored sections: {ignored}\n\n" if ignored else ""
    return header + "\n" + ignored_line + "".join(diff)


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
        idx = int(m.group("idx"))
        if idx in sizes:
            fail(f"{file.as_posix()}: duplicate virtqueue index {idx} in {context} virtqueue table")
        sizes[idx] = int(m.group("size"))
    if not sizes:
        fail(
            f"could not parse any virtqueue sizes for {context} in {file.as_posix()} "
            "(expected table rows like '| 0 | `rxq` | ... | **256** |')"
        )
    # Virtio queue indices are expected to start at 0 and be contiguous (0..N-1).
    # Enforce that here so contract doc typos are caught early (and so derived
    # MSI/MSI-X MessageNumberLimit minimums remain meaningful).
    indices = sorted(sizes)
    expected = list(range(len(indices)))
    if indices != expected:
        fail(
            format_error(
                f"{file.as_posix()}: {context} virtqueue indices must be contiguous starting at 0:",
                [f"expected: {expected}", f"got:      {indices}"],
            )
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

    _self_test_inf_inline_comment_stripping()
    _self_test_inf_parsers_ignore_comments()
    _self_test_inf_int_parsing()
    _self_test_read_text_utf16()
    _self_test_scan_text_tree_for_substrings_utf16()
    _self_test_inf_functional_bytes_utf16()
    _self_test_inf_alias_drift_excluding_sections_utf16()
    _self_test_parse_queue_table_sizes()
    _self_test_scan_inf_msi_interrupt_settings()
    _self_test_validate_win7_virtio_inf_msi_settings()
    _self_test_parse_guest_selftest_expected_service_names()

    w7_md = read_text(W7_VIRTIO_CONTRACT_MD)
    windows_md = read_text(WINDOWS_DEVICE_CONTRACT_MD)

    guest_selftest_services = parse_guest_selftest_expected_service_names(WIN7_VIRTIO_GUEST_SELFTEST_MAIN_CPP)

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

    # `New-AeroWin7TestImage.ps1` defaults to a conservative INF allowlist to avoid
    # accidentally staging test INFs. This allowlist must always include the
    # canonical in-tree Win7 virtio INFs; otherwise provisioning media can silently
    # go stale after driver package renames.
    required_allowlist_infs = {p.name.lower() for p in WIN7_VIRTIO_DRIVER_INFS.values()}
    try:
        ps1_allowlist_raw = parse_powershell_array_strings(
            text=read_text(WIN7_TEST_IMAGE_PS1), var_name="defaultInfAllowList", file=WIN7_TEST_IMAGE_PS1
        )
    except ValueError as e:
        errors.append(
            format_error(
                "Failed to parse the default INF allowlist from New-AeroWin7TestImage.ps1 (CI guardrail):",
                [
                    str(e),
                    "hint: keep the allowlist in the form `$defaultInfAllowList = @(\"...\")`",
                ],
            )
        )
    else:
        ps1_allowlist = {inf_allowlist_entry_basename(v).lower() for v in ps1_allowlist_raw}
        missing = sorted(required_allowlist_infs - ps1_allowlist)
        if missing:
            errors.append(
                format_error(
                    f"{WIN7_TEST_IMAGE_PS1.as_posix()}: $defaultInfAllowList is missing canonical Win7 virtio driver INF basenames:",
                    [
                        "missing:",
                        *[f"- {m}" for m in missing],
                        "current $defaultInfAllowList basenames:",
                        *[f"- {v}" for v in sorted(ps1_allowlist)],
                        "hint: update $defaultInfAllowList in drivers/windows7/tests/host-harness/New-AeroWin7TestImage.ps1 whenever driver INF basenames change or new canonical Win7 virtio INFs are added",
                    ],
                )
            )

        deprecated_in_allowlist = sorted({d.lower() for d in DEPRECATED_WIN7_TEST_INF_BASENAMES} & ps1_allowlist)
        if deprecated_in_allowlist:
            errors.append(
                format_error(
                    f"{WIN7_TEST_IMAGE_PS1.as_posix()}: $defaultInfAllowList contains deprecated INF basenames:",
                    [
                        *[f"- {d}" for d in deprecated_in_allowlist],
                        "hint: replace deprecated names with canonical aero_virtio_*.inf basenames",
                    ],
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

            # -------------------------------------------------------------
            # 2.1.1) Guest selftest hardcoded service-name expectations must
            #        stay aligned with the contract + shipped INFs.
            # -------------------------------------------------------------
            if device_name in ("virtio-net", "virtio-snd", "virtio-input"):
                selftest = guest_selftest_services.get(device_name)
                if selftest is None:
                    errors.append(
                        format_error(
                            f"{device_name}: could not locate expected service name in guest selftest:",
                            [
                                f"file: {WIN7_VIRTIO_GUEST_SELFTEST_MAIN_CPP.as_posix()}",
                                "hint: update scripts/ci/check-windows7-virtio-contract-consistency.py to match guest-selftest source changes",
                            ],
                        )
                    )
                elif selftest.value != service_name:
                    addservices = parse_inf_addservice_entries(inf_path)
                    inf_match = next((e for e in addservices if e.value.lower() == service_name.lower()), None)
                    errors.append(
                        format_error(
                            f"{device_name}: guest selftest expected Windows service name mismatch:",
                            [
                                f"expected (from {WINDOWS_DEVICE_CONTRACT_JSON.as_posix()} devices[{device_name}].driver_service_name): {service_name!r}",
                                f"found: {selftest.format_location()}",
                                f"INF AddService (expected): {inf_match.format_location() if inf_match else f'(missing) {inf_path.as_posix()}'}",
                            ],
                        )
                    )

            if device_name == "virtio-snd":
                # Transitional virtio-snd is out-of-scope for AERO-W7-VIRTIO v1,
                # but the guest selftest has an opt-in transitional path which
                # should remain aligned with the legacy INF service name.
                selftest_trans = guest_selftest_services.get("virtio-snd-transitional")
                if selftest_trans is not None and AERO_VIRTIO_SND_LEGACY_INF.exists():
                    legacy_addservices = parse_inf_addservice_entries(AERO_VIRTIO_SND_LEGACY_INF)
                    legacy_match = next(
                        (e for e in legacy_addservices if e.value.lower() == selftest_trans.value.lower()),
                        None,
                    )
                    if legacy_match is None:
                        errors.append(
                            format_error(
                                "virtio-snd: guest selftest transitional expected Windows service name is not installed by the legacy INF:",
                                [
                                    f"found: {selftest_trans.format_location()}",
                                    f"expected to match an AddService entry in: {AERO_VIRTIO_SND_LEGACY_INF.as_posix()}",
                                    "legacy INF AddService entries:",
                                    *(
                                        [f"- {e.format_location()}" for e in legacy_addservices]
                                        if legacy_addservices
                                        else ["- (none found)"]
                                    ),
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

        # Guardrail: keep MSI/MSI-X opt-in registry keys present so message-signaled
        # interrupt paths remain exercised by default (when supported by the platform).
        # Also require that the INF requests at least 1 + num_queues interrupt messages
        # (config interrupt + one per virtqueue) based on the contract queue table.
        #
        # This is a conservative minimum: Windows may grant fewer messages at runtime,
        # and the driver must still handle that via vector sharing/fallback paths.
        derived_min_limit: int | None = None
        contract_q = contract_queues.get(device_name)
        if contract_q:
            # Queue indices are expected to start at 0. Derive the minimum based on
            # the number of queues present in the contract table rather than the
            # max index to remain correct even if indices ever become non-contiguous.
            derived_min_limit = len(contract_q) + 1
        errors.extend(
            validate_win7_virtio_inf_msi_settings(
                device_name,
                inf_path,
                min_message_number_limit=derived_min_limit,
            )
        )

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
        # Do not require a specific `{base_hwid}&REV_XX` literal (some INFs further
        # qualify binding with SUBSYS_...); instead, enforce that the INF binds to
        # the expected VEN/DEV family and that all matches are revision-gated to
        # the contract revision.
        relevant = [h for h in hwids if h.upper().startswith(base_hwid.upper())]
        if not relevant:
            errors.append(
                format_error(
                    f"{inf_path.as_posix()}: INF is missing required virtio HWID family {base_hwid} (no active HWIDs start with it):",
                    [f"got: {sorted(hwids)}"],
                )
            )
            continue
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

        # virtio-input is exposed as two PCI functions (keyboard + mouse) but installs
        # through the same INF + service. For debuggability, the INF should use distinct
        # DeviceDesc strings for each function so they appear separately in Device Manager.
        #
        # Policy note:
        # - The canonical virtio-input INF is intentionally SUBSYS-only: it binds to
        #   SUBSYS-qualified keyboard/mouse HWIDs for distinct naming, and does *not*
        #   include a strict generic fallback HWID.
        # - A legacy filename alias INF exists for compatibility with older tooling.
        #   That alias may add an opt-in strict generic fallback in the models sections,
        #   but must otherwise remain byte-identical to the canonical INF from the
        #   first section header onward.
        if device_name == "virtio-input":
            validate_virtio_input_model_lines(
                inf_path=inf_path,
                strict_hwid=strict_hwid,
                contract_rev=contract_rev,
                require_fallback=False,
                errors=errors,
            )

    # ---------------------------------------------------------------------
    # 6) INF alias drift guardrails (legacy filename aliases must stay in sync).
    # ---------------------------------------------------------------------
    virtio_input_inf_dir = REPO_ROOT / "drivers/windows7/virtio-input/inf"
    virtio_input_canonical = virtio_input_inf_dir / "aero_virtio_input.inf"
    virtio_input_alias_enabled = virtio_input_inf_dir / "virtio-input.inf"
    virtio_input_alias_disabled = virtio_input_inf_dir / "virtio-input.inf.disabled"
    if virtio_input_alias_enabled.exists() and virtio_input_alias_disabled.exists():
        errors.append(
            f"{virtio_input_inf_dir.as_posix()}: both virtio-input.inf and virtio-input.inf.disabled exist; keep only one to avoid multiple matching INFs."
        )

    # Policy: `virtio-input.inf.disabled` is a legacy basename alias kept for compatibility.
    # It is allowed to diverge from the canonical INF only in the models sections
    # (`[Aero.NTx86]` / `[Aero.NTamd64]`) to add the opt-in strict generic fallback HWID.
    # Outside those models sections, from the first section header (`[Version]`) onward,
    # it must remain byte-for-byte identical to the canonical INF (only the leading
    # banner/comments may differ).
    if not virtio_input_alias_disabled.exists():
        errors.append(
            f"missing required legacy filename alias INF: {virtio_input_alias_disabled.as_posix()} (keep it checked in disabled-by-default; developers may locally enable it by renaming to virtio-input.inf)"
        )
    else:
        virtio_input_alias = virtio_input_alias_disabled
        virtio_input_contract_any = contract_ids["virtio-input (keyboard)"]
        base_hwid = f"PCI\\VEN_{virtio_input_contract_any.vendor_id:04X}&DEV_{virtio_input_contract_any.device_id:04X}"
        strict_hwid = f"{base_hwid}&REV_{contract_rev:02X}"

        # The legacy alias INF is kept for compatibility with workflows/tools that reference the
        # legacy `virtio-input.inf` name.
        # Policy: it may add a strict generic fallback model line in its models sections, but
        # otherwise must remain byte-for-byte identical to the canonical INF from the first
        # section header (`[Version]`) onward.
        validate_virtio_input_model_lines(
            inf_path=virtio_input_alias,
            strict_hwid=strict_hwid,
            contract_rev=contract_rev,
            require_fallback=True,
            errors=errors,
        )

        drift = check_inf_alias_drift_excluding_sections_bytes(
            canonical=virtio_input_canonical,
            alias=virtio_input_alias,
            repo_root=REPO_ROOT,
            label="virtio-input",
            drop_sections={"aero.ntx86", "aero.ntamd64"},
        )
        if drift:
            errors.append(drift)

    if errors:
        print("\n\n".join(errors), file=sys.stderr)
        raise SystemExit(1)


if __name__ == "__main__":
    main()
