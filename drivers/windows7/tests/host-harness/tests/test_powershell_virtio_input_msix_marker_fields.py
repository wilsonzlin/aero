#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellVirtioInputMsixMarkerFieldsTests(unittest.TestCase):
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

    def test_host_marker_emits_unknown_fields_sorted(self) -> None:
        # Ensure deterministic marker formatting: stable base ordering + sorted extra fields.
        fn = self._extract_function_text("Try-EmitAeroVirtioInputMsixMarker")

        self.assertRegex(
            fn,
            r'\$ordered\s*=\s*@\(\s*"mode"\s*,\s*"messages"\s*,\s*"mapping"\s*,\s*"used_vectors"',
        )

        self.assertRegex(
            fn,
            r"foreach\s*\(\s*\$k\s+in\s*\(\s*\$fields\.Keys\s*\|\s*Where-Object\s*\{[^}]*orderedSet[^}]*\}\s*\|\s*Sort-Object\s*\)\s*\)\s*\{",
        )


if __name__ == "__main__":
    unittest.main()
