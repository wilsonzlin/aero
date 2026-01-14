#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class PowerShellHarnessInputLedFlagTests(unittest.TestCase):
    def setUp(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = ps_path.read_text(encoding="utf-8", errors="replace")

    def test_with_input_led_param_exists(self) -> None:
        # Ensure the public harness switch exists (so users can require the marker).
        self.assertIn("[switch]$WithInputLed", self.text)

        # Keep alias pattern consistent with other virtio-input flags.
        self.assertIn('Alias("WithVirtioInputLed", "EnableVirtioInputLed")', self.text)

    def test_wait_result_enforces_led_marker_when_required(self) -> None:
        # Ensure the Wait-AeroSelftestResult plumbing exists and returns stable tokens.
        self.assertIn("$RequireVirtioInputLedPass", self.text)
        for token in (
            "MISSING_VIRTIO_INPUT_LED",
            "VIRTIO_INPUT_LED_SKIPPED",
            "VIRTIO_INPUT_LED_FAILED",
        ):
            self.assertIn(token, self.text)


if __name__ == "__main__":
    unittest.main()

