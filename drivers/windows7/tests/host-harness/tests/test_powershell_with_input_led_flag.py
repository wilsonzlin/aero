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
        self.assertRegex(self.text, re.compile(r"\[switch\]\s*\$WithInputLed\b", re.IGNORECASE))

        # Keep alias pattern consistent with other virtio-input flags.
        # The alias list may evolve, so avoid brittle exact-string matching.
        self.assertRegex(
            self.text,
            re.compile(
                r'\[Alias\s*\((?=[^)]*"WithVirtioInputLed")(?=[^)]*"EnableVirtioInputLed")[^)]*\)\]',
                re.IGNORECASE,
            ),
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

        # Ensure we parse the guest SKIP reason from the marker so CI logs can report why it was skipped.
        # This is now done via Try-ExtractLastAeroMarkerLine (tail-truncation safe) rather than a direct
        # rolling-tail regex scan.
        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|SKIP|"', self.text)

    def test_with_input_led_preflight_requires_keyboard_and_mouse(self) -> None:
        # In transitional mode, -WithInputLed implies virtio-input coverage, which requires both
        # virtio-keyboard-pci and virtio-mouse-pci to be present (guest selftest base virtio-input
        # test fails when either is missing).
        self.assertIn("QEMU does not advertise virtio-keyboard-pci/virtio-mouse-pci", self.text)
        self.assertIn("-WithInputLed", self.text)
        # Include common aliases to keep the error actionable even when users invoke via alias flags.
        self.assertIn("-WithVirtioInputLed", self.text)
        self.assertIn("-EnableVirtioInputLed", self.text)


if __name__ == "__main__":
    unittest.main()
