#!/usr/bin/env python3
"""
Guardrail: ensure Win7-era driver sources don't mix contiguous-memory alloc/free APIs.

`MmAllocateContiguousMemorySpecifyCache` MUST be freed with
`MmFreeContiguousMemorySpecifyCache` (with the same cache type + size).

This is a lightweight, Linux-friendly static scan intended to catch regressions without
requiring a WDK/MSBuild environment.

Heuristic:
  Fail if a file contains:
    - MmAllocateContiguousMemorySpecifyCache
    - MmFreeContiguousMemory(
  but does NOT contain:
    - MmFreeContiguousMemorySpecifyCache

This is intentionally conservative/targeted; it is meant to catch the common regression
pattern where an implementation is updated to use SpecifyCache alloc but the free call is
not updated.
"""

from __future__ import annotations

import re
from pathlib import Path


def _has_plain_free(text: str) -> bool:
    # Match MmFreeContiguousMemory( but NOT MmFreeContiguousMemorySpecifyCache(
    return re.search(r"\bMmFreeContiguousMemory\s*\(", text) is not None


def main() -> int:
    roots = [
        Path("drivers/windows7"),
        # Win7 AeroGPU KMD sources (built/shipped).
        Path("drivers/aerogpu/kmd/src"),
    ]

    c_files: list[Path] = []
    for root in roots:
        if not root.exists():
            print(f"skip: {root} not found")
            continue
        c_files.extend(sorted(root.rglob("*.c")))

    if not c_files:
        joined = ", ".join(str(r) for r in roots)
        print(f"skip: no .c files under any of: {joined}")
        return 0

    offenders: list[Path] = []

    for path in c_files:
        text = path.read_text(encoding="utf-8", errors="replace")

        if "MmAllocateContiguousMemorySpecifyCache" not in text:
            continue

        if not _has_plain_free(text):
            continue

        if "MmFreeContiguousMemorySpecifyCache" in text:
            continue

        offenders.append(path)

    if offenders:
        print("error: possible contiguous-memory alloc/free mismatch detected:")
        for path in offenders:
            print(f"  - {path}")
        print(
            "\nFix: replace MmFreeContiguousMemory(...) with MmFreeContiguousMemorySpecifyCache(..., size, cache_type)."
        )
        return 1

    print("ok: no MmAllocateContiguousMemorySpecifyCache + MmFreeContiguousMemory mismatches found")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
