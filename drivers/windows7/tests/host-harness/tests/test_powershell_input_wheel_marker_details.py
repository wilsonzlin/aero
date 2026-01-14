#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellInputWheelMarkerDetailsTests(unittest.TestCase):
    def setUp(self) -> None:
        self.ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = self.ps_path.read_text(encoding="utf-8", errors="replace")

    def test_skip_reason_and_counters_are_parsed_from_marker(self) -> None:
        m = re.search(
            r'"VIRTIO_INPUT_WHEEL_SKIPPED"\s*\{(?P<body>[\s\S]*?)\r?\n\s*\}\r?\n\s*"VIRTIO_INPUT_WHEEL_FAILED"\s*\{',
            self.text,
        )
        self.assertIsNotNone(m)
        assert m is not None
        body = m.group("body")

        self.assertIn(
            '-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|SKIP|"',
            body,
        )
        self.assertIn("\\|SKIP\\|([^|\\r\\n=]+)(?:\\||$)", body)
        self.assertIn("wheel_total=([^|\\r\\n]+)", body)
        self.assertIn("hwheel_total=([^|\\r\\n]+)", body)
        self.assertIn('$code -eq "flag_not_set"', body)
        self.assertIn("--test-input-events", body)

    def test_fail_reason_and_counters_are_parsed_from_marker(self) -> None:
        m = re.search(
            r'"VIRTIO_INPUT_WHEEL_FAILED"\s*\{(?P<body>[\s\S]*?)\r?\n\s*\}\r?\n\s*"VIRTIO_INPUT_EVENTS_EXTENDED_FAILED"\s*\{',
            self.text,
        )
        self.assertIsNotNone(m)
        assert m is not None
        body = m.group("body")

        self.assertIn(
            '-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|FAIL|"',
            body,
        )
        self.assertIn("reason=([^|\\r\\n]+)", body)
        # Backcompat: accept token-only FAIL markers (no `reason=` field).
        self.assertIn("\\|FAIL\\|([^|\\r\\n=]+)(?:\\||$)", body)
        self.assertIn("wheel_total=([^|\\r\\n]+)", body)
        self.assertIn("hwheel_total=([^|\\r\\n]+)", body)
        for pat in (
            "expected_wheel=([^|\\r\\n]+)",
            "expected_hwheel=([^|\\r\\n]+)",
            "wheel_events=([^|\\r\\n]+)",
            "hwheel_events=([^|\\r\\n]+)",
            "saw_wheel=([^|\\r\\n]+)",
            "saw_hwheel=([^|\\r\\n]+)",
        ):
            self.assertIn(pat, body)
        self.assertIn('$details = "(reason=$reason"', body)
        self.assertIn("expected_wheel=$expectedWheel", body)
        self.assertIn("wheel_events=$wheelEvents", body)
        self.assertIn("enabled $details", body)


if __name__ == "__main__":
    unittest.main()
