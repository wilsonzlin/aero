#!/usr/bin/env python3
"""
CI guardrail: AeroGPU Win7 KMD fence state must be accessed atomically.

Why:
  - The AeroGPU Win7 kernel-mode driver is built for both x86 and x64.
  - On x86, plain 64-bit loads/stores are not atomic and can tear.
  - Fence bookkeeping fields (LastSubmittedFence/LastCompletedFence/etc) are
    accessed across multiple contexts (submit thread, ISR/DPC, dbgctl escapes).

This script scans `drivers/aerogpu/kmd/src/aerogpu_kmd.c` and enforces that
member accesses to the fence fields are only performed via the local atomic
helper family (AeroGpuAtomic*U64).

It is intentionally lightweight and Linux-friendly; it does not require a WDK.
"""

from __future__ import annotations

from dataclasses import dataclass
import pathlib
import sys


def repo_root() -> pathlib.Path:
    # scripts/ci/<this file> -> scripts/ci -> scripts -> repo root
    return pathlib.Path(__file__).resolve().parents[2]


ROOT = repo_root()
SRC = ROOT / "drivers" / "aerogpu" / "kmd" / "src" / "aerogpu_kmd.c"


FENCE_FIELDS = {
    "LastSubmittedFence",
    "LastCompletedFence",
    "LastErrorFence",
    "LastNotifiedErrorFence",
}

ALLOWED_CALLS = {
    "AeroGpuAtomicReadU64",
    "AeroGpuAtomicWriteU64",
    "AeroGpuAtomicExchangeU64",
    "AeroGpuAtomicCompareExchangeU64",
}


@dataclass(frozen=True)
class Token:
    value: str
    kind: str  # "ident" | "number" | "punct"
    line: int  # 1-indexed
    col: int  # 1-indexed


def _strip_c_comments_and_literals(text: str) -> str:
    """
    Replace C/C++ comments and string/char literals with spaces (preserving
    newlines) so token locations remain stable for diagnostics.
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
                # Escape sequence - consume next char too.
                out.append(" ")
                if i + 1 < len(text):
                    out.append(" ")
                    i += 2
                else:
                    i += 1
                continue
            if ch == '"':
                state = "code"
                out.append(" ")
                i += 1
                continue
            if ch == "\n":
                # Unterminated string; reset defensively.
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
                else:
                    i += 1
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

        raise AssertionError(f"unhandled lexer state: {state}")

    return "".join(out)


def _lex(text: str) -> list[Token]:
    tokens: list[Token] = []
    i = 0
    line = 1
    col = 1

    def advance(n: int) -> None:
        nonlocal i, line, col
        for _ in range(n):
            if i >= len(text):
                return
            if text[i] == "\n":
                line += 1
                col = 1
            else:
                col += 1
            i += 1

    while i < len(text):
        ch = text[i]

        if ch.isspace():
            advance(1)
            continue

        tok_line = line
        tok_col = col

        # Identifiers
        if ch.isalpha() or ch == "_":
            start = i
            while i < len(text) and (text[i].isalnum() or text[i] == "_"):
                advance(1)
            tokens.append(Token(text[start:i], "ident", tok_line, tok_col))
            continue

        # Numbers (very loose; good enough for guarding syntax)
        if ch.isdigit():
            start = i
            while i < len(text) and (text[i].isalnum() or text[i] in "._"):
                advance(1)
            tokens.append(Token(text[start:i], "number", tok_line, tok_col))
            continue

        # Multi-char operators we care about
        if text.startswith("->", i):
            advance(2)
            tokens.append(Token("->", "punct", tok_line, tok_col))
            continue

        # Common 2-char punct; treat as a single token so the paren stack is stable.
        for op in ("==", "!=", "<=", ">=", "&&", "||", "++", "--", "<<", ">>"):
            if text.startswith(op, i):
                advance(len(op))
                tokens.append(Token(op, "punct", tok_line, tok_col))
                break
        else:
            # Single char punct
            advance(1)
            tokens.append(Token(ch, "punct", tok_line, tok_col))

    return tokens


def _nearest_enclosing_call(paren_stack: list[str | None]) -> str | None:
    for name in reversed(paren_stack):
        if name is not None:
            return name
    return None


def main() -> int:
    if not SRC.exists():
        print(f"OK: {SRC} not present; skipping.")
        return 0

    src_text = SRC.read_text(encoding="utf-8", errors="replace")
    stripped = _strip_c_comments_and_literals(src_text)
    tokens = _lex(stripped)

    # Track a lightweight parenthesis stack. For each '(' we record the nearest
    # identifier token to its left (which is typically the call name, but also
    # covers keywords like `if`/`for` so the "nearest enclosing call" heuristic
    # remains meaningful).
    paren_stack: list[str | None] = []

    violations: list[tuple[str, int, int, str | None]] = []

    prev: Token | None = None
    for tok in tokens:
        if tok.value == "(":
            name: str | None = None
            if prev is not None and prev.kind == "ident":
                name = prev.value
            paren_stack.append(name)
        elif tok.value == ")":
            if paren_stack:
                paren_stack.pop()
        else:
            # Fence field member access?
            if tok.kind == "ident" and tok.value in FENCE_FIELDS:
                if prev is not None and prev.value in ("->", "."):
                    enclosing = _nearest_enclosing_call(paren_stack)
                    if enclosing not in ALLOWED_CALLS:
                        violations.append((tok.value, tok.line, tok.col, enclosing))

        prev = tok

    if not violations:
        print("OK: AeroGPU KMD fence atomic access checks passed.")
        return 0

    lines = src_text.splitlines()
    print("ERROR: Found non-atomic fence field access in aerogpu_kmd.c:\n")
    for field, line, col, call in violations:
        context = lines[line - 1] if 1 <= line <= len(lines) else ""
        print(f"- {field} at {SRC}:{line}:{col} (enclosing call: {call!r})")
        if context:
            print(f"    {context}")
            print(f"    {' ' * (col - 1)}^")
        print("")

    print("Fence state must be accessed via AeroGpuAtomic*U64 helpers to avoid torn 64-bit reads/writes on x86.")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())

