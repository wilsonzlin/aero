#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class NewAeroWin7TestImageInputLedsFlagTests(unittest.TestCase):
    def setUp(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "New-AeroWin7TestImage.ps1"
        self.text = ps_path.read_text(encoding="utf-8", errors="replace")

    def test_test_input_leds_param_exists(self) -> None:
        # Ensure provisioning script generator exposes the switch.
        self.assertIn("[switch]$TestInputLeds", self.text)

    def test_test_input_leds_arg_is_baked_into_scheduled_task(self) -> None:
        # Ensure the generator appends --test-input-leds when -TestInputLeds is set.
        self.assertIn('$testInputLedsArg = " --test-input-leds"', self.text)

        # Ensure the scheduled task commandline includes the arg variable (in the /TR line).
        tr_lines = [
            line
            for line in self.text.splitlines()
            if "/TR" in line and "aero-virtio-selftest.exe" in line
        ]
        self.assertTrue(tr_lines, "expected to find schtasks /TR line in provisioning script")
        self.assertTrue(
            any("$testInputLedsArg" in line for line in tr_lines),
            "expected schtasks /TR command line to include $testInputLedsArg",
        )

    def test_readme_mentions_test_input_leds(self) -> None:
        self.assertIn("`-TestInputLeds` (adds `--test-input-leds` to the scheduled task)", self.text)


if __name__ == "__main__":
    unittest.main()

