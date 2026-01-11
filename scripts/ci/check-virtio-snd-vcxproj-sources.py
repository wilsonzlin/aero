#!/usr/bin/env python3
"""
Guardrail: prevent the Windows 7 virtio-snd MSBuild project from silently drifting
back to the legacy virtio-pci I/O-port backend.

Why this exists:
  - `drivers/windows7/virtio-snd` contains both legacy (I/O-port) and modern
    (virtio-pci modern + MMIO) transport implementations.
  - MSBuild will happily succeed even if the project accidentally compiles the
    legacy transport sources, but the resulting `virtiosnd.sys` cannot start
    against Aero's modern virtio device contract.

This check parses `virtio-snd.vcxproj` and enforces:
  - Required modern transport sources are included.
  - Known legacy transport sources are NOT included.
  - Optional (recommended): the project output name matches the INF NTMPDriver
    (`virtiosnd.sys`).

If files are renamed during migration, update REQUIRED_SOURCES / FORBIDDEN_*.
"""

from __future__ import annotations

import re
import sys
import xml.etree.ElementTree as ET
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]

VCXPROJ = REPO_ROOT / "drivers/windows7/virtio-snd/virtio-snd.vcxproj"
INF_FILES = [
    REPO_ROOT / "drivers/windows7/virtio-snd/inf/aero-virtio-snd.inf",
    REPO_ROOT / "drivers/windows7/virtio-snd/inf/virtio-snd.inf",
]

# These are project-relative paths (relative to drivers/windows7/virtio-snd/).
REQUIRED_SOURCES = {
    "src/virtiosnd_hw.c",
    "src/virtio_pci_modern_wdm.c",
    "src/virtiosnd_intx.c",
    "src/virtiosnd_control.c",
    "src/virtiosnd_tx.c",
    "src/backend_virtio.c",
}

# These are project-relative paths (relative to drivers/windows7/virtio-snd/).
FORBIDDEN_PROJECT_SOURCES = {
    "src/aeroviosnd_hw.c",
    "src/backend_virtio_legacy.c",
}

# These are repo-root-relative paths.
FORBIDDEN_REPO_SOURCES = {
    "drivers/windows7/virtio/common/src/virtio_pci_legacy.c",
}

# Forbidden by basename/suffix (regardless of where it comes from).
FORBIDDEN_BASENAMES = {
    "virtio_queue.c",
}


