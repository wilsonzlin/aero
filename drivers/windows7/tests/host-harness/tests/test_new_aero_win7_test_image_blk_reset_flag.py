#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class NewAeroWin7TestImageBlkResetFlagTests(unittest.TestCase):
    def setUp(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "New-AeroWin7TestImage.ps1"
        self.text = ps_path.read_text(encoding="utf-8", errors="replace")

    def test_test_blk_reset_param_exists(self) -> None:
        # Ensure provisioning script generator exposes the switch.
        self.assertIn("[switch]$TestBlkReset", self.text)

    def test_test_blk_reset_alias_exists(self) -> None:
        # For parity with -TestBlkResize, accept -TestVirtioBlkReset.
        self.assertRegex(
            self.text,
            re.compile(
                r'\[Alias\("TestVirtioBlkReset"\)\]\s*\r?\n\s*\[switch\]\$TestBlkReset\b',
                re.IGNORECASE,
            ),
        )

    def test_test_blk_reset_arg_is_baked_into_scheduled_task(self) -> None:
        # Ensure the generator appends --test-blk-reset when -TestBlkReset is set.
        self.assertIn('$testBlkResetArg = " --test-blk-reset"', self.text)

        # Ensure the scheduled task commandline includes the arg variable (avoid brittle ordering assumptions).
        # We match against the specific AeroVirtioSelftest task creation line so this doesn't accidentally
        # bind to earlier schtasks-related comments.
        self.assertRegex(
            self.text,
            re.compile(
                r'schtasks /Create /F /TN "AeroVirtioSelftest".*\$testBlkResetArg',
                re.IGNORECASE | re.DOTALL,
            ),
        )

    def test_readme_mentions_test_blk_reset(self) -> None:
        self.assertIn("generate this media with `-TestBlkReset` (adds `--test-blk-reset`)", self.text)


if __name__ == "__main__":
    unittest.main()

