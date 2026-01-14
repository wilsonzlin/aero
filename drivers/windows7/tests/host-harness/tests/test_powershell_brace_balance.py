#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import enum
import unittest
from pathlib import Path


class _ScanState(enum.Enum):
    NORMAL = "normal"
    SINGLE_QUOTE = "single_quote"
    DOUBLE_QUOTE = "double_quote"
    LINE_COMMENT = "line_comment"
    BLOCK_COMMENT = "block_comment"
    HERE_SINGLE = "here_single"
    HERE_DOUBLE = "here_double"


def _powershell_brace_balance(text: str) -> tuple[int, int, int, _ScanState, int]:
    """
    Best-effort brace balance check for PowerShell scripts.

    We intentionally ignore braces inside:
    - strings ('...' / "...")
    - here-strings (@'...'@ / @"..."@)
    - comments (#... and <#...#>)

    This is not a full parser, but it reliably catches common accidental syntax breakage
    (e.g. a stray `}`) without requiring PowerShell to be installed in CI.
    """

    def advance(chars: int) -> None:
        """
        Consume `chars` bytes from `text` and maintain the `at_line_start` flag.

        Here-string terminators must appear at the start of a line (no leading whitespace), so we
        track only the *absolute* start-of-line position.
        """
        nonlocal i, at_line_start
        for c in text[i : i + chars]:
            if c == "\n":
                at_line_start = True
            elif c != "\r":
                at_line_start = False
        i += chars

    brace_depth = 0
    paren_depth = 0
    bracket_depth = 0
    state = _ScanState.NORMAL
    at_line_start = True  # absolute (no leading whitespace) start of line

    i = 0
    n = len(text)
    embedded_paren_depth: list[int] = []
    embedded_return_state: list[_ScanState] = []

    while i < n:
        ch = text[i]
        nxt = text[i + 1] if i + 1 < n else ""
        line_start = at_line_start

        if state == _ScanState.HERE_SINGLE:
            # Here-string terminator must be at the start of a line: "'@" (optionally followed by whitespace).
            if line_start and ch == "'" and nxt == "@":
                # Ensure remainder of line is whitespace.
                j = i + 2
                while j < n and text[j] not in "\r\n":
                    if text[j] not in " \t":
                        break
                    j += 1
                else_ok = j == n or text[j] in "\r\n"
                if else_ok:
                    state = _ScanState.NORMAL
                    advance(j - i)
                    continue
            advance(1)
            continue

        if state == _ScanState.HERE_DOUBLE:
            if line_start and ch == '"' and nxt == "@":
                j = i + 2
                while j < n and text[j] not in "\r\n":
                    if text[j] not in " \t":
                        break
                    j += 1
                else_ok = j == n or text[j] in "\r\n"
                if else_ok:
                    state = _ScanState.NORMAL
                    advance(j - i)
                    continue
            advance(1)
            continue

        if state == _ScanState.LINE_COMMENT:
            if ch == "\n":
                state = _ScanState.NORMAL
            advance(1)
            continue

        if state == _ScanState.BLOCK_COMMENT:
            if ch == "#" and nxt == ">":
                state = _ScanState.NORMAL
                advance(2)
                continue
            advance(1)
            continue

        if state == _ScanState.SINGLE_QUOTE:
            # PowerShell single-quoted strings escape a quote by doubling it: '' => literal '
            if ch == "'" and nxt == "'":
                advance(2)
                continue
            if ch == "'":
                state = _ScanState.NORMAL
                advance(1)
                continue
            advance(1)
            continue

        if state == _ScanState.DOUBLE_QUOTE:
            # Backtick escapes the next character inside double-quoted strings.
            if ch == "`":
                # Backtick at EOF is allowed (treat as literal).
                if i + 1 < n:
                    advance(2)
                else:
                    advance(1)
                continue
            # Expandable strings can contain embedded expressions: "$(...)".
            # The expression is parsed as normal PowerShell code and can contain quotes, comments,
            # nested parens, etc, so we must not treat those tokens as terminating the outer string.
            if ch == "$" and nxt == "(":
                embedded_paren_depth.append(1)
                embedded_return_state.append(state)
                state = _ScanState.NORMAL
                paren_depth += 1
                advance(2)
                continue
            # Variable expansion with braces: "${var}".
            # Treat the whole construct as part of the string and skip until the closing brace.
            if ch == "$" and nxt == "{":
                j = i + 2
                while j < n:
                    cj = text[j]
                    if cj == "`" and j + 1 < n:
                        j += 2
                        continue
                    if cj == "}":
                        j += 1
                        break
                    j += 1
                if j > n:
                    return brace_depth, paren_depth, bracket_depth, state, len(embedded_paren_depth)
                advance(j - i)
                continue
            if ch == '"':
                state = _ScanState.NORMAL
                advance(1)
                continue
            advance(1)
            continue

        # NORMAL state.
        if ch == "#":
            state = _ScanState.LINE_COMMENT
            advance(1)
            continue
        if ch == "<" and nxt == "#":
            state = _ScanState.BLOCK_COMMENT
            advance(2)
            continue
        if ch == "@" and nxt == "'":
            state = _ScanState.HERE_SINGLE
            advance(2)
            continue
        if ch == "@" and nxt == '"':
            state = _ScanState.HERE_DOUBLE
            advance(2)
            continue
        if ch == "'":
            state = _ScanState.SINGLE_QUOTE
            advance(1)
            continue
        if ch == '"':
            state = _ScanState.DOUBLE_QUOTE
            advance(1)
            continue

        # If we're currently scanning an embedded "$(...)" expression from within an expandable
        # string, track paren nesting until the expression closes.
        handled_embedded_paren = False
        if embedded_paren_depth:
            if ch == "(":
                embedded_paren_depth[-1] += 1
                paren_depth += 1
                handled_embedded_paren = True
            elif ch == ")":
                embedded_paren_depth[-1] -= 1
                paren_depth -= 1
                if paren_depth < 0:
                    return brace_depth, paren_depth, bracket_depth, state, len(embedded_paren_depth)
                handled_embedded_paren = True
                if embedded_paren_depth[-1] == 0:
                    embedded_paren_depth.pop()
                    state = embedded_return_state.pop()
                    advance(1)
                    continue

        if ch == "{":
            brace_depth += 1
        elif ch == "}":
            brace_depth -= 1
            if brace_depth < 0:
                return brace_depth, paren_depth, bracket_depth, state, len(embedded_paren_depth)

        if not handled_embedded_paren:
            if ch == "(":
                paren_depth += 1
            elif ch == ")":
                paren_depth -= 1
                if paren_depth < 0:
                    return brace_depth, paren_depth, bracket_depth, state, len(embedded_paren_depth)

        if ch == "[":
            bracket_depth += 1
        elif ch == "]":
            bracket_depth -= 1
            if bracket_depth < 0:
                return brace_depth, paren_depth, bracket_depth, state, len(embedded_paren_depth)

        advance(1)

    return brace_depth, paren_depth, bracket_depth, state, len(embedded_paren_depth)


