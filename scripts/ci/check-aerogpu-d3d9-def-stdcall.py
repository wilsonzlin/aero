#!/usr/bin/env python3
"""
CI guardrail: validate AeroGPU D3D9 UMD `.def` export decoration (x86 stdcall).

Why this exists
---------------
The Win7 D3D9 runtime loads the UMD purely by ABI contract. On x86, that means
the exported entrypoint names must match MSVC's stdcall decoration:

  _OpenAdapter@N

Where `N` is the number of stack bytes pushed for the call. If `N` drifts, the
runtime may resolve the wrong symbol or corrupt the stack before the driver ever
gets a chance to log anything.

This script verifies that:
  - `drivers/aerogpu/umd/d3d9/aerogpu_d3d9_x86.def` maps undecorated exports to
    the expected decorated names, and
  - the decorated names are also exported directly (robustness), and
  - `drivers/aerogpu/umd/d3d9/aerogpu_d3d9_x64.def` contains the required
    undecorated exports (sanity).

Expected stack byte counts are sourced from:
  drivers/aerogpu/umd/d3d9/src/aerogpu_d3d9_wdk_abi_expected.h
"""

from __future__ import annotations

import pathlib
import re
import sys


_EXPECTED_RE = re.compile(
    r"^\s*#define\s+"
    r"AEROGPU_D3D9_WDK_ABI_EXPECT_"
    r"(?P<which>OPENADAPTER2?|OPENADAPTERFROMHDC|OPENADAPTERFROMLUID)"
    r"_STDCALL_BYTES\s+"
    r"(?P<bytes>\d+)\s*$",
    re.MULTILINE,
)

_DEF_MAPPING_RE = re.compile(r"^(?P<name>[A-Za-z0-9_]+)\s*=\s*(?P<target>[A-Za-z0-9_@]+)\s*$")
_DECORATED_RE = re.compile(r"^_(?P<base>OpenAdapter2?|OpenAdapterFromHdc|OpenAdapterFromLuid)@(?P<bytes>\d+)$", re.IGNORECASE)


def _parse_expected(text: str) -> dict[str, int]:
    out: dict[str, int] = {}
    for m in _EXPECTED_RE.finditer(text):
        out[m.group("which")] = int(m.group("bytes"))
    return out


def _parse_def_exports(text: str) -> list[str]:
    exports: list[str] = []
    for raw in text.splitlines():
        # Strip comments (semicolon is the .def comment delimiter).
        line = raw.split(";", 1)[0].strip()
        if not line:
            continue
        upper = line.upper()
        if upper.startswith("LIBRARY") or upper == "EXPORTS":
            continue
        exports.append(line)
    return exports


def main() -> int:
    repo_root = pathlib.Path(__file__).resolve().parents[2]
    expected_path = repo_root / "drivers/aerogpu/umd/d3d9/src/aerogpu_d3d9_wdk_abi_expected.h"
    def_x86_path = repo_root / "drivers/aerogpu/umd/d3d9/aerogpu_d3d9_x86.def"
    def_x64_path = repo_root / "drivers/aerogpu/umd/d3d9/aerogpu_d3d9_x64.def"

    expected_text = expected_path.read_text(encoding="utf-8", errors="replace")
    expected = _parse_expected(expected_text)

    required = ["OPENADAPTER", "OPENADAPTER2", "OPENADAPTERFROMHDC", "OPENADAPTERFROMLUID"]
    missing = [k for k in required if k not in expected]
    if missing:
        print(f"error: missing expected stdcall macros in {expected_path}: {', '.join(missing)}", file=sys.stderr)
        return 1

    def_x86_text = def_x86_path.read_text(encoding="utf-8", errors="replace")
    exports_x86 = _parse_def_exports(def_x86_text)

    # Gather mappings and raw decorated exports.
    mappings: dict[str, str] = {}
    raw_decorated: set[str] = set()
    for entry in exports_x86:
        m = _DEF_MAPPING_RE.match(entry)
        if m:
            mappings[m.group("name")] = m.group("target")
        else:
            raw_decorated.add(entry)

    expected_entries = {
        "OpenAdapter": ("OpenAdapter", expected["OPENADAPTER"]),
        "OpenAdapter2": ("OpenAdapter2", expected["OPENADAPTER2"]),
        "OpenAdapterFromHdc": ("OpenAdapterFromHdc", expected["OPENADAPTERFROMHDC"]),
        "OpenAdapterFromLuid": ("OpenAdapterFromLuid", expected["OPENADAPTERFROMLUID"]),
    }

    ok = True
    for export_name, (base, expected_bytes) in expected_entries.items():
        target = mappings.get(export_name)
        if target is None:
            print(f"error: {def_x86_path} missing export mapping: {export_name}=_{base}@N", file=sys.stderr)
            ok = False
            continue

        m = _DECORATED_RE.match(target)
        if not m or m.group("base").lower() != base.lower():
            print(f"error: {def_x86_path} {export_name} maps to unexpected symbol: {target!r}", file=sys.stderr)
            ok = False
            continue

        actual_bytes = int(m.group("bytes"))
        if actual_bytes != expected_bytes:
            print(
                f"error: {def_x86_path} {export_name} exports {target} but expected @{expected_bytes} bytes (got @{actual_bytes})",
                file=sys.stderr,
            )
            ok = False

        if target not in raw_decorated:
            print(f"error: {def_x86_path} missing raw decorated export entry: {target}", file=sys.stderr)
            ok = False

    # Optional sanity check: x64 exports should exist (no decoration on Win64).
    def_x64_text = def_x64_path.read_text(encoding="utf-8", errors="replace")
    exports_x64 = set(_parse_def_exports(def_x64_text))
    for export_name in expected_entries.keys():
        if export_name not in exports_x64:
            print(f"error: {def_x64_path} missing export: {export_name}", file=sys.stderr)
            ok = False

    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())

