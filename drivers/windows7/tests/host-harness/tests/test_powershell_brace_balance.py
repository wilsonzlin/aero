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


def _powershell_brace_balance(text: str) -> tuple[int, _ScanState]:
    """
    Best-effort brace balance check for PowerShell scripts.

    We intentionally ignore braces inside:
    - strings ('...' / "...")
    - here-strings (@'...'@ / @"..."@)
    - comments (#... and <#...#>)

    This is not a full parser, but it reliably catches common accidental syntax breakage
    (e.g. a stray `}`) without requiring PowerShell to be installed in CI.
    """
    depth = 0
    state = _ScanState.NORMAL
    at_line_start = True

    i = 0
    n = len(text)

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
                    i = j
                    continue
            i += 1
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
                    i = j
                    continue
            i += 1
            continue

        if state == _ScanState.LINE_COMMENT:
            if ch == "\n":
                state = _ScanState.NORMAL
            i += 1
            continue

        if state == _ScanState.BLOCK_COMMENT:
            if ch == "#" and nxt == ">":
                state = _ScanState.NORMAL
                i += 2
                continue
            i += 1
            continue

        if state == _ScanState.SINGLE_QUOTE:
            # PowerShell single-quoted strings escape a quote by doubling it: '' => literal '
            if ch == "'" and nxt == "'":
                i += 2
                continue
            if ch == "'":
                state = _ScanState.NORMAL
                i += 1
                continue
            i += 1
            continue

        if state == _ScanState.DOUBLE_QUOTE:
            # Backtick escapes the next character inside double-quoted strings.
            if ch == "`":
                i += 2
                continue
            if ch == '"':
                state = _ScanState.NORMAL
                i += 1
                continue
            i += 1
            continue

        # NORMAL state.
        if ch == "#":
            state = _ScanState.LINE_COMMENT
            i += 1
            continue
        if ch == "<" and nxt == "#":
            state = _ScanState.BLOCK_COMMENT
            i += 2
            continue
        if ch == "@" and nxt == "'":
            state = _ScanState.HERE_SINGLE
            i += 2
            continue
        if ch == "@" and nxt == '"':
            state = _ScanState.HERE_DOUBLE
            i += 2
            continue
        if ch == "'":
            state = _ScanState.SINGLE_QUOTE
            i += 1
            continue
        if ch == '"':
            state = _ScanState.DOUBLE_QUOTE
            i += 1
            continue

        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth < 0:
                return depth, state

        # Track line starts for here-string terminators. `at_line_start` remains true while scanning leading
        # whitespace on a line, and flips to false once we see the first non-whitespace character.
        if ch == "\n":
            at_line_start = True
        elif ch != "\r":
            if at_line_start and ch not in " \t":
                at_line_start = False

        i += 1

    return depth, state


class PowerShellBraceBalanceTests(unittest.TestCase):
    def test_invoke_harness_braces_balance(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        text = ps_path.read_text(encoding="utf-8", errors="replace")

        depth, state = _powershell_brace_balance(text)
        self.assertEqual(state, _ScanState.NORMAL, f"ended in state={state}")
        self.assertEqual(depth, 0, f"brace depth should be 0 at EOF (got {depth})")


if __name__ == "__main__":
    unittest.main()
