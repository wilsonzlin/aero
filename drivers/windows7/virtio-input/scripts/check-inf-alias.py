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
INF for all *functional* content, except for the models sections:
  - [Aero.NTx86]
  - [Aero.NTamd64]

The canonical INF is intentionally SUBSYS-only (keyboard + mouse) so it does not
overlap with the tablet INF. The legacy alias INF is allowed to include an
opt-in generic fallback model line (no SUBSYS), but it must otherwise mirror the
canonical INF.

Run from the repo root:
  python3 drivers/windows7/virtio-input/scripts/check-inf-alias.py
"""

from __future__ import annotations

import difflib
import re
import sys
from pathlib import Path


_DROP_SECTIONS = {"aero.ntx86", "aero.ntamd64"}


def _normalized_lines(path: Path) -> list[str]:
    """
    Return a normalized representation of the INF for drift checks.

    - Full-line and inline comments are stripped.
    - Empty lines are dropped.
    - The models sections are excluded, since the alias INF is allowed to differ
      there (it provides an opt-in generic fallback HWID).
    """

    out: list[str] = []
    current_section: str | None = None
    dropping = False

    for raw in path.read_text(encoding="utf-8", errors="replace").splitlines():
        line = raw.strip()
        if not line:
            continue
        if line.startswith(";"):
            continue
        if ";" in line:
            line = line.split(";", 1)[0].rstrip()
            if not line:
                continue

        m = re.match(r"^\[(?P<section>[^\]]+)\]\s*$", line)
        if m:
            current_section = m.group("section").strip()
            dropping = current_section.lower() in _DROP_SECTIONS
            if not dropping:
                out.append(f"[{current_section}]")
            continue

        if current_section is None or dropping:
            continue

        out.append(line)

    return out


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

    canonical_lines = _normalized_lines(canonical)
    alias_lines = _normalized_lines(alias)
    if canonical_lines == alias_lines:
        return 0

    sys.stderr.write("virtio-input INF alias drift detected.\n")
    sys.stderr.write(
        "The alias INF must match the canonical INF outside the models sections "
        f"{sorted(_DROP_SECTIONS)}.\n\n"
    )

    # Use repo-relative paths in the diff output to keep it readable and stable
    # across machines/CI environments.
    canonical_label = str(canonical.relative_to(repo_root))
    alias_label = str(alias.relative_to(repo_root))

    diff = difflib.unified_diff(
        [l + "\n" for l in canonical_lines],
        [l + "\n" for l in alias_lines],
        fromfile=canonical_label,
        tofile=alias_label,
        lineterm="\n",
    )
    for line in diff:
        sys.stderr.write(line)

    return 1


if __name__ == "__main__":
    raise SystemExit(main())
