#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class PowerShellHarnessHttpPathWhitespaceValidationTests(unittest.TestCase):
    def setUp(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = ps_path.read_text(encoding="utf-8", errors="replace")

    def test_harness_rejects_http_path_with_whitespace(self) -> None:
        # Keep PowerShell and Python harness validation consistent.
        self.assertIn("-HttpPath must not contain whitespace.", self.text)
        self.assertIn('$HttpPath -match "\\s"', self.text)


if __name__ == "__main__":
    unittest.main()

