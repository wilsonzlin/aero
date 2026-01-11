#!/usr/bin/env python3
"""
Guardrail: ensure Aero Windows drivers build against the *intended* split virtqueue
implementation, without relying on ambiguous header names or include-path ordering.

Background
----------
The repository intentionally contains *two* split-ring implementations:

  1) Canonical (shipping) engine:
     - drivers/windows/virtio/common/virtqueue_split.{c,h}
     - API surface: `VIRTQ_SPLIT` / `VirtqSplit*`

  2) Legacy portable engine (kept for transitional/QEMU experiments + host tests):
     - drivers/windows7/virtio/common/src/virtqueue_split_legacy.c
     - drivers/windows7/virtio/common/include/virtqueue_split_legacy.h
     - API surface: `virtqueue_split_*` + `virtio_os_ops_t`

All shipped Windows 7 virtio drivers (blk/net/input/snd) are expected to use the
canonical engine. The legacy portable engine is retained for host-side unit
tests and compatibility/experimentation. This script encodes that intended
wiring and fails on drift.
"""

from __future__ import annotations

import sys
import xml.etree.ElementTree as ET
from pathlib import Path

REPO_ROOT = Path(__file__).resolve().parents[2]

CANONICAL_VQ_C = REPO_ROOT / "drivers/windows/virtio/common/virtqueue_split.c"
CANONICAL_VQ_H = REPO_ROOT / "drivers/windows/virtio/common/virtqueue_split.h"
LEGACY_VQ_C = REPO_ROOT / "drivers/windows7/virtio/common/src/virtqueue_split_legacy.c"


