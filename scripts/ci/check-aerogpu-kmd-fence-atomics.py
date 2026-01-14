#!/usr/bin/env python3
"""
CI guardrail: AeroGPU Win7 KMD fence state must be accessed atomically.

Why:
  - The AeroGPU Win7 kernel-mode driver is built for both x86 and x64.
  - On x86, plain 64-bit loads/stores are not atomic and can tear.
  - Fence bookkeeping fields (LastSubmittedFence/LastCompletedFence/etc) are
    accessed across multiple contexts (submit thread, ISR/DPC, dbgctl escapes).

This script:
  - validates fence-field alignment invariants in
    `drivers/aerogpu/kmd/include/aerogpu_kmd.h`, and
  - scans AeroGPU Win7 KMD source files under `drivers/aerogpu/kmd/src/`
    and enforces that
member accesses to the fence fields are never performed as plain loads/stores.
In practice this means every occurrence must take the address of the field
(`&Adapter->LastCompletedFence`, etc), so the value is read/written via an
Interlocked-based helper.

It is intentionally lightweight and Linux-friendly; it does not require a WDK.
"""

from __future__ import annotations

from dataclasses import dataclass
import pathlib
import re
import sys


def repo_root() -> pathlib.Path:
    # scripts/ci/<this file> -> scripts/ci -> scripts -> repo root
    return pathlib.Path(__file__).resolve().parents[2]


ROOT = repo_root()
SRC_DIR = ROOT / "drivers" / "aerogpu" / "kmd" / "src"
HDR = ROOT / "drivers" / "aerogpu" / "kmd" / "include" / "aerogpu_kmd.h"


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


def _match_open_bracket(tokens: list[Token], close_idx: int) -> int | None:
    """
    Given an index to a ']' token, return the matching '[' index (or None).
    """

    if close_idx < 0 or close_idx >= len(tokens) or tokens[close_idx].value != "]":
        return None
    depth = 0
    for i in range(close_idx, -1, -1):
        v = tokens[i].value
        if v == "]":
            depth += 1
        elif v == "[":
            depth -= 1
            if depth == 0:
                return i
    return None


def _primary_expr_start(tokens: list[Token], end_idx: int) -> int | None:
    """
    Return the start index of a primary-ish expression ending at `end_idx`.

    This is a minimal heuristic used only for enforcing that fence fields are
    accessed via atomic helpers; it is not a full C parser.
    """

    if end_idx < 0 or end_idx >= len(tokens):
        return None

    end_tok = tokens[end_idx].value

    if end_tok == ")":
        match = _match_open_paren(tokens, end_idx)
        if match is not None:
            return match

    if end_tok == "]":
        match = _match_open_bracket(tokens, end_idx)
        if match is None:
            return end_idx
        # `arr[expr]` ends with ']' but the base expression ends before '['.
        base_end = match - 1
        if base_end < 0:
            return match
        return _member_expr_start(tokens, base_end)

    return end_idx


def _member_expr_start(tokens: list[Token], end_idx: int) -> int | None:
    """
    Return the start index of an expression that may include chained member
    accesses (e.g. `ctx->Adapter->LastSubmittedFence`).
    """

    start = _primary_expr_start(tokens, end_idx)
    if start is None:
        return None

    while True:
        # Handle chains like: <left> -> <ident>   or   <left> . <ident>
        if start >= 2 and tokens[start - 1].value in ("->", "."):
            left_end = start - 2
            left_start = _member_expr_start(tokens, left_end)
            if left_start is None:
                break
            start = left_start
            continue
        break

    return start


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


def _nearest_enclosing_call(paren_stack: list[str | None]) -> str | None:
    for name in reversed(paren_stack):
        if name is not None:
            return name
    return None


def _is_allowed_call(name: str | None) -> bool:
    """
    Fence fields must only be touched through atomic helpers. We accept:
      - `AeroGpuAtomic*U64` (driver abstraction)
      - `Interlocked*64` (direct atomic operations; alignment is ensured by C_ASSERT + DBG ASSERTs elsewhere)
    """

    if name is None:
        return False
    if name.startswith("AeroGpuAtomic") and name.endswith("U64"):
        return True
    if name.startswith("Interlocked") and name.endswith("64"):
        return True
    return False


