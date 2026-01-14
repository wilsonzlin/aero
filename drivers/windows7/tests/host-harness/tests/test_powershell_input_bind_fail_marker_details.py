#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellInputBindFailMarkerDetailsTests(unittest.TestCase):
    def setUp(self) -> None:
        self.ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = self.ps_path.read_text(encoding="utf-8", errors="replace")

    def test_input_bind_failed_parses_marker_fields(self) -> None:
        m = re.search(
            r'"VIRTIO_INPUT_BIND_FAILED"\s*\{(?P<body>[\s\S]*?)\r?\n\s*\}\r?\n\s*"VIRTIO_INPUT_LEDS_SKIPPED"\s*\{',
            self.text,
        )
        self.assertIsNotNone(m)
        assert m is not None
        body = m.group("body")

        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|FAIL|"', body)
        for pat in (
            "reason=([^|\\r\\n]+)",
            "expected=([^|\\r\\n]+)",
            "actual=([^|\\r\\n]+)",
            "pnp_id=([^|\\r\\n]+)",
            "devices=([^|\\r\\n]+)",
            "wrong_service=([^|\\r\\n]+)",
            "missing_service=([^|\\r\\n]+)",
            "problem=([^|\\r\\n]+)",
        ):
            self.assertIn(pat, body)
        self.assertIn("virtio-input-bind test reported FAIL$details", body)


if __name__ == "__main__":
    unittest.main()

