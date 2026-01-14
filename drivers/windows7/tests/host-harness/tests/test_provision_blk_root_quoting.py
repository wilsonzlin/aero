#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


def _commandline_to_argv(cmd: str) -> list[str]:
    """
    Minimal CommandLineToArgvW-compatible parser (sufficient for our quoting tests).

    Windows parsing rules (simplified):
    - Whitespace separates args when not inside quotes.
    - Backslashes before a quote are treated specially:
      - 2N backslashes + quote => N backslashes and toggles in_quotes
      - 2N+1 backslashes + quote => N backslashes + literal quote
    """

    argv: list[str] = []
    i = 0
    n = len(cmd)

    while True:
        while i < n and cmd[i] in " \t":
            i += 1
        if i >= n:
            break

        arg = ""
        in_quotes = False
        while i < n:
            ch = cmd[i]
            if ch in " \t" and not in_quotes:
                break

            if ch == "\\":
                start = i
                while i < n and cmd[i] == "\\":
                    i += 1
                bs_count = i - start

                if i < n and cmd[i] == '"':
                    arg += "\\" * (bs_count // 2)
                    if bs_count % 2 == 0:
                        in_quotes = not in_quotes
                    else:
                        arg += '"'
                    i += 1
                    continue

                arg += "\\" * bs_count
                continue

            if ch == '"':
                in_quotes = not in_quotes
                i += 1
                continue

            arg += ch
            i += 1

        argv.append(arg)
        while i < n and cmd[i] in " \t":
            i += 1

    return argv


def _escape_blk_root_for_schtasks_tr(blk_root: str) -> str:
    # Match the PowerShell logic in New-AeroWin7TestImage.ps1: expand trailing backslashes by 4x
    # so that after *two* layers of Windows command-line parsing the selftest sees the original
    # trailing `\`.
    return re.sub(r"\\+$", lambda m: m.group(0) * 4, blk_root)


class ProvisionBlkRootQuotingTests(unittest.TestCase):
    def test_ps1_uses_4x_trailing_backslash_expansion(self) -> None:
        ps1 = (
            Path(__file__).resolve().parents[1] / "New-AeroWin7TestImage.ps1"
        ).read_text(encoding="utf-8", errors="replace")
        self.assertIn(r'"\\+$"', ps1)
        # Keep this intentionally simple/brittle: we want CI to scream if the quoting logic changes.
        self.assertIn("$m.Value + $m.Value + $m.Value + $m.Value", ps1)

    def test_blk_root_trailing_backslash_does_not_swallow_following_flags(self) -> None:
        blk_root = "D:\\aero tests\\with space\\"
        escaped = _escape_blk_root_for_schtasks_tr(blk_root)

        tr = f'\\"C:\\AeroTests\\aero-virtio-selftest.exe\\" --blk-root \\"{escaped}\\" --expect-blk-msi'
        cmd = f'schtasks /Create /TN "AeroVirtioSelftest" /TR "{tr}"'

        argv = _commandline_to_argv(cmd)
        self.assertIn("/TR", argv)
        tr_value = argv[argv.index("/TR") + 1]

        # Second parse: scheduled task runs the stored command line.
        tr_argv = _commandline_to_argv(tr_value)
        self.assertIn("--blk-root", tr_argv)
        self.assertIn("--expect-blk-msi", tr_argv)
        self.assertEqual(tr_argv[tr_argv.index("--blk-root") + 1], blk_root)


if __name__ == "__main__":
    unittest.main()

