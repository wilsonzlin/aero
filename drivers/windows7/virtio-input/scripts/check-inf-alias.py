#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""
Verify that the legacy virtio-input INF alias stays in sync.

The Windows 7 virtio-input driver package has a canonical INF:
  - drivers/windows7/virtio-input/inf/aero_virtio_input.inf

For compatibility with older tooling/workflows, the repo also keeps a legacy
filename alias INF (checked in disabled-by-default):
  - drivers/windows7/virtio-input/inf/virtio-input.inf.disabled

Policy:
  - The alias INF is allowed to differ in the models sections (`Aero.NTx86` /
    `Aero.NTamd64`) so it can provide an opt-in generic fallback HWID.
  - Outside the models sections, the alias must stay in sync with the canonical
    INF (from the first section header onward).

Comparison notes:
  - Comments and empty lines are ignored.
  - Section names are treated case-insensitively.

Run from the repo root:
  python3 drivers/windows7/virtio-input/scripts/check-inf-alias.py
"""

from __future__ import annotations

import difflib
import sys
from pathlib import Path


MODELS_SECTIONS = {"aero.ntx86", "aero.ntamd64"}


def strip_inf_comments(line: str) -> str:
    """
    Remove INF comments (starting with ';') outside of quoted strings.
    """

    out: list[str] = []
    in_quote = False
    for ch in line:
        if ch == '"':
            in_quote = not in_quote
        if (not in_quote) and ch == ";":
            break
        out.append(ch)
    return "".join(out)


def inf_functional_lines(path: Path) -> list[str]:
    """
    Return normalized INF lines for comparison.

    - Starts at the first section header (or the first unexpected functional
      line if one appears before any section header).
    - Drops models sections (Aero.NTx86 / Aero.NTamd64) entirely.
    - Drops comments and empty lines.
    """

    raw_lines = path.read_text(encoding="utf-8", errors="replace").splitlines()

    start = None
    for i, line in enumerate(raw_lines):
        stripped = line.lstrip(" \t")
        if stripped.startswith("["):
            start = i
            break
        if stripped.startswith(";") or stripped.strip() == "":
            continue
        # Unexpected functional content before the first section header: keep it.
        start = i
        break
    if start is None:
        raise RuntimeError(f"{path}: could not find a section header (e.g. [Version])")

    out: list[str] = []
    skip_section = False
    for line in raw_lines[start:]:
        stripped = line.lstrip(" \t")
        if stripped.startswith("[") and "]" in stripped:
            sect_name = stripped[1 : stripped.index("]")].strip()
            skip_section = sect_name.lower() in MODELS_SECTIONS
            if skip_section:
                continue
            out.append(f"[{sect_name}]")
            continue

        if skip_section:
            continue

        no_comment = strip_inf_comments(line).strip()
        if no_comment == "":
            continue
        out.append(no_comment)

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

    canonical_body = inf_functional_lines(canonical)
    alias_body = inf_functional_lines(alias)
    if canonical_body == alias_body:
        return 0

    sys.stderr.write("virtio-input INF alias drift detected.\n")
    sys.stderr.write(
        "The alias INF must match the canonical INF from [Version] onward (excluding models sections Aero.NTx86/Aero.NTamd64).\n\n"
    )

    # Use repo-relative paths in the diff output to keep it readable and stable
    # across machines/CI environments.
    canonical_label = str(canonical.relative_to(repo_root))
    alias_label = str(alias.relative_to(repo_root))

    diff = difflib.unified_diff(
        [l + "\n" for l in canonical_body],
        [l + "\n" for l in alias_body],
        fromfile=canonical_label,
        tofile=alias_label,
        lineterm="\n",
    )
    for line in diff:
        sys.stderr.write(line)

    return 1


if __name__ == "__main__":
    raise SystemExit(main())
