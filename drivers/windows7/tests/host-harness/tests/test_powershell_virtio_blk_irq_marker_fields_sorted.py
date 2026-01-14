#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellVirtioBlkIrqMarkerFieldsSortedTests(unittest.TestCase):
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

    def test_blk_irq_marker_emits_extra_irq_msix_fields_sorted(self) -> None:
        fn = self._extract_function_text("Try-EmitAeroVirtioBlkIrqMarker")

        # Base keys should still be emitted explicitly first.
        self.assertIn("|irq_mode=", fn)
        self.assertIn("|irq_message_count=", fn)
        self.assertIn("|msix_queue_vector=", fn)

        # Extra `irq_*` fields should be appended in sorted key order.
        self.assertRegex(
            fn,
            r"foreach\s*\(\s*\$k\s+in\s*\(\s*\$fields\.Keys\s*\|\s*Where-Object\s*\{[^}]*StartsWith\(\"irq_\"\)[^}]*\}\s*\|\s*Sort-Object\s*\)\s*\)\s*\{",
        )

        # Extra `msi_*` / `msix_*` fields should also be appended sorted (after base keys).
        self.assertRegex(
            fn,
            r"foreach\s*\(\s*\$k\s+in\s*\(\s*\$fields\.Keys\s*\|\s*Where-Object\s*\{[^}]*StartsWith\(\"msi_\"\)[^}]*StartsWith\(\"msix_\"\)[^}]*\}\s*\|\s*Sort-Object\s*\)\s*\)\s*\{",
        )

        # Ensure the extra-field loops occur after the base `msix_queue_vector` emission.
        base_pos = fn.find("|msix_queue_vector=")
        extra_pos = fn.find('StartsWith("irq_")')
        self.assertNotEqual(base_pos, -1)
        self.assertNotEqual(extra_pos, -1)
        self.assertLess(base_pos, extra_pos)


if __name__ == "__main__":
    unittest.main()

