#!/usr/bin/env python3
"""
Guardrail: enforce correct contiguous allocation/free API pairing in Win7 virtio-snd.

Why this exists:
  - The Win7 virtio codebase standardizes on allocating DMA-ish contiguous buffers
    with `MmAllocateContiguousMemorySpecifyCache(...)`.
  - Those allocations must be freed with the matching API:
      MmFreeContiguousMemorySpecifyCache(ptr, size, cacheType)
    (and not `MmFreeContiguousMemory(ptr)`).

Mixing `MmAllocateContiguousMemorySpecifyCache` with `MmFreeContiguousMemory` has
been flagged as a correctness/maintainability risk. This check is intentionally
lightweight and targets the known virtio-snd sources that previously regressed.
"""

from __future__ import annotations

import re
import sys
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[2]

FILES_TO_CHECK = [
    REPO_ROOT / "drivers/windows7/virtio-snd/src/aero_virtio_snd_ioport_hw.c",
    REPO_ROOT / "drivers/windows7/virtio-snd/src/virtiosnd_backend_virtio.c",
]

ALLOC_RE = re.compile(r"\bMmAllocateContiguousMemorySpecifyCache\s*\(")
FREE_RE = re.compile(r"\bMmFreeContiguousMemory\s*\(")


def strip_c_comments_and_strings(src: str) -> str:
    """
    Remove C/C++ comments and string/char literals.

    This reduces false positives from e.g. documentation comments that mention
    the APIs without calling them.
    """

    out: list[str] = []
    i = 0
    n = len(src)

    state = "code"  # code | line_comment | block_comment | string | char

    while i < n:
        ch = src[i]

        if state == "code":
            if ch == "/" and i + 1 < n and src[i + 1] == "/":
                state = "line_comment"
                i += 2
                continue
            if ch == "/" and i + 1 < n and src[i + 1] == "*":
                state = "block_comment"
                i += 2
                continue
            if ch == '"':
                state = "string"
                out.append(" ")  # keep spacing
                i += 1
                continue
            if ch == "'":
                state = "char"
                out.append(" ")
                i += 1
                continue

            out.append(ch)
            i += 1
            continue

        if state == "line_comment":
            if ch == "\n":
                out.append("\n")
                state = "code"
            i += 1
            continue

        if state == "block_comment":
            if ch == "*" and i + 1 < n and src[i + 1] == "/":
                state = "code"
                i += 2
                continue
            if ch == "\n":
                out.append("\n")
            else:
                out.append(" ")
            i += 1
            continue

        if state == "string":
            if ch == "\\" and i + 1 < n:
                # Skip escaped char.
                out.append(" ")
                out.append(" ")
                i += 2
                continue
            if ch == '"':
                state = "code"
                out.append(" ")
                i += 1
                continue
            out.append("\n" if ch == "\n" else " ")
            i += 1
            continue

        if state == "char":
            if ch == "\\" and i + 1 < n:
                out.append(" ")
                out.append(" ")
                i += 2
                continue
            if ch == "'":
                state = "code"
                out.append(" ")
                i += 1
                continue
            out.append("\n" if ch == "\n" else " ")
            i += 1
            continue

        raise AssertionError(f"unexpected state: {state}")

    return "".join(out)


def main() -> None:
    failures: list[str] = []

    for path in FILES_TO_CHECK:
        try:
            raw = path.read_text(encoding="utf-8", errors="replace")
        except FileNotFoundError:
            failures.append(f"missing expected file: {path.relative_to(REPO_ROOT).as_posix()}")
            continue

        stripped = strip_c_comments_and_strings(raw)

        has_alloc = ALLOC_RE.search(stripped) is not None
        has_free = FREE_RE.search(stripped) is not None

        if has_alloc and has_free:
            rel = path.relative_to(REPO_ROOT).as_posix()
            failures.append(
                f"{rel}: found MmFreeContiguousMemory(...) but this file also calls "
                "MmAllocateContiguousMemorySpecifyCache(...).\n"
                "  Expected API pairing: MmAllocateContiguousMemorySpecifyCache <-> "
                "MmFreeContiguousMemorySpecifyCache(ptr, size, cacheType)."
            )

    if failures:
        for msg in failures:
            print(f"error: {msg}", file=sys.stderr)
        raise SystemExit(1)


if __name__ == "__main__":
    main()

