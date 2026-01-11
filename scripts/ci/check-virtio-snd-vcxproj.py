#!/usr/bin/env python3
"""
Guardrail: prevent drivers/windows7/virtio-snd/virtio-snd.vcxproj from drifting
back to the legacy virtio backend.

The Windows 7 virtio-snd driver package (INF + emulator contract) is modern-only.
Historically, the .vcxproj regressed to compiling legacy sources that will build
fine but cannot talk to the modern-only device.

This check is intentionally lightweight: it parses the MSBuild project file and
asserts that specific legacy .c files are *not* part of the ClCompile item list,
and that the modern bring-up sources are present.
"""

from __future__ import annotations

import argparse
import glob
import os
import sys
import xml.etree.ElementTree as ET
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]
DEFAULT_VCXPROJ = REPO_ROOT / "drivers/windows7/virtio-snd/virtio-snd.vcxproj"

MSBUILD_NS = "http://schemas.microsoft.com/developer/msbuild/2003"

FORBIDDEN_BASENAMES = {
    "backend_virtio_legacy.c",
    "aeroviosnd_hw.c",
    "virtio_pci_legacy.c",
    "virtio_queue.c",
}

REQUIRED_BASENAMES = {
    "virtiosnd_hw.c",
    "virtio_pci_modern_wdm.c",
    "virtiosnd_control.c",
    "virtiosnd_tx.c",
}


def fail(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(1)


def _expand_msbuild_path(
    project_dir: Path,
    raw: str,
    *,
    missing_ok: bool,
) -> list[Path]:
    """
    Expand a ClCompile Include/Remove path into concrete file paths.

    This handles the subset of MSBuild patterns used in this repo:
      - Relative paths (Windows-style separators)
      - Recursive globs (e.g. src\\**\\*.c)
      - $(ProjectDir) macro (best-effort)
    """

    value = raw.strip()
    if not value:
        return []

    # Best-effort $(ProjectDir) expansion (MSBuild uses a trailing separator).
    value = value.replace("$(ProjectDir)", str(project_dir) + os.sep)

    # Python's glob expects the host OS separator; normalize to '/' first and
    # then hand it back to Path/resolve().
    value_norm = value.replace("\\", "/")

    # Resolve relative patterns from the .vcxproj directory.
    pattern = (
        value_norm
        if os.path.isabs(value_norm)
        else (project_dir / value_norm).as_posix()
    )

    has_glob = any(ch in pattern for ch in ["*", "?", "["])
    if has_glob:
        return [Path(p).resolve() for p in glob.glob(pattern, recursive=True) if Path(p).is_file()]

    p = Path(pattern).resolve()
    if not p.exists():
        if missing_ok:
            return []
        fail(f"vcxproj references missing file: {raw} (resolved to {p})")
    if not p.is_file():
        return []
    return [p]


def parse_effective_clcompile_includes(vcxproj_path: Path) -> list[Path]:
    try:
        root = ET.fromstring(vcxproj_path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        fail(f"missing vcxproj: {vcxproj_path.as_posix()}")
    except ET.ParseError as e:
        fail(f"failed to parse XML from {vcxproj_path.as_posix()}: {e}")

    project_dir = vcxproj_path.parent.resolve()

    includes: set[Path] = set()
    removes: set[Path] = set()

    for elem in root.iter():
        if elem.tag != f"{{{MSBUILD_NS}}}ClCompile":
            continue
        inc = elem.get("Include")
        rem = elem.get("Remove")
        if inc:
            for p in _expand_msbuild_path(project_dir, inc, missing_ok=False):
                includes.add(p)
        if rem:
            for p in _expand_msbuild_path(project_dir, rem, missing_ok=True):
                removes.add(p)

    # MSBuild evaluation can include conditions/imports; this repo's virtio-snd
    # project uses explicit includes + Remove overrides, which we can model as:
    #   effective = union(includes) - union(removes)
    return sorted(includes - removes)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--vcxproj",
        type=Path,
        default=DEFAULT_VCXPROJ,
        help="Path to the virtio-snd.vcxproj to validate (defaults to the repo copy).",
    )
    args = parser.parse_args()

    vcxproj_path: Path = args.vcxproj
    if not vcxproj_path.is_absolute():
        vcxproj_path = (REPO_ROOT / vcxproj_path).resolve()

    effective = parse_effective_clcompile_includes(vcxproj_path)
    basenames_present = {p.name.lower() for p in effective}

    forbidden_present = sorted(
        {b for b in FORBIDDEN_BASENAMES if b in basenames_present}
    )
    required_missing = sorted(
        {b for b in REQUIRED_BASENAMES if b not in basenames_present}
    )

    if not forbidden_present and not required_missing:
        print(
            f"ok: {vcxproj_path.relative_to(REPO_ROOT).as_posix()} uses modern virtio-snd sources"
        )
        return

    # Build basename -> concrete project paths mapping for actionable output.
    paths_by_base: dict[str, set[str]] = {}
    for p in effective:
        b = p.name.lower()
        try:
            display = p.relative_to(REPO_ROOT).as_posix()
        except ValueError:
            display = str(p)
        paths_by_base.setdefault(b, set()).add(display)

    lines: list[str] = []
    rel = vcxproj_path.relative_to(REPO_ROOT).as_posix()
    lines.append(f"{rel} is not aligned with the modern-only virtio-snd backend.")

    if forbidden_present:
        lines.append("")
        lines.append("Forbidden legacy sources were found in <ClCompile Include=...>:")
        for b in forbidden_present:
            for p in sorted(paths_by_base.get(b, {b})):
                lines.append(f"  - {p}")

    if required_missing:
        lines.append("")
        lines.append("Required modern sources are missing from <ClCompile Include=...>:")
        for b in required_missing:
            lines.append(f"  - {b}")

    lines.append("")
    lines.append("Fix: edit the .vcxproj so it only builds the modern virtio-snd transport/backend.")

    fail("\n".join(lines))


if __name__ == "__main__":
    main()
