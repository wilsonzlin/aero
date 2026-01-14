#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class PowerShellQmpSetLinkUnsupportedDetectionTests(unittest.TestCase):
    def setUp(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = ps_path.read_text(encoding="utf-8", errors="replace")

    def test_try_aero_qmp_set_link_treats_unknown_command_as_unsupported(self) -> None:
        """
        The PowerShell harness should treat "unknown command" variants (including GenericError
        descriptions like "The command set_link has not been found") as QMP set_link being
        unsupported, so Wait-AeroSelftestResult can emit the stable QMP_SET_LINK_UNSUPPORTED token.
        """
        start = self.text.index("function Try-AeroQmpSetLink")
        end = self.text.index("function Convert-AeroPciInt", start)
        func = self.text[start:end]

        # Ensure the function consults the shared command-not-found detector for set_link.
        self.assertIn("Test-AeroQmpCommandNotFound", func)
        self.assertIn('-Command "set_link"', func)


if __name__ == "__main__":
    unittest.main()

