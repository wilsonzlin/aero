#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""
Verify that the legacy virtio-snd INF alias stays in sync.

The Windows 7 virtio-snd driver package has a canonical INF:
  - drivers/windows7/virtio-snd/inf/aero_virtio_snd.inf

For compatibility with older tooling/workflows, the repo also keeps a legacy
filename alias INF (checked in disabled-by-default):
  - drivers/windows7/virtio-snd/inf/virtio-snd.inf.disabled

To prevent behavior drift, the two files must be identical in all *functional*
content (everything from the first section header, usually `[Version]`, onward).
Only the leading comment/header block may differ.

Run from the repo root:
  python3 drivers/windows7/virtio-snd/scripts/check-inf-alias.py
"""

from __future__ import annotations

import difflib
import sys
from pathlib import Path


def _functional_bytes(path: Path) -> bytes:
    """
    Return the file content starting from the first section header line.

    We intentionally ignore the leading comment/header block so the alias INF can
    have a different filename banner, while still enforcing byte-for-byte
    equality for all sections/keys.
    """

    data = path.read_bytes()
    lines = data.splitlines(keepends=True)

    for i, line in enumerate(lines):
        stripped = line.lstrip(b" \t")

        # First section header (e.g. "[Version]") starts the functional region.
        if stripped.startswith(b"["):
            return b"".join(lines[i:])

        # Ignore leading comments and blank lines.
        if stripped.startswith(b";") or stripped in (b"\n", b"\r\n", b"\r"):
            continue

        # Unexpected preamble content (not comment, not blank, not section):
        # treat it as functional to avoid masking drift.
        return b"".join(lines[i:])

    raise RuntimeError(f"{path}: could not find a section header (e.g. [Version])")


def main() -> int:
    virtio_snd_root = Path(__file__).resolve().parents[1]
    inf_dir = virtio_snd_root / "inf"

    canonical = inf_dir / "aero_virtio_snd.inf"
    alias = inf_dir / "virtio-snd.inf.disabled"

    canonical_body = _functional_bytes(canonical)
    alias_body = _functional_bytes(alias)

    if canonical_body == alias_body:
        return 0

    sys.stderr.write("virtio-snd INF alias drift detected.\n")
    sys.stderr.write("The alias INF must match the canonical INF from [Version] onward.\n\n")

    canonical_lines = canonical_body.decode("utf-8", errors="replace").splitlines(keepends=True)
    alias_lines = alias_body.decode("utf-8", errors="replace").splitlines(keepends=True)

    diff = difflib.unified_diff(
        canonical_lines,
        alias_lines,
        fromfile=str(canonical),
        tofile=str(alias),
        lineterm="",
    )
    for line in diff:
        sys.stderr.write(line)
    sys.stderr.write("\n")

    return 1


if __name__ == "__main__":
    raise SystemExit(main())

