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
  - The canonical INF *and* the legacy alias INF both include:
      - explicit SUBSYS-qualified model lines for the Aero contract v1 keyboard
        (SUBSYS_0010) and mouse (SUBSYS_0011), and
      - a strict revision-gated generic fallback HWID (no SUBSYS):
          PCI\\VEN_1AF4&DEV_1052&REV_01
    The fallback is required by the current Windows device/driver contract policy
    (and by `tools/device_contract_validator`) so binding remains revision-gated
    even when subsystem IDs are absent/ignored.
  - The alias INF is allowed to differ in the models sections (`[Aero.NTx86]` /
    `[Aero.NTamd64]`), but must include the same required HWID bindings.
  - Outside the models sections, the alias must stay in sync with the canonical
    INF (from the first section header onward).

Comparison notes:
  - Comments and empty lines are ignored.
  - Section names are treated case-insensitively (normalized to lowercase for comparison).

Run from the repo root:
  python3 drivers/windows7/virtio-input/scripts/check-inf-alias.py
"""

from __future__ import annotations

import difflib
import sys
from pathlib import Path


MODELS_SECTIONS = {"aero.ntx86", "aero.ntamd64"}
BASE_HWID = r"PCI\VEN_1AF4&DEV_1052"
FALLBACK_HWID = r"PCI\VEN_1AF4&DEV_1052&REV_01"
KEYBOARD_HWID = r"PCI\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01"
MOUSE_HWID = r"PCI\VEN_1AF4&DEV_1052&SUBSYS_00111AF4&REV_01"
TABLET_SUBSYS = "SUBSYS_00121AF4"


def strip_inf_comments(line: str) -> str:
    """Remove INF comments (starting with ';') outside of quoted strings."""

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

    start: int | None = None
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
            # INF section names are case-insensitive. Normalize them so we don't
            # flag drift due to casing-only differences.
            sect_name_norm = sect_name.lower()
            skip_section = sect_name_norm in MODELS_SECTIONS
            if skip_section:
                continue
            out.append(f"[{sect_name_norm}]")
            continue

        if skip_section:
            continue

        no_comment = strip_inf_comments(line).strip()
        if no_comment == "":
            continue
        out.append(no_comment)
    return out


def find_hwid_model_lines(*, path: Path, section: str, hwid: str) -> tuple[bool, list[str]]:
    """
    Find model lines containing `hwid` within a specific models section.

    Returns `(section_seen, matches)` where `matches` contains the (comment-stripped)
    lines that included `hwid`.
    """

    raw_lines = path.read_text(encoding="utf-8", errors="replace").splitlines()

    target = section.lower()
    section_seen = False
    matches: list[str] = []

    current: str | None = None
    for line in raw_lines:
        stripped = line.lstrip(" \t")
        if stripped.startswith("[") and "]" in stripped:
            current = stripped[1 : stripped.index("]")].strip().lower()
            if current == target:
                section_seen = True
            continue

        if current != target:
            continue

        no_comment = strip_inf_comments(line).strip()
        if not no_comment:
            continue

        if hwid.lower() in no_comment.lower():
            matches.append(no_comment)

    return section_seen, matches


def section_active_lines(*, path: Path, section: str) -> tuple[bool, list[str]]:
    """
    Return all non-empty, comment-stripped lines within a section.

    This is used for lightweight policy checks (no full INF parser).
    """

    raw_lines = path.read_text(encoding="utf-8", errors="replace").splitlines()

    target = section.lower()
    section_seen = False
    current: str | None = None
    lines: list[str] = []

    for line in raw_lines:
        stripped = line.lstrip(" \t")
        if stripped.startswith("[") and "]" in stripped:
            current = stripped[1 : stripped.index("]")].strip().lower()
            if current == target:
                section_seen = True
            continue

        if current != target:
            continue

        no_comment = strip_inf_comments(line).strip()
        if not no_comment:
            continue
        lines.append(no_comment)

    return section_seen, lines


def enforce_models_policy(*, path: Path) -> list[str]:
    """
    Enforce the virtio-input model line policy for a given INF.

    Returns a list of human-readable error strings (empty if OK).
    """

    errors: list[str] = []
    for sect in ("Aero.NTx86", "Aero.NTamd64"):
        seen, lines = section_active_lines(path=path, section=sect)
        if not seen:
            errors.append(f"{path}: missing required models section [{sect}].")
            continue

        def _matches(needle: str) -> list[str]:
            n = needle.lower()
            return [l for l in lines if n in l.lower()]

        kb = _matches(KEYBOARD_HWID)
        ms = _matches(MOUSE_HWID)
        fb = _matches(FALLBACK_HWID)

        if len(kb) != 1:
            errors.append(
                f"{path}: [{sect}] expected exactly one keyboard model line ({KEYBOARD_HWID}); "
                + ("missing." if not kb else f"found {len(kb)}:\n  " + "\n  ".join(kb))
            )
        if len(ms) != 1:
            errors.append(
                f"{path}: [{sect}] expected exactly one mouse model line ({MOUSE_HWID}); "
                + ("missing." if not ms else f"found {len(ms)}:\n  " + "\n  ".join(ms))
            )
        if len(fb) != 1:
            errors.append(
                f"{path}: [{sect}] expected exactly one strict fallback model line ({FALLBACK_HWID}); "
                + ("missing." if not fb else f"found {len(fb)}:\n  " + "\n  ".join(fb))
            )

        # Reject any rev-less matches: we require revision gating to encode the contract major
        # version and to avoid binding to non-contract devices.
        revless = [
            l
            for l in lines
            if BASE_HWID.lower() in l.lower() and "&rev_" not in l.lower()
        ]
        if revless:
            errors.append(
                f"{path}: [{sect}] contains rev-less model line(s) matching {BASE_HWID} (missing &REV_.. qualifier):\n  "
                + "\n  ".join(revless)
            )

        # Tablet subsystem IDs belong in `aero_virtio_tablet.inf` (more specific HWID), not here.
        tablet = [l for l in lines if TABLET_SUBSYS.lower() in l.lower()]
        if tablet:
            errors.append(
                f"{path}: [{sect}] contains unexpected tablet subsystem model line(s) ({TABLET_SUBSYS}); "
                "tablet devices must bind via aero_virtio_tablet.inf:\n  " + "\n  ".join(tablet)
            )

    return errors


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
        # Even if the non-models sections match, enforce the models/HWID policy.
        # This guards against accidental driver binding changes which can silently
        # break in-guest installs and Guest Tools packaging/validation.
        errors = [*enforce_models_policy(path=canonical), *enforce_models_policy(path=alias)]
        if errors:
            sys.stderr.write("virtio-input INF models policy violation(s):\n")
            for e in errors:
                sys.stderr.write(f"- {e}\n")
            return 1
        return 0

    sys.stderr.write("virtio-input INF alias drift detected.\n")
    sys.stderr.write(
        "The alias INF must match the canonical INF from [Version] onward "
        "(excluding models sections Aero.NTx86/Aero.NTamd64).\n\n"
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
