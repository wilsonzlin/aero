#!/usr/bin/env python3
"""
CI guardrail: AeroGPU Win7 KMD submit/allocation-list size hardening.

The AeroGPU Win7 KMD processes allocation lists provided by dxgkrnl (and, indirectly,
user-mode runtimes). If AllocationListSize / AllocationCount are unexpectedly large,
naive `count * sizeof(...)` arithmetic can overflow and lead to undersized allocations
and out-of-bounds writes.

The driver relies on two layers of defense in the hot submission paths:
  1. A hard cap on the number of allocation-list entries we will process.
  2. Overflow-checked size computations using RtlSizeTMult / RtlSizeTAdd.

This script scans `drivers/aerogpu/kmd/src/aerogpu_kmd.c` for key invariants in:
  - AeroGpuBuildAndAttachMeta (metaSize sizing)
  - AeroGpuDdiSubmitCommand (legacy descSize sizing)
  - AeroGpuBuildAllocTable + AeroGpuAllocTableScratchAllocBlock (scratch/table sizing)

It is intentionally lightweight and Linux-friendly; it does not require a WDK or a
Windows build environment.
"""

from __future__ import annotations

import pathlib
import re
import sys


def repo_root() -> pathlib.Path:
    # scripts/ci/<this file> -> scripts/ci -> scripts -> repo root
    return pathlib.Path(__file__).resolve().parents[2]


ROOT = repo_root()
SRC = ROOT / "drivers" / "aerogpu" / "kmd" / "src" / "aerogpu_kmd.c"


def _extract_function(text: str, name: str) -> str:
    """
    Extract a C function body (including outer braces) using a minimal lexer that
    ignores braces inside comments and string/char literals.
    """

    # Anchor on the *definition* line (not call sites). These are internal helpers,
    # so `static` is a stable discriminator and avoids false matches.
    m = re.search(rf"(?m)^\s*static\b[^\n]*\b{re.escape(name)}\s*\(", text)
    if not m:
        raise ValueError(f"function definition not found: {name}")

    brace_start = text.find("{", m.end())
    if brace_start < 0:
        raise ValueError(f"function opening brace not found: {name}")

    depth = 0
    i = brace_start
    state = "code"
    while i < len(text):
        ch = text[i]
        nxt = text[i + 1] if i + 1 < len(text) else ""

        if state == "code":
            if ch == "/" and nxt == "/":
                state = "line_comment"
                i += 2
                continue
            if ch == "/" and nxt == "*":
                state = "block_comment"
                i += 2
                continue
            if ch == '"':
                state = "string"
                i += 1
                continue
            if ch == "'":
                state = "char"
                i += 1
                continue
            if ch == "{":
                depth += 1
            elif ch == "}":
                depth -= 1
                if depth == 0:
                    return text[brace_start : i + 1]
            i += 1
            continue

        if state == "line_comment":
            if ch == "\n":
                state = "code"
            i += 1
            continue

        if state == "block_comment":
            if ch == "*" and nxt == "/":
                state = "code"
                i += 2
                continue
            i += 1
            continue

        if state == "string":
            if ch == "\\":
                # Skip escaped character.
                i += 2
                continue
            if ch == '"':
                state = "code"
            i += 1
            continue

        if state == "char":
            if ch == "\\":
                i += 2
                continue
            if ch == "'":
                state = "code"
            i += 1
            continue

    raise ValueError(f"function body not terminated: {name}")


def _require(body: str, what: str, pattern: str) -> str | None:
    if not re.search(pattern, body, re.S):
        return f"{what}: missing pattern: {pattern}"
    return None