def fail(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(1)


def read_text(path: Path) -> str:
    try:
        return path.read_text(encoding="utf-8", errors="replace")
    except FileNotFoundError:
        fail(f"missing required file: {path.as_posix()}")


def normalize_path(value: str) -> str:
    # MSBuild projects are Windows-first and typically use backslashes. Normalize
    # for stable comparisons in CI (Linux).
    return value.strip().replace("\\", "/")


def expand_msbuild_path(raw: str, project_dir: Path) -> set[Path]:
    """
    Resolve a ClCompile Include/Remove value to concrete paths.

    This intentionally supports only the subset we expect in-tree:
      - relative paths
      - simple globbing (*, ?, **)
      - $(ProjectDir) prefix
    """

    value = normalize_path(raw)

    if value.startswith("$(ProjectDir)"):
        # MSBuild's $(ProjectDir) includes a trailing slash.
        value = str(project_dir.as_posix()) + "/" + value[len("$(ProjectDir)") :].lstrip("/")

    if "$(" in value:
        fail(f"unsupported MSBuild macro in ClCompile entry: {raw!r}")

    if any(ch in value for ch in ["*", "?", "["]):
        # Path.glob is relative to the base directory; if the pattern is absolute
        # (unlikely here), fall back to the directory portion.
        if Path(value).is_absolute():
            base = Path(value).parent
            pattern = Path(value).name
        else:
            base = project_dir
            pattern = value
        return {p.resolve() for p in base.glob(pattern) if p.is_file()}

    path = Path(value)
    if not path.is_absolute():
        path = project_dir / path
    return {path.resolve()}


def parse_vcxproj_compiled_sources(vcxproj: Path) -> tuple[set[str], set[str], set[Path]]:
    """
    Returns:
      - project-relative POSIX paths
      - repo-root-relative POSIX paths (for entries within the repo)
      - absolute paths (best effort; may include paths outside repo)
    """

    project_dir = vcxproj.parent

    try:
        root = ET.parse(vcxproj).getroot()
    except ET.ParseError as e:
        fail(f"invalid XML in {vcxproj.as_posix()}: {e}")

    include_raw: list[str] = []
    remove_raw: list[str] = []

    for elem in root.findall(".//{*}ClCompile"):
        if "Include" in elem.attrib:
            include_raw.append(elem.attrib["Include"])
        if "Remove" in elem.attrib:
            remove_raw.append(elem.attrib["Remove"])

    if not include_raw:
        fail(f"no <ClCompile Include=...> items found in {vcxproj.as_posix()}")

    included: set[Path] = set()
    for raw in include_raw:
        paths = expand_msbuild_path(raw, project_dir)
        if not paths:
            fail(f"ClCompile Include matched no files: {raw!r}")
        included |= paths

    removed: set[Path] = set()
    for raw in remove_raw:
        removed |= expand_msbuild_path(raw, project_dir)

    compiled = {p for p in included if p not in removed}

    project_rel: set[str] = set()
    repo_rel: set[str] = set()

    for p in compiled:
        try:
            project_rel.add(p.relative_to(project_dir).as_posix())
        except ValueError:
            # Unlikely, but keep going; repo_rel will also likely fail.
            pass

        try:
            repo_rel.add(p.relative_to(REPO_ROOT).as_posix())
        except ValueError:
            pass

    return project_rel, repo_rel, compiled


def parse_vcxproj_output_name(vcxproj: Path) -> str:
    try:
        root = ET.parse(vcxproj).getroot()
    except ET.ParseError as e:
        fail(f"invalid XML in {vcxproj.as_posix()}: {e}")

    target_name_elem = root.find(".//{*}TargetName")
    target_ext_elem = root.find(".//{*}TargetExt")

    if target_name_elem is None or not (target_name := (target_name_elem.text or "").strip()):
        fail(f"missing <TargetName> in {vcxproj.as_posix()}")

    target_ext = ".sys"
    if target_ext_elem is not None and (text := (target_ext_elem.text or "").strip()):
        target_ext = text
    if not target_ext.startswith("."):
        target_ext = "." + target_ext

    return f"{target_name}{target_ext}"


def extract_inf_ntmpdriver(inf_path: Path) -> str:
    # Match `HKR,,NTMPDriver,,virtiosnd.sys` (common in this tree) and tolerate
    # optional flags or quoted strings.
    for line in read_text(inf_path).splitlines():
        # Strip inline comments.
        line = line.split(";", 1)[0].strip()
        if not line:
            continue
        if "NTMPDriver" not in line:
            continue

        fields = [f.strip() for f in line.split(",")]
        if len(fields) < 3:
            continue
        if fields[0].upper() != "HKR":
            continue
        if fields[2].upper() != "NTMPDRIVER":
            continue

        # Value is typically the last field.
        value = fields[-1].strip().strip('"')
        if value:
            return value

    fail(f"could not find NTMPDriver registry value in {inf_path.as_posix()}")
    raise AssertionError("unreachable")


def main() -> None:
    project_rel, repo_rel, compiled_paths = parse_vcxproj_compiled_sources(VCXPROJ)

    missing = sorted(p for p in REQUIRED_SOURCES if p not in project_rel)
    if missing:
        fail(
            "virtio-snd.vcxproj is missing required modern sources:\n"
            + "\n".join(f"  - {p}" for p in missing)
        )

    forbidden_found: list[str] = []
    for p in sorted(FORBIDDEN_PROJECT_SOURCES):
        if p in project_rel:
            forbidden_found.append(p)
    for p in sorted(FORBIDDEN_REPO_SOURCES):
        if p in repo_rel:
            forbidden_found.append(p)
    for abs_path in sorted(compiled_paths, key=lambda p: p.as_posix()):
        if abs_path.name in FORBIDDEN_BASENAMES:
            try:
                forbidden_found.append(abs_path.relative_to(REPO_ROOT).as_posix())
            except ValueError:
                forbidden_found.append(abs_path.as_posix())

    if forbidden_found:
        fail(
            "virtio-snd.vcxproj includes forbidden legacy transport sources:\n"
            + "\n".join(f"  - {p}" for p in forbidden_found)
        )

    # Optional/recommended: ensure the produced SYS name matches the INF's NTMPDriver.
    output_name = parse_vcxproj_output_name(VCXPROJ)
    inf_names = {extract_inf_ntmpdriver(p) for p in INF_FILES}
    if len({n.lower() for n in inf_names}) != 1:
        fail(
            "virtio-snd INF files disagree on NTMPDriver:\n"
            + "\n".join(f"  - {p.as_posix()}: {extract_inf_ntmpdriver(p)}" for p in INF_FILES)
        )

    expected = next(iter(inf_names))
    if output_name.lower() != expected.lower():
        fail(
            "virtio-snd output name mismatch between MSBuild project and INF:\n"
            f"  {VCXPROJ.as_posix()}: {output_name}\n"
            f"  NTMPDriver (INF): {expected}"
        )


if __name__ == "__main__":
    main()

