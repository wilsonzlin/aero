#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""\
Verify that the legacy virtio-input INF alias stays in sync.

The Windows 7 virtio-input driver package has a canonical INF:
  - drivers/windows7/virtio-input/inf/aero_virtio_input.inf

For compatibility with older tooling/workflows, the repo also keeps a legacy
filename alias INF (checked in disabled-by-default):
  - drivers/windows7/virtio-input/inf/virtio-input.inf.disabled

Developers may locally enable the alias by renaming it to `virtio-input.inf`.

Policy:
  - The alias INF is allowed to differ in the models sections (`Aero.NTx86` /
    `Aero.NTamd64`), but should otherwise remain identical to the canonical INF.
  - Outside the models sections, the alias must stay in sync with the canonical
    INF (from the first section header onward).
  - The canonical INF is intentionally strict and SUBSYS-gated only (no generic
    fallback HWID).
  - The legacy alias INF must include the strict, revision-gated generic fallback
    HWID (`PCI\\VEN_1AF4&DEV_1052&REV_01`) in its models sections.

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
FALLBACK_HWID = r"PCI\VEN_1AF4&DEV_1052&REV_01"


def read_text(path: Path) -> str:
    """
    Read an INF-like text file robustly.

    Windows tooling commonly writes INFs as UTF-16 (sometimes without a BOM). CI
    guardrails should still be able to parse those files.
    """

    data = path.read_bytes()

    # BOM handling.
    if data.startswith(b"\xff\xfe") or data.startswith(b"\xfe\xff"):
        try:
            return data.decode("utf-16").lstrip("\ufeff")
        except UnicodeDecodeError as e:
            raise RuntimeError(f"{path}: failed to decode file as UTF-16 (has BOM): {e}") from e

    # Try UTF-8 first (accept UTF-8 BOM).
    utf8_text: str | None
    try:
        utf8_text = data.decode("utf-8-sig")
    except UnicodeDecodeError:
        utf8_text = None
    else:
        # Fast path: no NULs means we almost certainly decoded correctly.
        if "\x00" not in utf8_text:
            return utf8_text

    # Heuristic detection for UTF-16 without BOM.
    # Plain ASCII UTF-16LE will have many 0x00 bytes at odd indices; UTF-16BE will
    # have many 0x00 bytes at even indices.
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
        try:
            return data.decode(enc).lstrip("\ufeff")
        except UnicodeDecodeError as e:
            raise RuntimeError(f"{path}: failed to decode file as {enc} (heuristic): {e}") from e

    if utf8_text is not None:
        raise RuntimeError(f"{path}: decoded as UTF-8 but contained NUL bytes (likely UTF-16 without BOM)")
    raise RuntimeError(f"{path}: failed to decode file as UTF-8 (unexpected encoding)")


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

    raw_lines = read_text(path).splitlines()

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

    raw_lines = read_text(path).splitlines()

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

    if canonical_body != alias_body:
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

    # Even if the functional regions match, enforce the models section policy:
    # - Canonical INF is SUBSYS-gated only (no generic fallback match).
    # - Alias INF adds an opt-in strict generic fallback HWID for environments that
    #   do not expose Aero subsystem IDs.
    for sect in ("Aero.NTx86", "Aero.NTamd64"):
        canonical_seen, canonical_matches = find_hwid_model_lines(path=canonical, section=sect, hwid=FALLBACK_HWID)
        alias_seen, alias_matches = find_hwid_model_lines(path=alias, section=sect, hwid=FALLBACK_HWID)

        if not canonical_seen:
            sys.stderr.write(f"{canonical}: missing required models section [{sect}].\n")
            return 1
        if not alias_seen:
            sys.stderr.write(f"{alias}: missing required models section [{sect}].\n")
            return 1

        if canonical_matches:
            sys.stderr.write(
                "virtio-input INF policy violation: the canonical INF must not include the "
                f"generic fallback model line {FALLBACK_HWID} (it should be SUBSYS-gated only).\n"
            )
            sys.stderr.write(f"Unexpected fallback model line(s) in {canonical} [{sect}]:\n")
            for line in canonical_matches:
                sys.stderr.write(f"  {line}\n")
            return 1

        if len(alias_matches) != 1:
            sys.stderr.write(
                "virtio-input INF policy violation: the legacy alias INF must include exactly one "
                f"strict fallback model line {FALLBACK_HWID}.\n"
            )
            if not alias_matches:
                sys.stderr.write(f"Missing fallback model line in {alias} [{sect}].\n")
            else:
                sys.stderr.write(f"Found {len(alias_matches)} fallback model line(s) in {alias} [{sect}]:\n")
                for line in alias_matches:
                    sys.stderr.write(f"  {line}\n")
            return 1

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
