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

  2) Portable split-ring engine (used by Win7 miniports + host tests):
      - drivers/windows7/virtio/common/src/virtqueue_split_legacy.c
      - drivers/windows7/virtio/common/include/virtqueue_split_legacy.h
      - API surface: `virtqueue_split_*` + `virtio_os_ops_t`

In-tree driver wiring:
  - virtio-blk / virtio-net: legacy portable engine (pairs with the Win7
    miniport virtio-pci modern transport in drivers/windows7/virtio/common).
  - virtio-input / virtio-snd: canonical engine.
  - host tests:
    - drivers/windows/virtio/common/tests => virtqueue_split.c
    - drivers/windows7/virtio/common/tests => virtqueue_split_legacy.c

This script encodes that intended wiring and fails on drift.
"""

from __future__ import annotations

import subprocess
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


def normalize_lower(value: str) -> str:
    return normalize(value).lower()


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
    if needle.lower() not in normalize_lower(text):
        fail(f"{context}: expected {path.as_posix()} to contain: {needle!r}")


def forbid_contains(path: Path, *, needle: str, context: str) -> None:
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except FileNotFoundError:
        fail(f"missing required file: {path.as_posix()}")
    if needle.lower() in normalize_lower(text):
        fail(f"{context}: forbidden reference in {path.as_posix()}: {needle!r}")


def check_include_hints(*, context: str, include_text: str, required: tuple[str, ...], forbidden: tuple[str, ...]) -> None:
    include_lower = normalize_lower(include_text)

    missing = [hint for hint in required if hint.lower() not in include_lower]
    present_forbidden = [hint for hint in forbidden if hint.lower() in include_lower]

    if missing or present_forbidden:
        details: list[str] = []
        if missing:
            details.append("missing: " + ", ".join(repr(h) for h in missing))
        if present_forbidden:
            details.append("forbidden: " + ", ".join(repr(h) for h in present_forbidden))
        fail(f"{context}: include directory drift ({'; '.join(details)})\n  includes: {include_text}")


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
    msbuild_projects: dict[str, dict[str, object]] = {
        # Win7 miniports (StorPort/NDIS) compile against the Win7 common stack and the
        # legacy portable virtqueue engine.
        "virtio-blk": {
            "proj": REPO_ROOT / "drivers/windows7/virtio-blk/aero_virtio_blk.vcxproj",
            "expected_src": "virtqueue_split_legacy.c",
            "forbidden_src": "virtqueue_split.c",
            "required_includes": (
                "virtio/common/include",
                "virtio/common/os_shim",
                "win7/virtio/virtio-core/portable",
            ),
            "forbidden_includes": ("windows/virtio/common",),
        },
        "virtio-net": {
            "proj": REPO_ROOT / "drivers/windows7/virtio-net/aero_virtio_net.vcxproj",
            "expected_src": "virtqueue_split_legacy.c",
            "forbidden_src": "virtqueue_split.c",
            "required_includes": (
                "virtio/common/include",
                "virtio/common/os_shim",
                "win7/virtio/virtio-core/portable",
            ),
            "forbidden_includes": ("windows/virtio/common",),
        },
        # virtio-input targets Win7 (KMDF 1.9) and uses the canonical split virtqueue engine.
        "virtio-input": {
            "proj": REPO_ROOT / "drivers/windows7/virtio-input/aero_virtio_input.vcxproj",
            "expected_src": "virtqueue_split.c",
            "forbidden_src": "virtqueue_split_legacy.c",
            "required_includes": ("windows/virtio/common",),
            "forbidden_includes": (),
        },
        "virtio-snd": {
            "proj": REPO_ROOT / "drivers/windows7/virtio-snd/aero_virtio_snd.vcxproj",
            "expected_src": "virtqueue_split.c",
            "forbidden_src": "virtqueue_split_legacy.c",
            "required_includes": ("windows/virtio/common",),
            "forbidden_includes": (),
        },
    }

    for name, cfg in msbuild_projects.items():
        proj = cfg["proj"]
        expected_src = cfg["expected_src"]
        forbidden_src = cfg["forbidden_src"]
        required_includes = cfg["required_includes"]
        forbidden_includes = cfg["forbidden_includes"]

        compiled = parse_vcxproj_compiled_clcompile_includes(proj)

        expected_suffix = "/" + str(expected_src).lower()
        forbidden_suffix = "/" + str(forbidden_src).lower()

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

        include_dirs = parse_vcxproj_additional_include_dirs(proj)
        check_include_hints(
            context=f"{name}: MSBuild project include dirs",
            include_text=include_dirs,
            required=tuple(str(x) for x in required_includes),  # type: ignore[arg-type]
            forbidden=tuple(str(x) for x in forbidden_includes),  # type: ignore[arg-type]
        )

    # ---------------------------------------------------------------------
    # WinDDK 7600 build.exe SOURCES files (deprecated but kept in-tree)
    # ---------------------------------------------------------------------
    sources_files: dict[str, dict[str, object]] = {
        "virtio-blk": {
            "sources": REPO_ROOT / "drivers/windows7/virtio-blk/sources",
            "expected_src": "virtqueue_split_legacy.c",
            "forbidden_src": "virtqueue_split.c",
            "required_includes": (
                "virtio/common/include",
                "virtio/common/os_shim",
                "win7/virtio/virtio-core/portable",
            ),
            "forbidden_includes": ("windows/virtio/common",),
        },
        "virtio-net": {
            "sources": REPO_ROOT / "drivers/windows7/virtio-net/sources",
            "expected_src": "virtqueue_split_legacy.c",
            "forbidden_src": "virtqueue_split.c",
            "required_includes": (
                "virtio/common/include",
                "virtio/common/os_shim",
                "win7/virtio/virtio-core/portable",
            ),
            "forbidden_includes": ("windows/virtio/common",),
        },
        "virtio-input": {
            "sources": REPO_ROOT / "drivers/windows7/virtio-input/sources",
            "expected_src": "virtqueue_split.c",
            "forbidden_src": "virtqueue_split_legacy.c",
            "required_includes": ("windows/virtio/common",),
            "forbidden_includes": (),
        },
        "virtio-snd": {
            "sources": REPO_ROOT / "drivers/windows7/virtio-snd/src/sources",
            "expected_src": "virtqueue_split.c",
            "forbidden_src": "virtqueue_split_legacy.c",
            "required_includes": ("windows/virtio/common",),
            "forbidden_includes": (),
        },
    }

    for name, cfg in sources_files.items():
        sources = cfg["sources"]
        expected_src = cfg["expected_src"]
        forbidden_src = cfg["forbidden_src"]
        required_includes = cfg["required_includes"]
        forbidden_includes = cfg["forbidden_includes"]

        require_contains(sources, needle=str(expected_src), context=f"{name}: SOURCES")
        forbid_contains(sources, needle=str(forbidden_src), context=f"{name}: SOURCES")

        include_context = f"{name}: INCLUDES"
        for hint in tuple(str(x) for x in required_includes):  # type: ignore[arg-type]
            require_contains(sources, needle=hint, context=include_context)
        for hint in tuple(str(x) for x in forbidden_includes):  # type: ignore[arg-type]
            forbid_contains(sources, needle=hint, context=include_context)

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

    virtio_net_pci_cfg_check = REPO_ROOT / "scripts/ci/check-win7-virtio-net-pci-config-access.py"
    proc = subprocess.run([sys.executable, str(virtio_net_pci_cfg_check)])
    if proc.returncode != 0:
        raise SystemExit(proc.returncode)

    print("ok: Windows driver build files reference the expected split-virtqueue implementation")


if __name__ == "__main__":
    main()
