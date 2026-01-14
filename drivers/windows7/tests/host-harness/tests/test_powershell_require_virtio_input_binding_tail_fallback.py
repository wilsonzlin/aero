#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellHarnessVirtioInputBindingTailFallbackTests(unittest.TestCase):
    def setUp(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = ps_path.read_text(encoding="utf-8", errors="replace")

    def test_require_virtio_input_binding_has_tail_truncation_fallback(self) -> None:
        # When -RequireVirtioInputBinding is enabled, the harness should fall back to scanning the full serial
        # log (via Try-ExtractLastAeroMarkerLine) if the rolling tail buffer truncates the marker.
        self.assertIn('$prefix = "AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|"', self.text)
        self.assertRegex(
            self.text,
            re.compile(
                r"Try-ExtractLastAeroMarkerLine\s+-Tail\s+\$tail\s+-Prefix\s+\$prefix\s+-SerialLogPath\s+\$SerialLogPath",
                re.IGNORECASE,
            ),
        )


if __name__ == "__main__":
    unittest.main()

