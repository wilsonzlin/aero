#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Verify that the virtio-input legacy INF filename alias stays in sync.

The Windows 7 virtio-input driver package has a canonical keyboard/mouse INF:
  - drivers/windows7/virtio-input/inf/aero_virtio_input.inf

For compatibility with older tooling/workflows, the repo also keeps a legacy
filename alias INF (checked in disabled-by-default):
  - drivers/windows7/virtio-input/inf/virtio-input.inf.disabled

Developers may locally enable the alias by renaming it to `virtio-input.inf`.

Policy:
  - The canonical INF (`aero_virtio_input.inf`) is intentionally SUBSYS-only
    (keyboard + mouse; no generic fallback).
  - The legacy alias INF (`virtio-input.inf` / `virtio-input.inf.disabled`) is
    an opt-in compatibility shim for workflows that reference the legacy
    filename. It is allowed to diverge in the models sections (`[Aero.NTx86]` /
    `[Aero.NTamd64]`) to add a strict, revision-gated generic fallback (no
    SUBSYS).
  - Outside those models sections, from the first section header (`[Version]`)
    onward, the alias must remain byte-for-byte identical to the canonical INF.
  - Only the leading comment/banner block (above `[Version]`) may differ.
  - The CI guardrail `scripts/ci/check-windows7-virtio-contract-consistency.py`
    validates the virtio-input HWID/model-line policy (SUBSYS-only canonical;
    strict fallback in the alias; no tablet entry).

Run from the repo root:
  python3 drivers/windows7/virtio-input/scripts/check-inf-alias.py
"""

from __future__ import annotations

import difflib
import sys
from pathlib import Path

VIRTIO_VENDOR_ID = 0x1AF4
VIRTIO_INPUT_DEVICE_ID = 0x1052
STRICT_FALLBACK_HWID = (
    f"PCI\\VEN_{VIRTIO_VENDOR_ID:04X}&DEV_{VIRTIO_INPUT_DEVICE_ID:04X}&REV_01"
)
MODELS_SECTIONS = {"aero.ntx86", "aero.ntamd64"}


def read_text_best_effort(path: Path) -> str:
    """Best-effort INF text decoding (UTF-8/ASCII, UTF-16LE/BE with or without BOM)."""

    data = path.read_bytes()

    # BOM handling.
    if data.startswith(b"\xff\xfe") or data.startswith(b"\xfe\xff"):
        return data.decode("utf-16", errors="replace").lstrip("\ufeff")
    if data.startswith(b"\xef\xbb\xbf"):
        return data.decode("utf-8-sig", errors="replace")

    # Try UTF-8 first.
    utf8_text: str | None
    try:
        utf8_text = data.decode("utf-8")
    except UnicodeDecodeError:
        utf8_text = None
    else:
        # Fast-path: no NULs means we decoded correctly.
        if "\x00" not in utf8_text:
            return utf8_text

    # Heuristic detection for UTF-16 without BOM.
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
        return data.decode(enc, errors="replace").lstrip("\ufeff")

    if utf8_text is not None:
        # Likely UTF-16 without BOM decoded as UTF-8. Strip NUL padding.
        return utf8_text.replace("\x00", "")

    return data.decode("utf-8", errors="replace")


def strip_inf_inline_comment(raw: str) -> str:
    """Strip INF inline comments (semicolon outside quotes)."""

    out: list[str] = []
    in_quote: str | None = None
    for ch in raw:
        if ch in ("'", '"'):
            if in_quote == ch:
                in_quote = None
            elif in_quote is None:
                in_quote = ch
        if ch == ";" and in_quote is None:
            break
        out.append(ch)
    return "".join(out)


def inf_section_text(*, text: str, section: str) -> list[str]:
    """Return active (non-comment) lines within a section."""

    out: list[str] = []
    current: str | None = None
    for raw in text.splitlines():
        line = strip_inf_inline_comment(raw).strip()
        if not line:
            continue
        if line.startswith("[") and line.endswith("]") and len(line) >= 2:
            current = line[1:-1].strip().lower()
            continue
        if current == section.lower():
            out.append(line)
    return out


def _first_nonblank_ascii_byte(*, line: bytes, first_line: bool) -> int | None:
    """Return the first meaningful ASCII byte on a line, ignoring whitespace and NULs.

    This is robust to UTF-16LE/BE encoded INFs where each ASCII character may be
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
    """Return the file content starting from the first section header line."""

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

        # Unexpected preamble content (not comment, not blank, not section): treat it as functional.
        return b"".join(lines[i:])

    raise RuntimeError(f"{path}: could not find a section header (e.g. [Version])")


