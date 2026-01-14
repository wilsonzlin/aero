#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class PowerShellInputRequireAliasTests(unittest.TestCase):
    def test_invoke_harness_exposes_input_require_aliases(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        text = ps_path.read_text(encoding="utf-8", errors="replace")

        # Keep these aliases stable for ergonomics (and to match the Python harness).
        self.assertIn(
            'Alias("WithVirtioInputEvents", "EnableVirtioInputEvents", "RequireVirtioInputEvents")',
            text,
        )
        self.assertIn(
            'Alias("WithVirtioInputMediaKeys", "EnableVirtioInputMediaKeys", "RequireVirtioInputMediaKeys")',
            text,
        )
        self.assertIn(
            'Alias("WithVirtioInputLed", "EnableVirtioInputLed", "RequireVirtioInputLed")',
            text,
        )
        self.assertIn(
            'Alias("WithVirtioInputWheel", "EnableVirtioInputWheel", "RequireVirtioInputWheel")',
            text,
        )
        self.assertIn(
            'Alias("WithVirtioInputTabletEvents", "EnableVirtioInputTabletEvents", "RequireVirtioInputTabletEvents", "WithTabletEvents", "EnableTabletEvents")',
            text,
        )


if __name__ == "__main__":
    unittest.main()

