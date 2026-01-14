#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class NewAeroWin7TestImageInputLedFlagTests(unittest.TestCase):
    def setUp(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "New-AeroWin7TestImage.ps1"
        self.text = ps_path.read_text(encoding="utf-8", errors="replace")

    def test_test_input_led_param_exists(self) -> None:
        # Ensure provisioning script generator exposes the switch.
        self.assertIn("[switch]$TestInputLed", self.text)

    def test_test_input_led_arg_is_baked_into_scheduled_task(self) -> None:
        # Ensure the generator appends --test-input-led when -TestInputLed is set.
        self.assertIn('$testInputLedArg = " --test-input-led"', self.text)

        # Ensure the scheduled task commandline includes the arg variable.
        self.assertIn("$testInputLedArg$testInputTabletEventsArg", self.text)

    def test_readme_mentions_test_input_led(self) -> None:
        self.assertIn("`-TestInputLed` (adds `--test-input-led` to the scheduled task)", self.text)


if __name__ == "__main__":
    unittest.main()

