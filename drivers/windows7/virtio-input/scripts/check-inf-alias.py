#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""\
Verify that the legacy virtio-input INF filename alias stays in sync.

The Windows 7 virtio-input driver package has a canonical INF:
  - drivers/windows7/virtio-input/inf/aero_virtio_input.inf

For compatibility with older tooling/workflows, the repo also keeps a legacy
filename alias INF (checked in disabled-by-default):
  - drivers/windows7/virtio-input/inf/virtio-input.inf.disabled

Developers may locally enable the alias by renaming it to `virtio-input.inf`.

Policy:
  - The alias INF is a *filename alias only*.
  - The canonical INF (`aero_virtio_input.inf`) is SUBSYS-only (no strict generic
    fallback HWID).
  - The alias INF is allowed to diverge from the canonical INF only in the models
    sections (`[Aero.NTx86]` / `[Aero.NTamd64]`) to add the opt-in strict,
    revision-gated generic fallback HWID (`PCI\\VEN_1AF4&DEV_1052&REV_01`).
  - Outside those models sections, from the first section header (typically
    `[Version]`) onward, the alias must remain byte-for-byte identical to the
    canonical INF.
  - Only the leading banner/comment block may differ.
Run from the repo root:
  python3 drivers/windows7/virtio-input/scripts/check-inf-alias.py
"""

from __future__ import annotations

import difflib
import sys
from pathlib import Path


MODELS_SECTIONS = {"aero.ntx86", "aero.ntamd64"}
STRICT_FALLBACK_HWID = r"PCI\VEN_1AF4&DEV_1052&REV_01"


def _first_nonblank_ascii_byte(*, line: bytes, first_line: bool) -> int | None:
    """
    Return the first meaningful ASCII byte on a line, ignoring whitespace and NULs.

    This is robust to UTF-16LE/BE encoded INFs where each ASCII character is
    separated by a NUL byte.
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


def inf_functional_bytes(path: Path) -> bytes:
    """
    Return the file content starting from the first section header line.

    We intentionally ignore the leading comment/header block so the alias INF can
    have a different filename banner, while still enforcing byte-for-byte
    equality for all sections/keys.
    """

    data = path.read_bytes()
    lines = data.splitlines(keepends=True)

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


def _decode_lines_for_diff(data: bytes) -> list[str]:
    """
    Decode bytes for a readable unified diff.

    The comparison is byte-for-byte, but when files drift we want the diff output
    to be readable even if the INF is UTF-16 encoded (with or without a BOM).
    """

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

def _strip_inf_inline_comment(line: str) -> str:
    """Strip INF comments (starting with ';') outside quoted strings."""

    out: list[str] = []
    in_quote = False
    for ch in line:
        if ch == '"':
            in_quote = not in_quote
        if (not in_quote) and ch == ";":
            break
        out.append(ch)
    return "".join(out)


def count_hwid_model_lines(*, path: Path, section: str, hwid: str) -> tuple[bool, int]:
    """
    Count model lines containing `hwid` within a specific models section.

    Returns `(section_seen, count)`. Comments are ignored.
    """

    lines = _decode_lines_for_diff(path.read_bytes())
    current: str | None = None
    seen = False
    count = 0
    for raw in lines:
        line = raw.rstrip("\r\n")
        stripped = line.lstrip(" \t")
        if stripped.startswith("[") and "]" in stripped:
            current = stripped[1 : stripped.index("]")].strip().lower()
            if current == section.lower():
                seen = True
            continue
        if current != section.lower():
            continue
        no_comment = _strip_inf_inline_comment(line).strip()
        if not no_comment:
            continue
        if hwid.lower() in no_comment.lower():
            count += 1
    return seen, count


def strip_inf_sections(data: bytes, *, sections: set[str]) -> bytes:
    """Remove entire INF sections (including their headers) by name (case-insensitive)."""

    out: list[bytes] = []
    skipping = False

    for line in data.splitlines(keepends=True):
        # Support both UTF-8/ASCII INFs and UTF-16LE/BE INFs by stripping NUL bytes for
        # section header detection only.
        line_ascii = line.replace(b"\x00", b"")
        stripped = line_ascii.lstrip(b" \t")
        if stripped.startswith(b"[") and b"]" in stripped:
            end = stripped.find(b"]")
            name = stripped[1:end].strip().decode("utf-8", errors="replace").lower()
            skipping = name in sections

        if skipping:
            continue
        out.append(line)

    return b"".join(out)