def fail(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    raise SystemExit(1)


def normalize(value: str) -> str:
    return value.replace("\\", "/").strip()


def parse_vcxproj_compiled_clcompile_includes(vcxproj: Path) -> set[str]:
    """
    Return the set of *compiled* ClCompile Include values (normalized to POSIX-ish
    paths). We support the simple <ClCompile Remove=...> pattern that appears in
    this repo.

    Note: we intentionally do not expand globs here; the guarded driver projects
    list `virtqueue_split.c` explicitly.
    """

    try:
        root = ET.parse(vcxproj).getroot()
    except ET.ParseError as e:
        fail(f"invalid XML in {vcxproj.as_posix()}: {e}")

    includes: set[str] = set()
    removes: set[str] = set()

    for elem in root.findall(".//{*}ClCompile"):
        if "Include" in elem.attrib:
            includes.add(normalize(elem.attrib["Include"]))
        if "Remove" in elem.attrib:
            removes.add(normalize(elem.attrib["Remove"]))

    if not includes:
        fail(f"no <ClCompile Include=...> items found in {vcxproj.as_posix()}")

    return {p for p in includes if p not in removes}


def parse_vcxproj_additional_include_dirs(vcxproj: Path) -> str:
    try:
        root = ET.parse(vcxproj).getroot()
    except ET.ParseError as e:
        fail(f"invalid XML in {vcxproj.as_posix()}: {e}")

    values: list[str] = []
    for elem in root.findall(".//{*}AdditionalIncludeDirectories"):
        if elem.text:
            values.append(normalize(elem.text))

    return ";".join(values)


def require_contains(path: Path, *, needle: str, context: str) -> None:
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except FileNotFoundError:
        fail(f"missing required file: {path.as_posix()}")
    if needle not in normalize(text):
        fail(f"{context}: expected {path.as_posix()} to contain: {needle!r}")


def forbid_contains(path: Path, *, needle: str, context: str) -> None:
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except FileNotFoundError:
        fail(f"missing required file: {path.as_posix()}")
    if needle in normalize(text):
        fail(f"{context}: forbidden reference in {path.as_posix()}: {needle!r}")


def main() -> None:
    if not CANONICAL_VQ_C.is_file():
        fail(f"missing canonical virtqueue implementation: {CANONICAL_VQ_C.relative_to(REPO_ROOT)}")
    if not CANONICAL_VQ_H.is_file():
        fail(f"missing canonical virtqueue header: {CANONICAL_VQ_H.relative_to(REPO_ROOT)}")
    if not LEGACY_VQ_C.is_file():
        fail(f"missing legacy virtqueue implementation: {LEGACY_VQ_C.relative_to(REPO_ROOT)}")

    # ---------------------------------------------------------------------
    # MSBuild projects: enforce the expected split-virtqueue engine per driver.
    # ---------------------------------------------------------------------
    msbuild_projects: dict[str, tuple[Path, str, str, str]] = {
        # Win7 miniports (StorPort/NDIS) use the WDF-free canonical split-ring engine.
        "virtio-blk": (
            REPO_ROOT / "drivers/windows7/virtio/blk/aerovblk.vcxproj",
            "virtqueue_split.c",
            "virtqueue_split_legacy.c",
            "windows/virtio/common",
        ),
        "virtio-net": (
            REPO_ROOT / "drivers/windows7/virtio/net/aerovnet.vcxproj",
            "virtqueue_split.c",
            "virtqueue_split_legacy.c",
            "windows/virtio/common",
        ),
        # virtio-input lives under drivers/windows but targets Win7 (KMDF 1.9) and
        # uses the WDF-free split virtqueue engine.
        "virtio-input": (
            REPO_ROOT / "drivers/windows/virtio-input/virtio-input.vcxproj",
            "virtqueue_split.c",
            "virtqueue_split_legacy.c",
            "virtio/common",
        ),
    }

    for name, (proj, expected_src, forbidden_src, include_hint) in msbuild_projects.items():
        compiled = parse_vcxproj_compiled_clcompile_includes(proj)

        expected_suffix = "/" + expected_src.lower()
        forbidden_suffix = "/" + forbidden_src.lower()

        has_expected = any(p.lower().endswith(expected_suffix) for p in compiled)
        if not has_expected:
            fail(
                f"{name}: MSBuild project does not compile {expected_src}\n"
                f"  project: {proj.relative_to(REPO_ROOT).as_posix()}\n"
                f"  compiled: {sorted(compiled)}"
            )

        has_forbidden = any(p.lower().endswith(forbidden_suffix) for p in compiled)
        if has_forbidden:
            fail(
                f"{name}: MSBuild project must not compile {forbidden_src}\n"
                f"  project: {proj.relative_to(REPO_ROOT).as_posix()}"
            )

        include_dirs = parse_vcxproj_additional_include_dirs(proj).lower()
        if include_hint.lower() not in include_dirs:
            fail(
                f"{name}: MSBuild project missing expected include dir hint ({include_hint})\n"
                f"  project: {proj.relative_to(REPO_ROOT).as_posix()}\n"
                f"  AdditionalIncludeDirectories: {include_dirs}"
            )

    # ---------------------------------------------------------------------
    # WDK 7.1 build.exe SOURCES files (deprecated but kept in-tree)
    # ---------------------------------------------------------------------
    sources_files: dict[str, tuple[Path, str, str, str]] = {
        "virtio-blk": (
            REPO_ROOT / "drivers/windows7/virtio/blk/sources",
            "virtqueue_split.c",
            "virtqueue_split_legacy.c",
            "windows/virtio/common",
        ),
        "virtio-net": (
            REPO_ROOT / "drivers/windows7/virtio/net/sources",
            "virtqueue_split.c",
            "virtqueue_split_legacy.c",
            "windows/virtio/common",
        ),
        "virtio-input": (
            REPO_ROOT / "drivers/windows/virtio-input/sources",
            "virtqueue_split.c",
            "virtqueue_split_legacy.c",
            "virtio/common",
        ),
        "virtio-snd": (
            REPO_ROOT / "drivers/windows7/virtio-snd/src/sources",
            "virtqueue_split.c",
            "virtqueue_split_legacy.c",
            "windows/virtio/common",
        ),
    }

    for name, (sources, expected_src, forbidden_src, include_hint) in sources_files.items():
        require_contains(sources, needle=expected_src, context=f"{name}: SOURCES")
        forbid_contains(sources, needle=forbidden_src, context=f"{name}: SOURCES")
        require_contains(sources, needle=include_hint, context=f"{name}: INCLUDES")

    # ---------------------------------------------------------------------
    # Host harnesses: ensure each split-ring implementation is still exercised.
    # ---------------------------------------------------------------------
    require_contains(
        REPO_ROOT / "drivers/windows/virtio/common/tests/CMakeLists.txt",
        needle="virtqueue_split.c",
        context="drivers/windows/virtio/common/tests (CMake)",
    )
    require_contains(
        REPO_ROOT / "drivers/windows/virtio/common/tests/Makefile",
        needle="virtqueue_split.c",
        context="drivers/windows/virtio/common/tests (Makefile)",
    )
    require_contains(
        REPO_ROOT / "drivers/windows7/virtio/common/tests/CMakeLists.txt",
        needle="virtqueue_split_legacy.c",
        context="drivers/windows7/virtio/common/tests (CMake)",
    )

    print("ok: Windows driver build files reference the expected split-virtqueue implementation")


if __name__ == "__main__":
    main()