def _check_header_invariants() -> list[str]:
    errors: list[str] = []
    if not HDR.exists():
        errors.append(f"expected KMD header to exist: {HDR}")
        return errors

    text = HDR.read_text(encoding="utf-8", errors="replace")

    for field in sorted(FENCE_FIELDS):
        decl_re = re.compile(rf"DECLSPEC_ALIGN\s*\(\s*8\s*\)[^;]*\bULONGLONG\b[^;]*\b{re.escape(field)}\b\s*;",
                             re.MULTILINE)
        if not decl_re.search(text):
            errors.append(f"{field}: missing `DECLSPEC_ALIGN(8) (volatile) ULONGLONG {field};` declaration in aerogpu_kmd.h")

        field_offset_re = rf"FIELD_OFFSET\s*\(\s*AEROGPU_ADAPTER\s*,\s*{re.escape(field)}\s*\)"
        c_assert_re = re.compile(rf"C_ASSERT\s*\(\s*[^;]*{field_offset_re}[^;]*\)\s*;", re.MULTILINE)
        c_asserts = list(c_assert_re.finditer(text))
        if not c_asserts:
            errors.append(f"{field}: missing FIELD_OFFSET alignment C_ASSERT in aerogpu_kmd.h")
            continue

        # Be tolerant of suffix variations (7, 7u, 0x7U, 7ULL, etc) as well as
        # alternate formulations (`% 8`).
        has_alignment_check = False
        for m in c_asserts:
            stmt = m.group(0)
            if re.search(r"&\s*\(?\s*(?:0x)?7[uUlL]*\s*\)?", stmt) or re.search(r"%\s*\(?\s*8[uUlL]*\s*\)?", stmt):
                has_alignment_check = True
                break
        if not has_alignment_check:
            errors.append(f"{field}: FIELD_OFFSET C_ASSERT does not appear to enforce 8-byte alignment (expected '& 7' or '% 8')")

    return errors


def main() -> int:
    if not SRC_DIR.exists():
        print(f"OK: {SRC_DIR} not present; skipping.")
        return 0

    header_errors = _check_header_invariants()
    if header_errors:
        print("ERROR: AeroGPU KMD header fence atomicity invariants failed:\n")
        for err in header_errors:
            print(f"- {err}")
        return 1

    sources = sorted(SRC_DIR.rglob("*.c")) + sorted(SRC_DIR.rglob("*.cpp"))
    if not sources:
        print(f"OK: no AeroGPU KMD sources found under {SRC_DIR}; skipping.")
        return 0

    all_violations: list[tuple[pathlib.Path, str, int, int, str]] = []

    for src in sources:
        src_text = src.read_text(encoding="utf-8", errors="replace")
        stripped = _strip_c_comments_and_literals(src_text)
        tokens = _lex(stripped)

        paren_stack: list[str | None] = []
        prev: Token | None = None

        for i, tok in enumerate(tokens):
            if tok.value == "(":
                call_name: str | None = None
                if prev is not None and prev.kind == "ident":
                    call_name = prev.value
                paren_stack.append(call_name)
            elif tok.value == ")":
                if paren_stack:
                    paren_stack.pop()

            if tok.kind == "ident" and tok.value in FENCE_FIELDS and i >= 2 and tokens[i - 1].value in ("->", "."):
                # Find the start of the object expression in `<obj>-><field>`.
                obj_end_idx = i - 2
                obj_start_idx = _member_expr_start(tokens, obj_end_idx)
                if obj_start_idx is None:
                    all_violations.append((src, tok.value, tok.line, tok.col, src_text))
                    prev = tok
                    continue

                # Include any extra wrapping parens, so `&(Adapter->Field)` is treated
                # the same as `&Adapter->Field`.
                expr_start_idx = obj_start_idx
                while expr_start_idx > 0 and tokens[expr_start_idx - 1].value == "(":
                    expr_start_idx -= 1

                amp_idx = expr_start_idx - 1
                if amp_idx < 0 or tokens[amp_idx].value != "&" or not _is_unary_address_of(tokens, amp_idx):
                    all_violations.append((src, tok.value, tok.line, tok.col, src_text))
                    prev = tok
                    continue

                enclosing = _nearest_enclosing_call(paren_stack)
                if not _is_allowed_call(enclosing):
                    all_violations.append((src, tok.value, tok.line, tok.col, src_text))

            prev = tok

    if not all_violations:
        print("OK: AeroGPU KMD fence atomic access checks passed.")
        return 0

    print("ERROR: Found non-atomic fence field access in AeroGPU KMD sources:\n")
    for src, field, line, col, src_text in all_violations:
        lines = src_text.splitlines()
        context = lines[line - 1] if 1 <= line <= len(lines) else ""
        print(f"- {field} at {src}:{line}:{col}")
        if context:
            print(f"    {context}")
            print(f"    {' ' * (col - 1)}^")
        print("")

    print(
        "Fence state must only be accessed via AeroGpuAtomic*U64 (or Interlocked*64) helpers to avoid torn 64-bit reads/writes on x86."
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