def main() -> int:
    if not SRC.is_file():
        print(f"skip: {SRC.relative_to(ROOT)} not found", file=sys.stderr)
        return 0

    text = SRC.read_text(encoding="utf-8", errors="replace")
    errors: list[str] = []

    if not re.search(r"(?m)^\s*#\s*define\s+AEROGPU_KMD_SUBMIT_ALLOCATION_LIST_MAX_COUNT\b", text):
        errors.append(
            f"{SRC.relative_to(ROOT)}: missing AEROGPU_KMD_SUBMIT_ALLOCATION_LIST_MAX_COUNT definition"
        )

    # --- AeroGpuBuildAndAttachMeta ---
    try:
        body = _extract_function(text, "AeroGpuBuildAndAttachMeta")
        errors.extend(
            e
            for e in [
                _require(
                    body,
                    "AeroGpuBuildAndAttachMeta cap check",
                    r"AllocationCount\s*>\s*AEROGPU_KMD_SUBMIT_ALLOCATION_LIST_MAX_COUNT",
                ),
                _require(
                    body,
                    "AeroGpuBuildAndAttachMeta allocBytes sizing",
                    r"RtlSizeTMult\s*\(\s*[^,]*\bAllocationCount\b[^,]*,\s*sizeof\s*\(\s*aerogpu_legacy_submission_desc_allocation\s*\)\s*,\s*&allocBytes\s*\)",
                ),
                _require(
                    body,
                    "AeroGpuBuildAndAttachMeta metaSize sizing",
                    r"RtlSizeTAdd\s*\(\s*FIELD_OFFSET\s*\(\s*AEROGPU_SUBMISSION_META\s*,\s*Allocations\s*\)\s*,\s*allocBytes\s*,\s*&metaSize\s*\)",
                ),
            ]
            if e
        )
    except Exception as e:  # pragma: no cover (best-effort guard script)
        errors.append(f"{SRC.relative_to(ROOT)}: {e}")

    # --- AeroGpuDdiSubmitCommand (legacy descriptor sizing) ---
    try:
        body = _extract_function(text, "AeroGpuDdiSubmitCommand")
        errors.extend(
            e
            for e in [
                _require(
                    body,
                    "AeroGpuDdiSubmitCommand cap check",
                    r"allocCount\s*>\s*AEROGPU_KMD_SUBMIT_ALLOCATION_LIST_MAX_COUNT",
                ),
                _require(
                    body,
                    "AeroGpuDdiSubmitCommand allocBytes sizing",
                    r"RtlSizeTMult\s*\(\s*[^,]*\ballocCount\b[^,]*,\s*sizeof\s*\(\s*aerogpu_legacy_submission_desc_allocation\s*\)\s*,\s*&allocBytes\s*\)",
                ),
                _require(
                    body,
                    "AeroGpuDdiSubmitCommand descSize sizing",
                    r"RtlSizeTAdd\s*\(\s*[^,]*\baerogpu_legacy_submission_desc_header\b[^,]*,\s*allocBytes\s*,\s*&descSize\s*\)",
                ),
            ]
            if e
        )
    except Exception as e:  # pragma: no cover
        errors.append(f"{SRC.relative_to(ROOT)}: {e}")

    # --- AeroGpuBuildAllocTable (cap + table size math) ---
    try:
        body = _extract_function(text, "AeroGpuBuildAllocTable")
        errors.extend(
            e
            for e in [
                _require(
                    body,
                    "AeroGpuBuildAllocTable cap check",
                    r"Count\s*>\s*AEROGPU_KMD_SUBMIT_ALLOCATION_LIST_MAX_COUNT",
                ),
                _require(
                    body,
                    "AeroGpuBuildAllocTable entriesBytes sizing",
                    r"RtlSizeTMult\s*\(\s*[^,]*\bentryCount\b[^,]*,\s*sizeof\s*\(\s*struct\s+aerogpu_alloc_entry\s*\)\s*,\s*&entriesBytes\s*\)",
                ),
                _require(
                    body,
                    "AeroGpuBuildAllocTable tableSizeBytes sizing",
                    r"RtlSizeTAdd\s*\(\s*[^,]*\bstruct\s+aerogpu_alloc_table_header\b[^,]*,\s*entriesBytes\s*,\s*&tableSizeBytes\s*\)",
                ),
            ]
            if e
        )
    except Exception as e:  # pragma: no cover
        errors.append(f"{SRC.relative_to(ROOT)}: {e}")

    # --- Scratch allocation sizing (tmp/hash structures) ---
    try:
        body = _extract_function(text, "AeroGpuAllocTableScratchAllocBlock")
        errors.extend(
            e
            for e in [
                _require(
                    body,
                    "AeroGpuAllocTableScratchAllocBlock tmpBytes sizing",
                    r"RtlSizeTMult\s*\(\s*[^,]*\bTmpEntriesCap\b[^,]*,\s*sizeof\s*\(\s*struct\s+aerogpu_alloc_entry\s*\)\s*,\s*&tmpBytes\s*\)",
                ),
                _require(
                    body,
                    "AeroGpuAllocTableScratchAllocBlock hash/meta sizing",
                    r"RtlSizeTMult\s*\(\s*[^,]*\bHashCap\b[^,]*,\s*sizeof\s*\(\s*[^)]+\s*\)\s*,\s*&[A-Za-z0-9_]*Bytes\s*\)",
                ),
                _require(
                    body,
                    "AeroGpuAllocTableScratchAllocBlock offset accumulation",
                    r"RtlSizeTAdd\s*\(\s*off\s*,\s*tmpBytes\s*,\s*&off\s*\)",
                ),
            ]
            if e
        )
    except Exception as e:  # pragma: no cover
        errors.append(f"{SRC.relative_to(ROOT)}: {e}")

    if errors:
        print("ERROR: AeroGPU KMD submit size hardening regression detected:", file=sys.stderr)
        for err in errors:
            print(f"  - {err}", file=sys.stderr)
        return 1

    print("OK: AeroGPU KMD submit size hardening checks passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
