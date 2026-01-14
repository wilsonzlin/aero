#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""
Verify that the legacy virtio-input INF alias stays in sync.

The Windows 7 virtio-input driver package has a canonical INF:
  - drivers/windows7/virtio-input/inf/aero_virtio_input.inf

For compatibility with older tooling/workflows, the repo also keeps a legacy
filename alias INF (checked in disabled-by-default):
  - drivers/windows7/virtio-input/inf/virtio-input.inf.disabled

To prevent behavior drift, the alias INF should stay in sync with the canonical
INF in all functional content from the first section header (`[Version]`)
onward. The header comment/banner block (before `[Version]`) may differ so the
alias can self-document its purpose.

Run from the repo root:
  python3 drivers/windows7/virtio-input/scripts/check-inf-alias.py
"""

from __future__ import annotations

import difflib
import sys
from pathlib import Path


def inf_functional_bytes(path: Path) -> bytes:
    """
    Return the INF bytes starting from the first section header.

    This intentionally ignores the leading comment/banner block so a legacy alias
    INF can use a different filename header while still enforcing byte-for-byte
    equality of all functional sections/keys.
    """

    data = path.read_bytes()
    lines = data.splitlines(keepends=True)
    for i, line in enumerate(lines):
        stripped = line.lstrip(b" \t")
        if stripped.startswith(b"["):
            return b"".join(lines[i:])
        if stripped.startswith(b";") or stripped in (b"\n", b"\r\n", b"\r"):
            continue
        return b"".join(lines[i:])
    raise RuntimeError(f"{path}: could not find a section header (e.g. [Version])")


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
            "virtio-input INF alias check: no alias INF found "
            "(expected virtio-input.inf or virtio-input.inf.disabled); skipping.\n"
        )
        return 0

    canonical_body = inf_functional_bytes(canonical)
    alias_body = inf_functional_bytes(alias)
    if canonical_body == alias_body:
        return 0

    sys.stderr.write("virtio-input INF alias drift detected.\n")
    sys.stderr.write("The alias INF must match the canonical INF from [Version] onward.\n\n")

    # Use repo-relative paths in the diff output to keep it readable and stable
    # across machines/CI environments.
    canonical_label = str(canonical.relative_to(repo_root))
    alias_label = str(alias.relative_to(repo_root))

    diff = difflib.unified_diff(
        canonical_body.decode("utf-8", errors="replace").splitlines(keepends=True),
        alias_body.decode("utf-8", errors="replace").splitlines(keepends=True),
        fromfile=canonical_label,
        tofile=alias_label,
        lineterm="\n",
    )
    for line in diff:
        sys.stderr.write(line)

    return 1


if __name__ == "__main__":
    raise SystemExit(main())