def main() -> int:
    virtio_input_root = Path(__file__).resolve().parents[1]
    repo_root = virtio_input_root.parents[2]
    inf_dir = virtio_input_root / "inf"

    canonical = inf_dir / "aero_virtio_input.inf"

    # The repo keeps the alias checked in disabled-by-default, but developers may
    # locally enable it by renaming to `virtio-input.inf`. Support both so the
    # check can be used in either state.
    alias_enabled = inf_dir / "virtio-input.inf"
    alias_disabled = inf_dir / "virtio-input.inf.disabled"
    if alias_enabled.exists():
        alias = alias_enabled
    elif alias_disabled.exists():
        alias = alias_disabled
    else:
        sys.stderr.write(
            "virtio-input INF alias drift check: no alias INF found "
            "(expected virtio-input.inf or virtio-input.inf.disabled); skipping.\n"
        )
        return 0

    canonical_body = strip_inf_sections(inf_functional_bytes(canonical), sections=MODELS_SECTIONS)
    alias_body = strip_inf_sections(inf_functional_bytes(alias), sections=MODELS_SECTIONS)
    if canonical_body == alias_body:
        # Functional regions match; still validate the models-section policy:
        # - canonical INF must not contain the strict generic fallback HWID at all
        #   (including comments).
        # - alias INF must include exactly one strict fallback model line per models section.
        strict_bytes = STRICT_FALLBACK_HWID.encode("ascii", errors="ignore").upper()
        if strict_bytes and strict_bytes in canonical.read_bytes().upper():
            sys.stderr.write(
                "virtio-input INF policy violation: canonical INF must not contain the strict generic fallback HWID "
                f"({STRICT_FALLBACK_HWID}); it is opt-in via the alias INF.\n"
            )
            return 1

        for sect in ("Aero.NTx86", "Aero.NTamd64"):
            canonical_seen, canonical_count = count_hwid_model_lines(
                path=canonical, section=sect, hwid=STRICT_FALLBACK_HWID
            )
            alias_seen, alias_count = count_hwid_model_lines(path=alias, section=sect, hwid=STRICT_FALLBACK_HWID)
            if not canonical_seen:
                sys.stderr.write(f"{canonical}: missing required models section [{sect}].\n")
                return 1
            if not alias_seen:
                sys.stderr.write(f"{alias}: missing required models section [{sect}].\n")
                return 1
            if canonical_count != 0:
                sys.stderr.write(
                    "virtio-input INF policy violation: canonical INF must be SUBSYS-only (no strict generic fallback "
                    f"model line {STRICT_FALLBACK_HWID}).\n"
                )
                return 1
            if alias_count != 1:
                sys.stderr.write(
                    "virtio-input INF policy violation: legacy alias INF must include exactly one strict generic fallback "
                    f"model line {STRICT_FALLBACK_HWID} in [{sect}] (found {alias_count}).\n"
                )
                return 1

        return 0

    sys.stderr.write("virtio-input INF alias drift detected.\n")
    sys.stderr.write(
        "The alias INF must match the canonical INF from [Version] onward, excluding the models sections "
        "[Aero.NTx86]/[Aero.NTamd64] (and ignoring the leading banner).\n\n"
    )

    canonical_lines = _decode_lines_for_diff(canonical_body)
    alias_lines = _decode_lines_for_diff(alias_body)

    # Use repo-relative paths in the diff output to keep it readable and stable
    # across machines/CI environments.
    canonical_label = str(canonical.relative_to(repo_root))
    alias_label = str(alias.relative_to(repo_root))

    diff = difflib.unified_diff(
        canonical_lines,
        alias_lines,
        fromfile=canonical_label,
        tofile=alias_label,
        lineterm="",
    )
    for line in diff:
        sys.stderr.write(line + "\n")

    return 1


if __name__ == "__main__":
    raise SystemExit(main())
