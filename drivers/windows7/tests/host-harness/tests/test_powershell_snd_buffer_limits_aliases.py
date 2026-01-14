#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class PowerShellSndBufferLimitsAliasTests(unittest.TestCase):
    def test_invoke_harness_exposes_snd_buffer_limits_aliases(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        text = ps_path.read_text(encoding="utf-8", errors="replace")

        # Keep these aliases stable for ergonomics (and to match Python flags).
        self.assertIn('Alias("WithVirtioSndBufferLimits"', text)
        self.assertIn('"EnableSndBufferLimits"', text)
        self.assertIn('"EnableVirtioSndBufferLimits"', text)
        self.assertIn('"RequireVirtioSndBufferLimits"', text)


if __name__ == "__main__":
    unittest.main()
