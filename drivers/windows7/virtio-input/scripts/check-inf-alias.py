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
INF in all functional content *except* the models sections (`[Aero.NTx86]` and
`[Aero.NTamd64]`). The alias is allowed to include an additional generic fallback
HWID model entry for opt-in driver binding.

Run from the repo root:
  python3 drivers/windows7/virtio-input/scripts/check-inf-alias.py
"""

from __future__ import annotations

import difflib
import re
import sys
from pathlib import Path


def _normalized_inf_lines_without_sections(path: Path, *, drop_sections: set[str]) -> list[str]:
    """
    Normalized INF representation for drift checks:
    - strips full-line and inline comments
    - drops empty lines
    - optionally removes entire sections (by name, case-insensitive)
    """

    drop = {s.lower() for s in drop_sections}
    out: list[str] = []
    current_section: str | None = None
    dropping = False

    text = path.read_text(encoding="utf-8", errors="replace")
    for raw in text.splitlines():
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
            dropping = current_section.lower() in drop
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

    # The legacy alias INF is allowed to differ only in the models sections.
    drop_sections = {"Aero.NTx86", "Aero.NTamd64"}
    canonical_lines = _normalized_inf_lines_without_sections(
        canonical, drop_sections=drop_sections
    )
    alias_lines = _normalized_inf_lines_without_sections(alias, drop_sections=drop_sections)
    if canonical_lines == alias_lines:
        return 0

    sys.stderr.write("virtio-input INF alias drift detected outside ignored sections.\n")
    sys.stderr.write(f"Ignored sections: {sorted(drop_sections)}\n\n")

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
