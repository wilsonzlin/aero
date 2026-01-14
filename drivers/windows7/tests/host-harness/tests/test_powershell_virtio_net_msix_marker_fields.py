#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellVirtioNetMsixMarkerFieldsTests(unittest.TestCase):
    def setUp(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = ps_path.read_text(encoding="utf-8", errors="replace")

    def test_host_marker_emits_unknown_fields_sorted(self) -> None:
        # Ensure the PowerShell harness mirrors the guest virtio-net-msix marker into the host marker
        # while preserving forward compatibility with appended fields:
        #  - keeps a stable base ordering (mode/messages/config_vector/rx_vector/tx_vector)
        #  - appends any extra k=v fields in sorted key order (so log scraping remains deterministic)

        self.assertIn("function Try-EmitAeroVirtioNetMsixMarker", self.text)

        # Base ordering list.
        self.assertRegex(
            self.text,
            r'\$ordered\s*=\s*@\(\s*"mode"\s*,\s*"messages"\s*,\s*"config_vector"\s*,\s*"rx_vector"\s*,\s*"tx_vector"',
        )

        # Extra fields are emitted sorted.
        self.assertRegex(
            self.text,
            r"foreach\s*\(\s*\$k\s+in\s*\(\s*\$fields\.Keys\s*\|\s*Where-Object\s*\{[^}]*orderedSet[^}]*\}\s*\|\s*Sort-Object\s*\)\s*\)\s*\{",
        )


if __name__ == "__main__":
    unittest.main()

