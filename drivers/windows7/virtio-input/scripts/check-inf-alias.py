#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""
Verify that the virtio-input legacy INF filename alias stays in sync.

Canonical INF:
  drivers/windows7/virtio-input/inf/aero_virtio_input.inf

Legacy filename alias (checked in disabled-by-default):
  drivers/windows7/virtio-input/inf/virtio-input.inf.disabled

Developers may locally enable the alias by renaming it to `virtio-input.inf`.

Policy:
  - The alias is filename-only.
  - From the first section header (`[Version]`) onward, the alias must remain
    byte-for-byte identical to the canonical INF (only the leading banner/comments
    may differ).

Comparison notes:
  - The comparison is intentionally byte-based. It does not normalize whitespace,
    comments, section casing, or line endings.

Run from the repo root:
  python3 drivers/windows7/virtio-input/scripts/check-inf-alias.py
"""

from __future__ import annotations

import difflib
import sys
from pathlib import Path


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
        if b in (0x00, 0x09, 0x0A, 0x0D, 0x20):
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

        # First section header (e.g. "[Version]") starts the compared region.
        if first == ord("["):
            return b"".join(lines[i:])

        # Ignore leading comments.
        if first == ord(";"):
            continue

        # Unexpected preamble content (not comment, not blank, not section): treat
        # it as functional to avoid masking drift.
        return b"".join(lines[i:])

    raise RuntimeError(f"{path}: could not find a section header line (e.g. [Version])")


def _decode_lines_for_diff(data: bytes) -> list[str]:
    """Decode bytes for a readable unified diff (preserve line endings)."""

    if data.startswith(b"\xff\xfe") or data.startswith(b"\xfe\xff"):
        text = data.decode("utf-16", errors="replace").lstrip("\ufeff")
    elif data.startswith(b"\xef\xbb\xbf"):
        text = data.decode("utf-8-sig", errors="replace")
    else:
        text = data.decode("utf-8", errors="replace")
        # If this was UTF-16 without a BOM, it will look like NUL-padded text.
        if "\x00" in text:
            text = text.replace("\x00", "")

    # Keep line endings so difflib produces a readable unified diff.
    out: list[str] = []
    for line in text.splitlines(keepends=True):
        # Make CRLF/CR visible without emitting literal '\r' characters that can
        # break terminal output.
        if line.endswith("\r\n"):
            out.append(line[:-2] + "\\r\n")
        elif line.endswith("\r"):
            out.append(line[:-1] + "\\r\n")
        else:
            out.append(line)
    return out


def main() -> int:
    repo_root = Path(__file__).resolve().parents[4]
    inf_dir = repo_root / "drivers/windows7/virtio-input/inf"

    canonical = inf_dir / "aero_virtio_input.inf"
    if not canonical.exists():
        sys.stderr.write(
            f"virtio-input INF alias drift check: canonical INF not found: {canonical}\n"
        )
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

    try:
        canonical_body = inf_functional_bytes(canonical)
        alias_body = inf_functional_bytes(alias)
    except RuntimeError as e:
        sys.stderr.write(f"{e}\n")
        return 1

    if canonical_body == alias_body:
        print(
            "virtio-input INF alias drift check: OK ({} stays in sync with {} from [Version] onward)".format(
                alias.relative_to(repo_root), canonical.relative_to(repo_root)
            )
        )
        return 0

    canonical_label = str(canonical.relative_to(repo_root))
    alias_label = str(alias.relative_to(repo_root))
    diff = difflib.unified_diff(
        _decode_lines_for_diff(canonical_body),
        _decode_lines_for_diff(alias_body),
        fromfile=canonical_label,
        tofile=alias_label,
        lineterm="",
    )
    sys.stderr.write(
        "virtio-input INF alias drift detected (expected byte-identical from the first section header onward):\n"
        + "".join(diff)
        + "\n"
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
