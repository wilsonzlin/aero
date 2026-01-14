#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PsHarnessHttpLogPathParamTests(unittest.TestCase):
    def test_http_log_path_param_exists_and_default_empty(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        text = ps_path.read_text(encoding="utf-8", errors="replace")

        # Extract the top-level script param() block (avoid matching function-level param blocks).
        m = re.search(
            r"\[CmdletBinding\(\)\]\s*param\((?P<body>.*?)\)\s*Set-StrictMode",
            text,
            flags=re.S,
        )
        self.assertIsNotNone(m, f"failed to locate top-level param() block in {ps_path}")
        body = m.group("body")

        lines = body.splitlines()
        for i, line in enumerate(lines):
            if re.search(r"\[string\]\$HttpLogPath\b", line):
                default_m = re.search(r"\$HttpLogPath\s*=\s*(?P<default>\"\"|\$null)", line)
                self.assertIsNotNone(default_m, f"failed to parse $HttpLogPath default in: {line!r}")
                self.assertEqual(default_m.group("default"), '""', "default $HttpLogPath must be empty (no logging)")

                # Ensure the parameter stays optional (not Mandatory=$true).
                j = i - 1
                while j >= 0:
                    prev = lines[j].strip()
                    if not prev or prev.startswith("#"):
                        j -= 1
                        continue
                    self.assertIn(
                        "Mandatory = $false",
                        prev,
                        "expected [Parameter(Mandatory = $false)] for $HttpLogPath",
                    )
                    break
                else:
                    self.fail("missing [Parameter(...)] attribute for $HttpLogPath")

                return

        self.fail("missing [string]$HttpLogPath parameter in PowerShell harness param() block")


if __name__ == "__main__":
    unittest.main()

