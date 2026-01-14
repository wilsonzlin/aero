#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellInputEventsExtendedFailMarkerDetailsTests(unittest.TestCase):
    def setUp(self) -> None:
        self.ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = self.ps_path.read_text(encoding="utf-8", errors="replace")

    def test_extended_failed_parses_fail_marker(self) -> None:
        m = re.search(
            r'"VIRTIO_INPUT_EVENTS_EXTENDED_FAILED"\s*\{(?P<body>[\s\S]*?)\r?\n\s*\}\r?\n\s*"QMP_INPUT_INJECT_FAILED"\s*\{',
            self.text,
        )
        self.assertIsNotNone(m)
        assert m is not None
        body = m.group("body")

        # Should try to find the last failing subtest marker line (tail-truncation safe).
        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-modifiers|FAIL|"', body)
        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-buttons|FAIL|"', body)
        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-wheel|FAIL|"', body)

        # Should parse common fields from whichever marker was found.
        for pat in (
            "reason=([^|\\r\\n]+)",
            "err=([^|\\r\\n]+)",
            "kbd_reports=([^|\\r\\n]+)",
            "mouse_reports=([^|\\r\\n]+)",
            "wheel_total=([^|\\r\\n]+)",
        ):
            self.assertIn(pat, body)

        self.assertIn("reported FAIL while -WithInputEventsExtended/-WithInputEventsExtra was enabled$details", body)


if __name__ == "__main__":
    unittest.main()

