#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
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
        # The alias list may evolve, so avoid brittle exact-string matching.
        self.assertRegex(
            self.text,
            r'Alias\("WithVirtioInputLed",\s*"EnableVirtioInputLed"(?:,\s*"RequireVirtioInputLed")?\)',
        )

    def test_wait_result_enforces_led_marker_when_required(self) -> None:
        # Ensure the Wait-AeroSelftestResult plumbing exists and returns stable tokens.
        self.assertIn("$RequireVirtioInputLedPass", self.text)
        for token in (
            "MISSING_VIRTIO_INPUT_LED",
            "VIRTIO_INPUT_LED_SKIPPED",
            "VIRTIO_INPUT_LED_FAILED",
        ):
            self.assertIn(token, self.text)

    def test_with_input_led_preflight_requires_keyboard_and_mouse(self) -> None:
        # In transitional mode, -WithInputLed implies virtio-input coverage, which requires both
        # virtio-keyboard-pci and virtio-mouse-pci to be present (guest selftest base virtio-input
        # test fails when either is missing).
        self.assertIn(
            "QEMU does not advertise virtio-keyboard-pci/virtio-mouse-pci but -WithInputLed was enabled",
            self.text,
        )


if __name__ == "__main__":
    unittest.main()
