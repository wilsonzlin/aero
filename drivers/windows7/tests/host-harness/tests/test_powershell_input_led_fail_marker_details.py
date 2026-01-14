#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellInputLedFailMarkerDetailsTests(unittest.TestCase):
    def setUp(self) -> None:
        self.ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = self.ps_path.read_text(encoding="utf-8", errors="replace")

    def test_input_leds_fail_details_are_parsed(self) -> None:
        m = re.search(
            r'"VIRTIO_INPUT_LEDS_FAILED"\s*\{(?P<body>[\s\S]*?)\r?\n\s*\}\r?\n\s*"MISSING_VIRTIO_INPUT_EVENTS"\s*\{',
            self.text,
        )
        self.assertIsNotNone(m)
        assert m is not None
        body = m.group("body")

        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|FAIL|"', body)
        self.assertIn("reason=([^|\\r\\n]+)", body)
        self.assertIn("\\|FAIL\\|([^|\\r\\n=]+)(?:\\||$)", body)
        self.assertIn("err=([^|\\r\\n]+)", body)
        self.assertIn("writes=([^|\\r\\n]+)", body)
        self.assertIn("reason=$reason err=$err", body)

    def test_input_led_fail_details_are_parsed(self) -> None:
        m = re.search(
            r'"VIRTIO_INPUT_LED_FAILED"\s*\{(?P<body>[\s\S]*?)\r?\n\s*\}\r?\n\s*"QMP_MEDIA_KEYS_UNSUPPORTED"\s*\{',
            self.text,
        )
        self.assertIsNotNone(m)
        assert m is not None
        body = m.group("body")

        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|FAIL|"', body)
        self.assertIn("reason=([^|\\r\\n]+)", body)
        self.assertIn("\\|FAIL\\|([^|\\r\\n=]+)(?:\\||$)", body)
        self.assertIn("err=([^|\\r\\n]+)", body)
        self.assertIn("sent=([^|\\r\\n]+)", body)
        self.assertIn("format=([^|\\r\\n]+)", body)
        self.assertIn("led=([^|\\r\\n]+)", body)
        self.assertIn("reason=$reason err=$err", body)


if __name__ == "__main__":
    unittest.main()

