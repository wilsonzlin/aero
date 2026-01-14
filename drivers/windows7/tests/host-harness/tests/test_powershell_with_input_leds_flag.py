#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellHarnessInputLedsFlagTests(unittest.TestCase):
    def setUp(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = ps_path.read_text(encoding="utf-8", errors="replace")

    def test_with_input_leds_param_exists(self) -> None:
        # Ensure the public harness switch exists (so callers can require virtio-input-leds markers).
        self.assertRegex(self.text, re.compile(r"\[switch\]\s*\$WithInputLeds\b", re.IGNORECASE))

        # Keep alias pattern consistent with other virtio-input flags.
        # The alias list may evolve, so avoid brittle exact-string matching.
        self.assertRegex(
            self.text,
            r'Alias\("WithVirtioInputLeds",\s*"EnableVirtioInputLeds"(?:,\s*"RequireVirtioInputLeds")?\)',
        )

    def test_wait_result_enforces_leds_marker_when_required(self) -> None:
        # Ensure the Wait-AeroSelftestResult plumbing exists and returns stable tokens.
        self.assertIn("$RequireVirtioInputLedsPass", self.text)
        for token in (
            "MISSING_VIRTIO_INPUT_LEDS",
            "VIRTIO_INPUT_LEDS_SKIPPED",
            "VIRTIO_INPUT_LEDS_FAILED",
        ):
            self.assertIn(token, self.text)

        # Ensure the main harness wires the switch into the Wait-AeroSelftestResult requirement param.
        self.assertRegex(
            self.text,
            re.compile(
                r"-RequireVirtioInputLedsPass\s*\(\[bool\]\$WithInputLeds\)",
                re.IGNORECASE,
            ),
        )

    def test_with_input_leds_preflight_requires_keyboard(self) -> None:
        # Input LED/statusq testing requires QEMU to advertise virtio-keyboard-pci.
        self.assertIn(
            "QEMU does not advertise virtio-keyboard-pci but -WithInputLeds was enabled",
            self.text,
        )


if __name__ == "__main__":
    unittest.main()
