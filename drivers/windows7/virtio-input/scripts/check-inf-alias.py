#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Verify that the virtio-input legacy INF filename alias stays in sync.

The Windows 7 virtio-input driver package has a canonical INF:
  - drivers/windows7/virtio-input/inf/aero_virtio_input.inf

For compatibility with older tooling/workflows, the repo also keeps a legacy
filename alias INF (checked in disabled-by-default):
  - drivers/windows7/virtio-input/inf/virtio-input.inf.disabled

Developers may locally enable the alias by renaming it to `virtio-input.inf`.

Policy:
  - The alias INF is a *legacy filename alias* that also provides an opt-in strict
    generic fallback HWID (`PCI\\VEN_1AF4&DEV_1052&REV_01`) for environments that do
    not expose the Aero subsystem IDs.
  - It is allowed to differ from the canonical INF in the models sections
    (`[Aero.NTx86]` / `[Aero.NTamd64]`) to add that fallback entry.
  - Outside the models sections, it is expected to stay in sync with the canonical
    INF. This check enforces that.

Comparison notes:
  - Comments and the allowed-to-diverge models sections are ignored.
  - Outside the ignored sections, the comparison is strict (all functional lines
    must match).
Run from the repo root:
  python3 drivers/windows7/virtio-input/scripts/check-inf-alias.py
"""

from __future__ import annotations

import difflib
import re
import sys
from pathlib import Path


def _strip_inf_inline_comment(line: str) -> str:
    """Strip a ';' comment from an INF line, but keep semicolons inside quotes."""

    in_quote = False
    out: list[str] = []
    for ch in line:
        if ch == '"':
            in_quote = not in_quote
            out.append(ch)
            continue
        if ch == ";" and not in_quote:
            break
        out.append(ch)
    return "".join(out)


def _read_text(path: Path) -> str:
    """Decode an INF file as text.

    This helper is resilient to common Windows encodings (UTF-16 with or without
    BOM, or UTF-8 with BOM) so drift checks still work if the INF is edited with
    tools that change encodings.
    """

    data = path.read_bytes()
    if data.startswith(b"\xff\xfe") or data.startswith(b"\xfe\xff"):
        return data.decode("utf-16", errors="replace").lstrip("\ufeff")
    if data.startswith(b"\xef\xbb\xbf"):
        return data.decode("utf-8-sig", errors="replace")

    text = data.decode("utf-8", errors="replace")

    # If the file was UTF-16 without a BOM, it will look like NUL-padded UTF-8.
    if "\x00" in text:
        text = text.replace("\x00", "")

    return text


def _normalized_inf_lines_without_sections(path: Path, *, drop_sections: set[str]) -> list[str]:
    """Return a normalized INF representation for drift checks.

    Normalization rules:
    - strips full-line and inline comments (INF comments start with ';' outside quoted strings)
    - drops empty lines
    - removes entire sections (by name, case-insensitive)
    - normalizes section headers to lowercase (INF section names are case-insensitive)
    """

    drop = {s.lower() for s in drop_sections}
    out: list[str] = []
    current_section: str | None = None
    dropping = False

    for raw in _read_text(path).splitlines():
        line = _strip_inf_inline_comment(raw).strip()
        if not line:
            continue

        m = re.match(r"^\[(?P<section>[^\]]+)\]\s*$", line)
        if m:
            current_section = m.group("section").strip()
            dropping = current_section.lower() in drop
            if not dropping:
                # INF section names are case-insensitive.
                out.append(f"[{current_section.lower()}]")
            continue

        if dropping:
            continue

        out.append(line)

    return out

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

    drop_sections = {"Aero.NTx86", "Aero.NTamd64"}
    canonical_lines = _normalized_inf_lines_without_sections(canonical, drop_sections=drop_sections)
    alias_lines = _normalized_inf_lines_without_sections(alias, drop_sections=drop_sections)
    if canonical_lines == alias_lines:
        print(
            "virtio-input INF alias drift check: OK ({} stays in sync with {} outside models sections)".format(
                alias.relative_to(repo_root), canonical.relative_to(repo_root)
            )
        )
        return 0

    sys.stderr.write("virtio-input INF alias drift detected outside models sections.\n")
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
        lineterm="",
    )
    sys.stderr.write("".join(diff))

    return 1


if __name__ == "__main__":
    raise SystemExit(main())
