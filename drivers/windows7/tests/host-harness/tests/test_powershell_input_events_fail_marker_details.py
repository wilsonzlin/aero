#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellInputEventsFailMarkerDetailsTests(unittest.TestCase):
    def setUp(self) -> None:
        self.ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = self.ps_path.read_text(encoding="utf-8", errors="replace")

    def test_fail_reason_err_and_counts_are_parsed(self) -> None:
        m = re.search(
            r'"VIRTIO_INPUT_EVENTS_FAILED"\s*\{(?P<body>[\s\S]*?)\r?\n\s*\}\r?\n\s*"MISSING_VIRTIO_INPUT_MEDIA_KEYS"\s*\{',
            self.text,
        )
        self.assertIsNotNone(m)
        assert m is not None
        body = m.group("body")

        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|FAIL|"', body)
        self.assertIn("reason=([^|\\r\\n]+)", body)
        # Backcompat: token-only FAIL marker (no `reason=` field).
        self.assertIn("\\|FAIL\\|([^|\\r\\n=]+)(?:\\||$)", body)
        self.assertIn("err=([^|\\r\\n]+)", body)
        self.assertIn("kbd_reports=([^|\\r\\n]+)", body)
        self.assertIn("mouse_reports=([^|\\r\\n]+)", body)
        self.assertIn("kbd_bad_reports=([^|\\r\\n]+)", body)
        self.assertIn("mouse_bad_reports=([^|\\r\\n]+)", body)
        self.assertIn("reason=$reason err=$err", body)
        self.assertIn("VIRTIO_INPUT_EVENTS_FAILED", body)


if __name__ == "__main__":
    unittest.main()

