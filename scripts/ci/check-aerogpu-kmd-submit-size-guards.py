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


def _forbid(body: str, what: str, pattern: str) -> str | None:
    if re.search(pattern, body, re.S):
        return f"{what}: forbidden pattern found: {pattern}"
    return None


def _strip_c_comments(text: str) -> str:
    """
    Best-effort stripping for guardrail regexes.

    We want to ignore comment contents and string/char literals so that:
      - "count * sizeof" in comments/log strings doesn't trip the forbid checks.
      - '//' or '/*' inside strings doesn't cause us to accidentally delete real code.

    This is a minimal lexer (not a full C parser) but is sufficient for our static scan.
    """

    out: list[str] = []
    i = 0
    state = "code"

    while i < len(text):
        ch = text[i]
        nxt = text[i + 1] if i + 1 < len(text) else ""

        if state == "code":
            if ch == "/" and nxt == "/":
                state = "line_comment"
                out.append(" ")
                out.append(" ")
                i += 2
                continue
            if ch == "/" and nxt == "*":
                state = "block_comment"
                out.append(" ")
                out.append(" ")
                i += 2
                continue
            if ch == '"':
                state = "string"
                out.append(" ")
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
                state = "code"
                out.append("\n")
            else:
                out.append(" ")
            i += 1
            continue

        if state == "block_comment":
            if ch == "*" and nxt == "/":
                state = "code"
                out.append(" ")
                out.append(" ")
                i += 2
                continue
            if ch == "\n":
                out.append("\n")
            else:
                out.append(" ")
            i += 1
            continue

        if state == "string":
            if ch == "\\":
                # Skip escaped character.
                out.append(" ")
                if i + 1 < len(text):
                    out.append(" ")
                i += 2
                continue
            if ch == '"':
                state = "code"
                out.append(" ")
                i += 1
                continue
            if ch == "\n":
                # Malformed string; recover to code on newline.
                state = "code"
                out.append("\n")
                i += 1
                continue
            out.append(" ")
            i += 1
            continue

        if state == "char":
            if ch == "\\":
                out.append(" ")
                if i + 1 < len(text):
                    out.append(" ")
                i += 2
                continue
            if ch == "'":
                state = "code"
                out.append(" ")
                i += 1
                continue
            if ch == "\n":
                state = "code"
                out.append("\n")
                i += 1
                continue
            out.append(" ")
            i += 1
            continue

    return "".join(out)


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

    # --- AeroGpuSubmissionMetaTotalBytes (meta byte accounting) ---
    try:
        body = _extract_function(text, "AeroGpuSubmissionMetaTotalBytes")
        body_nocomments = _strip_c_comments(body)
        errors.extend(
            e
            for e in [
                _require(
                    body,
                    "AeroGpuSubmissionMetaTotalBytes allocBytes/metaBytes sizing",
                    r"RtlSizeTMult\s*\(\s*\(?\s*(?:\(\s*SIZE_T\s*\)\s*)?Meta->AllocationCount\s*\)?\s*,\s*sizeof\s*\(\s*aerogpu_legacy_submission_desc_allocation\s*\)\s*,\s*&(?P<alloc_var>[A-Za-z0-9_]+)\s*\)\s*;.*?"
                    r"RtlSizeTAdd\s*\(\s*FIELD_OFFSET\s*\(\s*AEROGPU_SUBMISSION_META\s*,\s*Allocations\s*\)\s*,\s*(?P=alloc_var)\s*,\s*&(?P<meta_var>[A-Za-z0-9_]+)\s*\)",
                ),
                _require(
                    body,
                    "AeroGpuSubmissionMetaTotalBytes overflow sentinel",
                    r"0xFFFFFFFFFFFFFFFFull",
                ),
                _forbid(
                    body_nocomments,
                    "AeroGpuSubmissionMetaTotalBytes unsafe multiplication",
                    r"Meta->AllocationCount\b\s*\)*\s*\*\s*\(?\s*sizeof\b",
                ),
            ]
            if e
        )
    except Exception as e:  # pragma: no cover
        errors.append(f"{SRC.relative_to(ROOT)}: {e}")

    # --- AeroGpuBuildAndAttachMeta ---
    try:
        body = _extract_function(text, "AeroGpuBuildAndAttachMeta")
        body_nocomments = _strip_c_comments(body)
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
                    "AeroGpuBuildAndAttachMeta checked sizing",
                    r"RtlSizeTMult\s*\(\s*\(?\s*(?:\(\s*SIZE_T\s*\)\s*)?AllocationCount\s*\)?\s*,\s*sizeof\s*\(\s*[^)]+\s*\)\s*,\s*&(?P<alloc_var>[A-Za-z0-9_]+)\s*\)\s*;.*?"
                    r"RtlSizeTAdd\s*\(\s*(?:FIELD_OFFSET|offsetof)\s*\(\s*AEROGPU_SUBMISSION_META\s*,\s*Allocations\s*\)\s*,\s*(?P=alloc_var)\s*,\s*&(?P<meta_var>[A-Za-z0-9_]+)\s*\)",
                ),
                _forbid(
                    body_nocomments,
                    "AeroGpuBuildAndAttachMeta unsafe multiplication",
                    r"AllocationCount\b\s*\)*\s*\*\s*\(?\s*sizeof\b",
                ),
            ]
            if e
        )
    except Exception as e:  # pragma: no cover (best-effort guard script)
        errors.append(f"{SRC.relative_to(ROOT)}: {e}")

    # --- AeroGpuDdiSubmitCommand (legacy descriptor sizing) ---
    try:
        body = _extract_function(text, "AeroGpuDdiSubmitCommand")
        body_nocomments = _strip_c_comments(body)
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
                    "AeroGpuDdiSubmitCommand checked sizing",
                    r"RtlSizeTMult\s*\(\s*\(?\s*(?:\(\s*SIZE_T\s*\)\s*)?allocCount\s*\)?\s*,\s*sizeof\s*\(\s*[^)]+\s*\)\s*,\s*&(?P<alloc_var>[A-Za-z0-9_]+)\s*\)\s*;.*?"
                    r"RtlSizeTAdd\s*\(\s*[^,]*\b(?:aerogpu_legacy_submission_desc_header|struct\s+aerogpu_legacy_submission_desc_header)\b[^,]*,\s*(?P=alloc_var)\s*,\s*&descSize\s*\)",
                ),
                _forbid(
                    body_nocomments,
                    "AeroGpuDdiSubmitCommand unsafe multiplication",
                    r"allocCount\b\s*\)*\s*\*\s*\(?\s*sizeof\b",
                ),
            ]
            if e
        )
    except Exception as e:  # pragma: no cover
        errors.append(f"{SRC.relative_to(ROOT)}: {e}")

    # --- AeroGpuBuildAllocTable (cap + table size math) ---
    try:
        body = _extract_function(text, "AeroGpuBuildAllocTable")
        body_nocomments = _strip_c_comments(body)
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
                    "AeroGpuBuildAllocTable checked sizing",
                    r"RtlSizeTMult\s*\(\s*\(?\s*(?:\(\s*SIZE_T\s*\)\s*)?entryCount\s*\)?\s*,\s*sizeof\s*\(\s*[^)]+\s*\)\s*,\s*&(?P<entries_var>[A-Za-z0-9_]+)\s*\)\s*;.*?"
                    r"RtlSizeTAdd\s*\(\s*[^,]*\b(?:struct\s+)?aerogpu_alloc_table_header\b[^,]*,\s*(?P=entries_var)\s*,\s*&(?P<table_var>[A-Za-z0-9_]+)\s*\)",
                ),
                _forbid(
                    body_nocomments,
                    "AeroGpuBuildAllocTable unsafe multiplication",
                    r"entryCount\b\s*\)*\s*\*\s*\(?\s*sizeof\b",
                ),
            ]
            if e
        )
    except Exception as e:  # pragma: no cover
        errors.append(f"{SRC.relative_to(ROOT)}: {e}")

    # --- Scratch allocation sizing (tmp/hash structures) ---
    try:
        body = _extract_function(text, "AeroGpuAllocTableScratchAllocBlock")
        body_nocomments = _strip_c_comments(body)
        errors.extend(
            e
            for e in [
                _require(
                    body,
                    "AeroGpuAllocTableScratchAllocBlock tmpBytes sizing",
                    r"RtlSizeTMult\s*\(\s*\(?\s*(?:\(\s*SIZE_T\s*\)\s*)?TmpEntriesCap\s*\)?\s*,\s*sizeof\s*\(\s*[^)]+\s*\)\s*,\s*&(?P<tmp_var>[A-Za-z0-9_]+)\s*\)\s*;.*?"
                    r"RtlSizeTAdd\s*\(\s*off\s*,\s*(?P=tmp_var)\s*,\s*&off\s*\)",
                ),
                _require(
                    body,
                    "AeroGpuAllocTableScratchAllocBlock hash/meta sizing",
                    r"RtlSizeTMult\s*\(\s*[^,]*\bHashCap\b[^,]*,\s*sizeof\s*\(\s*[^)]+\s*\)\s*,\s*&[A-Za-z0-9_]*Bytes\s*\)",
                ),
                _forbid(
                    body_nocomments,
                    "AeroGpuAllocTableScratchAllocBlock unsafe multiplication",
                    r"(?:TmpEntriesCap|HashCap)\b\s*\)*\s*\*\s*\(?\s*sizeof\b",
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
