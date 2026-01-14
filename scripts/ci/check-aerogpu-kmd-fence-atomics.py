#!/usr/bin/env python3
"""
CI guardrail: AeroGPU Win7 KMD fence state must be accessed atomically.

Why:
  - The AeroGPU Win7 kernel-mode driver is built for both x86 and x64.
  - On x86, plain 64-bit loads/stores are not atomic and can tear.
  - Fence bookkeeping fields (LastSubmittedFence/LastCompletedFence/etc) are
    accessed across multiple contexts (submit thread, ISR/DPC, dbgctl escapes).

This script scans `drivers/aerogpu/kmd/src/aerogpu_kmd.c` and enforces that
member accesses to the fence fields are never performed as plain loads/stores.
In practice this means every occurrence must take the address of the field
(`&Adapter->LastCompletedFence`, etc), so the value is read/written via an
Interlocked-based helper.

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

TYPE_KEYWORDS = {
    # C type/qualifier keywords commonly used in casts.
    "const",
    "volatile",
    "signed",
    "unsigned",
    "short",
    "long",
    "int",
    "char",
    "void",
    "struct",
    "union",
    "enum",
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


def _match_open_paren(tokens: list[Token], close_idx: int) -> int | None:
    """
    Given an index to a ')' token, return the matching '(' index (or None).
    """

    if close_idx < 0 or close_idx >= len(tokens) or tokens[close_idx].value != ")":
        return None
    depth = 0
    for i in range(close_idx, -1, -1):
        v = tokens[i].value
        if v == ")":
            depth += 1
        elif v == "(":
            depth -= 1
            if depth == 0:
                return i
    return None


def _is_probably_type_cast(tokens: list[Token], close_idx: int) -> bool:
    """
    Heuristic: determine whether the parentheses ending at `close_idx` look like a
    type-cast, so an `&` immediately following is unary (e.g. `(volatile ULONGLONG*)&x`).

    This is intentionally conservative and tailored for this repo's C style:
      - Allow identifiers that are either in TYPE_KEYWORDS or contain no lowercase
        letters (ULONGLONG, AEROGPU_ADAPTER, ULONG_PTR, ...).
      - Allow `*` tokens, but only as trailing tokens (TYPE*, TYPE**, ...).
      - Reject anything that looks like an expression (numbers or other operators).
    """

    open_idx = _match_open_paren(tokens, close_idx)
    if open_idx is None:
        return False

    inner = tokens[open_idx + 1 : close_idx]
    if not inner:
        return False

    saw_star = False
    saw_typeish_ident = False
    for t in inner:
        if t.kind == "number":
            return False
        if t.value == "*":
            saw_star = True
            continue
        if t.kind != "ident":
            return False
        if t.value in TYPE_KEYWORDS:
            saw_typeish_ident = True
            continue
        # If the identifier contains lowercase letters, assume it's not a type.
        if any(c.islower() for c in t.value):
            return False
        saw_typeish_ident = True

        if saw_star:
            # Non-trailing identifier after '*' => likely an expression (a*b), not a cast.
            return False

    return saw_typeish_ident


def _is_unary_address_of(tokens: list[Token], amp_idx: int) -> bool:
    """
    Determine whether `tokens[amp_idx] == '&'` is unary address-of.

    We treat it as unary when it appears in a context where an expression is
    expected, or when it follows a type-cast close-paren.
    """

    if amp_idx < 0 or amp_idx >= len(tokens) or tokens[amp_idx].value != "&":
        return False
    if amp_idx == 0:
        return True

    prev = tokens[amp_idx - 1]

    if prev.value == ")":
        return _is_probably_type_cast(tokens, amp_idx - 1)

    # Identifiers and numbers generally terminate an expression, making '&' binary.
    if prev.kind in ("ident", "number"):
        # Keywords like `return` do not terminate an expression.
        return prev.kind == "ident" and prev.value in {
            "return",
            "case",
            "sizeof",
            "__forceinline",
        }

    if prev.value in (")", "]", "++", "--"):
        return False

    # Default: treat as unary after operators/delimiters like '(', ',', '='.
    return True


def main() -> int:
    if not SRC.exists():
        print(f"OK: {SRC} not present; skipping.")
        return 0

    src_text = SRC.read_text(encoding="utf-8", errors="replace")
    stripped = _strip_c_comments_and_literals(src_text)
    tokens = _lex(stripped)

    violations: list[tuple[str, int, int]] = []

    for i in range(2, len(tokens)):
        tok = tokens[i]
        if tok.kind != "ident" or tok.value not in FENCE_FIELDS:
            continue
        access_op = tokens[i - 1].value
        if access_op not in ("->", "."):
            continue

        # Find the start of the object expression in `<obj>-><field>`.
        obj_end_idx = i - 2
        if obj_end_idx < 0:
            violations.append((tok.value, tok.line, tok.col))
            continue

        obj_start_idx = obj_end_idx
        if tokens[obj_end_idx].value == ")":
            match = _match_open_paren(tokens, obj_end_idx)
            if match is not None:
                obj_start_idx = match

        # Include any extra wrapping parens, so `&(Adapter->Field)` is treated
        # the same as `&Adapter->Field`.
        expr_start_idx = obj_start_idx
        while expr_start_idx > 0 and tokens[expr_start_idx - 1].value == "(":
            expr_start_idx -= 1

        amp_idx = expr_start_idx - 1
        if amp_idx < 0 or tokens[amp_idx].value != "&" or not _is_unary_address_of(tokens, amp_idx):
            violations.append((tok.value, tok.line, tok.col))

    if not violations:
        print("OK: AeroGPU KMD fence atomic access checks passed.")
        return 0

    lines = src_text.splitlines()
    print("ERROR: Found non-atomic fence field access in aerogpu_kmd.c:\n")
    for field, line, col in violations:
        context = lines[line - 1] if 1 <= line <= len(lines) else ""
        print(f"- {field} at {SRC}:{line}:{col}")
        if context:
            print(f"    {context}")
            print(f"    {' ' * (col - 1)}^")
        print("")

    print("Fence state must only be accessed via Interlocked-based helpers to avoid torn 64-bit reads/writes on x86.")
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
