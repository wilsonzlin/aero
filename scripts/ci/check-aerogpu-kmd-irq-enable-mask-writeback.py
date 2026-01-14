#!/usr/bin/env python3
"""
CI guardrail: AeroGPU Win7 KMD IRQ_ENABLE mask writeback semantics.

Background
----------
The AeroGPU Win7 KMD updates `adapter->IrqEnableMask` (cached IRQ enable mask) from
DIRQL contexts (ISR) using interlocked operations. A common pitfall is that the
Windows `InterlockedAnd`/`InterlockedOr` APIs return the *previous* value of the
target, not the post-update value.

When we mask off `AEROGPU_IRQ_ERROR` delivery (to avoid interrupt storms on a
sticky/level-triggered ERROR bit), we must ensure the value programmed into the
device's `AEROGPU_MMIO_REG_IRQ_ENABLE` register reflects the *new* mask with the
ERROR bit cleared.

This script is intentionally lightweight and Linux-friendly; it does not require
a WDK or Windows build environment.
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


def _strip_c_comments_and_literals(text: str) -> str:
    """
    Best-effort removal of C/C++ comments and string/char literal bodies.

    We want guardrail regexes to match real code, not doc comments or log strings.
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
                out.append(" ")
                i += 2
                continue
            if ch == '"':
                state = "code"
            out.append(" ")
            i += 1
            continue

        if state == "char":
            if ch == "\\":
                out.append(" ")
                out.append(" ")
                i += 2
                continue
            if ch == "'":
                state = "code"
            out.append(" ")
            i += 1
            continue

    return "".join(out)


def main() -> int:
    if not SRC.exists():
        print(f"ERROR: expected source file not found: {SRC}", file=sys.stderr)
        return 2

    text = SRC.read_text(encoding="utf-8")
    body = _strip_c_comments_and_literals(text)

    errors: list[str] = []

    # Forbidden: programming IRQ_ENABLE using the *return value* of InterlockedAnd
    # directly. InterlockedAnd returns the old value, so this can accidentally
    # keep ERROR delivery enabled.
    if re.search(
        r"AeroGpuWriteRegU32\s*\(\s*(?:adapter|Adapter)\s*,\s*AEROGPU_MMIO_REG_IRQ_ENABLE\s*,[^;]*\bInterlockedAnd\s*\(",
        body,
        re.S,
    ):
        errors.append(
            "IRQ_ENABLE write uses InterlockedAnd(...) directly; InterlockedAnd returns the old value."
        )

    # Also forbid: assign InterlockedAnd(old) result into a variable and then use
    # that variable to program IRQ_ENABLE.
    #
    # NOTE: Be permissive about cast/paren style:
    # - The KMD commonly spells the pointer cast as `(volatile LONG*)&adapter->IrqEnableMask`
    #   (rather than `(volatile LONG*)&(adapter->IrqEnableMask)`).
    # - Future edits may write the result cast as either `(ULONG)InterlockedAnd(...)` or
    #   `(ULONG)(InterlockedAnd(...))`.
    #
    # We don't attempt to parse C here; this is a best-effort regex guardrail.
    assign_re = re.compile(
        r"\b([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(?:\([^)]*\)\s*)?(?:\(\s*)*InterlockedAnd\s*\(\s*"
        r"[^,;]*\b(?:adapter|Adapter)->IrqEnableMask\b[^,;]*,",
        re.S,
    )
    vars_old = set(assign_re.findall(body))
    for var in sorted(vars_old):
        if re.search(
            rf"\bAeroGpuWriteRegU32\s*\(\s*(?:adapter|Adapter)\s*,\s*AEROGPU_MMIO_REG_IRQ_ENABLE\s*,\s*"
            rf"(?:\(\s*[^()]*\)\s*)*\(?\s*{re.escape(var)}\s*\)?\s*\)",
            body,
            re.S,
        ):
            errors.append(
                f"IRQ_ENABLE write uses '{var}', which is the old value returned by InterlockedAnd(&IrqEnableMask, ...)."
            )

    if errors:
        print("ERROR: AeroGPU KMD IRQ_ENABLE writeback guard failed:")
        for e in errors:
            print(f"- {e}")
        return 1

    print("OK: AeroGPU KMD IRQ_ENABLE writeback guard passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