class PowerShellBraceBalanceTests(unittest.TestCase):
    def test_host_harness_powershell_scripts_parse_balanced(self) -> None:
        """
        The Win7 host harness has several PowerShell entrypoints that are not executed by the Python
        unit tests, so syntax regressions can slip in unnoticed.

        Perform a best-effort structural scan to catch missing/extra braces (and unclosed embedded
        $() expressions inside expandable strings) without requiring PowerShell to be installed.
        """
        harness_dir = Path(__file__).resolve().parents[1]
        scripts = [
            "Invoke-AeroVirtioWin7Tests.ps1",
            "New-AeroWin7TestImage.ps1",
            "AeroVirtioWin7QemuArgs.ps1",
            "Start-AeroWin7Installer.ps1",
        ]
        for name in scripts:
            with self.subTest(script=name):
                text = (harness_dir / name).read_text(encoding="utf-8", errors="replace")
                brace_depth, paren_depth, bracket_depth, state, embedded = _powershell_brace_balance(
                    text
                )
                self.assertEqual(state, _ScanState.NORMAL, f"{name}: ended in state={state}")
                self.assertEqual(
                    brace_depth,
                    0,
                    f"{name}: brace depth should be 0 at EOF (got {brace_depth})",
                )
                self.assertEqual(
                    paren_depth,
                    0,
                    f"{name}: paren depth should be 0 at EOF (got {paren_depth})",
                )
                self.assertEqual(
                    bracket_depth,
                    0,
                    f"{name}: bracket depth should be 0 at EOF (got {bracket_depth})",
                )
                self.assertEqual(embedded, 0, f"{name}: unclosed embedded $(...) expression(s): {embedded}")


if __name__ == "__main__":
    unittest.main()
