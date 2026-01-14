#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Verify that the virtio-input legacy INF filename alias stays in sync.

Canonical INF:
  - drivers/windows7/virtio-input/inf/aero_virtio_input.inf

Legacy filename alias INF (checked in disabled-by-default):
  - drivers/windows7/virtio-input/inf/virtio-input.inf.disabled

Developers may locally enable the alias by renaming it to `virtio-input.inf`.

Policy:
  - The alias INF is a *filename alias only*.
  - From the first section header line (typically `[Version]`) onward, the alias
    must remain byte-for-byte identical to the canonical INF.
  - Only the leading banner/comments before the first section header may differ.

Run from the repo root:
  python3 drivers/windows7/virtio-input/scripts/check-inf-alias.py
"""

from __future__ import annotations

import difflib
import sys
from pathlib import Path


def _inf_functional_bytes(path: Path) -> bytes:
    """Return the file bytes starting from the first section header line."""

    data = path.read_bytes()
    lines = data.splitlines(keepends=True)

    def _first_nonblank_ascii_byte(line: bytes, *, first_line: bool) -> int | None:
        # Return the first meaningful ASCII byte on a line, ignoring whitespace and NULs.
        # This makes the scan robust to UTF-16LE/BE INFs (with or without a BOM).
        if first_line:
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
        first = _first_nonblank_ascii_byte(line, first_line=(i == 0))
        if first is None:
            continue

        # First section header (e.g. "[Version]") starts the compared region.
        if first == ord("["):
            return b"".join(lines[i:])

        # Ignore leading comments.
        if first == ord(";"):
            continue

        # Unexpected preamble content (not comment, not blank, not section): treat it as
        # functional to avoid masking drift.
        return b"".join(lines[i:])

    raise RuntimeError(f"{path}: could not find a section header (e.g. [Version])")


def _decode_lines_for_diff(data: bytes) -> list[str]:
    """Decode bytes for readable unified diff output."""

    if data.startswith(b"\xff\xfe") or data.startswith(b"\xfe\xff"):
        text = data.decode("utf-16", errors="replace").lstrip("\ufeff")
    elif data.startswith(b"\xef\xbb\xbf"):
        text = data.decode("utf-8-sig", errors="replace")
    else:
        text = data.decode("utf-8", errors="replace")
        # If this was UTF-16 without a BOM, it will look like NUL-padded UTF-8.
        if "\x00" in text:
            text = text.replace("\x00", "")

    out: list[str] = []
    for line in text.splitlines(keepends=True):
        # Make CRLF/CR visible without emitting literal '\r' characters.
        if line.endswith("\r\n"):
            out.append(line[:-2] + "\\r\n")
        elif line.endswith("\r"):
            out.append(line[:-1] + "\\r\n")
        else:
            out.append(line)
    return out


def main() -> int:
    virtio_input_root = Path(__file__).resolve().parents[1]
    repo_root = virtio_input_root.parents[2]
    inf_dir = virtio_input_root / "inf"

    canonical = inf_dir / "aero_virtio_input.inf"
    if not canonical.exists():
        sys.stderr.write(
            "virtio-input INF alias drift check: canonical INF not found: {}\n".format(
                canonical.relative_to(repo_root)
            )
        )
        return 1

    # The repo keeps the alias checked in disabled-by-default, but developers may locally enable
    # it by renaming to `virtio-input.inf`. Support both so the check can be used in either state.
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

    canonical_body = _inf_functional_bytes(canonical)
    alias_body = _inf_functional_bytes(alias)
    if canonical_body == alias_body:
        print(
            "virtio-input INF alias drift check: OK ({} stays in sync with {} outside banner/comments)".format(
                alias.relative_to(repo_root), canonical.relative_to(repo_root)
            )
        )
        return 0

    sys.stderr.write("virtio-input INF alias drift detected.\n")
    sys.stderr.write(
        "The alias INF must match the canonical INF from the first section header ([Version]) onward "
        "(byte-for-byte; only the leading banner/comments may differ).\n\n"
    )

    diff = difflib.unified_diff(
        _decode_lines_for_diff(canonical_body),
        _decode_lines_for_diff(alias_body),
        fromfile=str(canonical.relative_to(repo_root)),
        tofile=str(alias.relative_to(repo_root)),
        lineterm="\n",
    )
    sys.stderr.write("".join(diff))
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
