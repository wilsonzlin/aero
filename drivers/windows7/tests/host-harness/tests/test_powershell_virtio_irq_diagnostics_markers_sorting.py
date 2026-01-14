#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellVirtioIrqDiagnosticsMarkersSortingTests(unittest.TestCase):
    def setUp(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = ps_path.read_text(encoding="utf-8", errors="replace")

    def _extract_function_text(self, name: str) -> str:
        m = re.search(rf"(?m)^\s*function\s+{re.escape(name)}\b", self.text)
        self.assertIsNotNone(m, f"missing PowerShell function {name}")
        assert m is not None
        start = m.start()

        m2 = re.search(r"(?m)^\s*function\s+", self.text[m.end() :])
        end = len(self.text) if m2 is None else (m.end() + m2.start())
        return self.text[start:end]

    def test_irq_diag_markers_sort_devices_and_fields(self) -> None:
        fn = self._extract_function_text("Try-EmitAeroVirtioIrqDiagnosticsMarkers")

        # Deterministic output ordering: devices sorted.
        self.assertRegex(
            fn,
            r"foreach\s*\(\s*\$dev\s+in\s*\(\s*\$byDev\.Keys\s*\|\s*Sort-Object\s*\)\s*\)\s*\{",
        )

        # Deterministic output ordering: fields sorted.
        self.assertRegex(
            fn,
            r"foreach\s*\(\s*\$k\s+in\s*\(\s*\$fields\.Keys\s*\|\s*Sort-Object\s*\)\s*\)\s*\{",
        )


if __name__ == "__main__":
    unittest.main()

