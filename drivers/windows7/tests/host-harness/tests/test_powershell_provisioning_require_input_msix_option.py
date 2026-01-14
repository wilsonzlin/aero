#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class PowerShellProvisioningRequireInputMsixOptionTests(unittest.TestCase):
    def test_new_aero_win7_test_image_supports_require_input_msix(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "New-AeroWin7TestImage.ps1"
        text = ps_path.read_text(encoding="utf-8", errors="replace")

        # Parameter and alias should exist.
        self.assertIn("RequireInputMsix", text)
        self.assertIn("RequireVirtioInputMsix", text)

        # The scheduled task should be able to include the guest selftest flag.
        self.assertIn("--require-input-msix", text)


if __name__ == "__main__":
    unittest.main()

