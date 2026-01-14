#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class PowerShellMsixAliasTests(unittest.TestCase):
    def test_invoke_harness_exposes_require_msix_aliases(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        text = ps_path.read_text(encoding="utf-8", errors="replace")

        # Keep these aliases stable for ergonomics (and to match provisioning script naming).
        self.assertIn('Alias("RequireNetMsix")', text)
        self.assertIn('Alias("RequireBlkMsix")', text)
        self.assertIn('Alias("RequireSndMsix")', text)
        self.assertIn('Alias("RequireInputMsix")', text)


if __name__ == "__main__":
    unittest.main()