def strip_inf_sections_bytes(data: bytes, *, sections: set[str]) -> bytes:
    """Remove entire INF sections (including their headers) by name (case-insensitive)."""

    drop = {s.lower() for s in sections}
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


def decode_lines_for_diff(data: bytes) -> list[str]:
    """Decode bytes for a readable unified diff."""

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


def main() -> int:
    repo_root = Path(__file__).resolve().parents[4]
    inf_dir = repo_root / "drivers/windows7/virtio-input/inf"

    canonical = inf_dir / "aero_virtio_input.inf"
    if not canonical.exists():
        sys.stderr.write(f"virtio-input INF alias drift check: canonical INF not found: {canonical}\n")
        return 1

    alias_enabled = inf_dir / "virtio-input.inf"
    alias_disabled = inf_dir / "virtio-input.inf.disabled"
    if alias_enabled.exists() and alias_disabled.exists():
        sys.stderr.write(
            f"virtio-input INF alias drift check: both {alias_enabled} and {alias_disabled} exist; keep only one.\n"
        )
        return 1

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

    canonical_text = read_text_best_effort(canonical)
    alias_text = read_text_best_effort(alias)

    errors: list[str] = []

    # The canonical INF must not contain the strict fallback HWID *anywhere*, even in
    # comments. This keeps it truly SUBSYS-only and prevents accidentally
    # cargo-culting the fallback line back into the canonical file.
    if STRICT_FALLBACK_HWID.lower() in canonical_text.lower():
        errors.append(
            f"{canonical.as_posix()}: canonical INF must not contain strict fallback HWID {STRICT_FALLBACK_HWID!r} "
            "(fallback is alias-only)"
        )

    # 1) Canonical must not include the strict fallback in models sections.
    for section in ("Aero.NTx86", "Aero.NTamd64"):
        lines = inf_section_text(text=canonical_text, section=section)
        hits = [l for l in lines if STRICT_FALLBACK_HWID.lower() in l.lower()]
        if hits:
            errors.append(
                f"{canonical.as_posix()}: canonical INF must not contain strict fallback model line in [{section}] "
                f"(fallback is alias-only):\n  " + "\n  ".join(hits)
            )

    # 2) Alias must include exactly one strict fallback model line per models section.
    for section in ("Aero.NTx86", "Aero.NTamd64"):
        lines = inf_section_text(text=alias_text, section=section)
        hits = [l for l in lines if STRICT_FALLBACK_HWID.lower() in l.lower()]
        if len(hits) != 1:
            errors.append(
                f"{alias.as_posix()}: expected exactly one strict fallback model line in [{section}] "
                f"(got {len(hits)}):\n  " + "\n  ".join(hits)
            )

    # 3) Drift check: from [Version] onward, alias must match canonical byte-for-byte outside models sections.
    canonical_body = strip_inf_sections_bytes(inf_functional_bytes(canonical), sections=MODELS_SECTIONS)
    alias_body = strip_inf_sections_bytes(inf_functional_bytes(alias), sections=MODELS_SECTIONS)
    if canonical_body != alias_body:
        canonical_label = str(canonical.relative_to(repo_root))
        alias_label = str(alias.relative_to(repo_root))

        diff = difflib.unified_diff(
            decode_lines_for_diff(canonical_body),
            decode_lines_for_diff(alias_body),
            fromfile=canonical_label,
            tofile=alias_label,
            lineterm="",
        )
        errors.append(
            "virtio-input INF alias drift detected (expected byte-identical outside banner/comments + models sections):\n"
            + "".join(diff)
        )

    if errors:
        sys.stderr.write("\n\n".join(errors) + "\n")
        return 1

    print(
        "virtio-input INF alias drift check: OK ({} stays in sync with {} outside banner/comments + models sections)".format(
            alias.relative_to(repo_root), canonical.relative_to(repo_root)
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

