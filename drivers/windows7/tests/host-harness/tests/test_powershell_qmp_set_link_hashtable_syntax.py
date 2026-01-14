#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class PowerShellQmpSetLinkHashtableSyntaxTests(unittest.TestCase):
    def setUp(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = ps_path.read_text(encoding="utf-8", errors="replace")

    def test_try_aero_qmp_set_link_cmd_hashtables_close_correctly(self) -> None:
        """
        Try-AeroQmpSetLink builds a `$cmd = @{ ... }` hashtable (with a nested `arguments = @{ ... }`)
        for `set_link`.

        A stray extra `}` inside the hashtable block is a syntax error that is easy to miss because
        most unit tests only do text-based assertions (they don't execute PowerShell).

        This test performs a tiny structural check: each `$cmd = @{` set_link block should close the
        *outer* hashtable with a `}` at the same indentation level as the `$cmd = @{` line.
        """
        start = self.text.index("function Try-AeroQmpSetLink")
        end = self.text.index("function Convert-AeroPciInt", start)
        func = self.text[start:end]

        lines = func.splitlines()

        # Locate `$cmd = @{` blocks that correspond to the set_link commands.
        cmd_indices: list[int] = []
        for i, line in enumerate(lines):
            if "$cmd = @{" not in line:
                continue
            window = "\n".join(lines[i : i + 6])
            if 'execute = "set_link"' in window:
                cmd_indices.append(i)

        self.assertTrue(cmd_indices, "expected to find set_link $cmd = @{ blocks in Try-AeroQmpSetLink")

        for i in cmd_indices:
            start_indent = lines[i][: len(lines[i]) - len(lines[i].lstrip())]
            depth = 0
            opened = 0
            closed = 0
            saw_close = False

            for line in lines[i:]:
                opened += line.count("@{")
                depth += line.count("@{")
                if line.strip() == "}":
                    depth -= 1
                    closed += 1
                    if depth == 0 and opened > 0:
                        close_indent = line[: len(line) - len(line.lstrip())]
                        self.assertEqual(
                            close_indent,
                            start_indent,
                            "outer set_link $cmd hashtable must close at the same indentation as it opens",
                        )
                        saw_close = True
                        break

            self.assertTrue(saw_close, "did not find matching closing brace for set_link $cmd hashtable")
            self.assertEqual(
                opened,
                closed,
                "set_link $cmd hashtable open/close count mismatch (possible stray or missing brace)",
            )


if __name__ == "__main__":
    unittest.main()

