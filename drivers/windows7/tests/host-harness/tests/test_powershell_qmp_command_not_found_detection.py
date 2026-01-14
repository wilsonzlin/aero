#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowershellQmpCommandNotFoundDetectionTests(unittest.TestCase):
    def setUp(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = ps_path.read_text(encoding="utf-8", errors="replace")

    def test_device_not_found_is_not_treated_as_missing_command(self) -> None:
        """
        The PowerShell harness uses Test-AeroQmpCommandNotFound to decide whether to fall back
        from QMP-only features (e.g. input-send-event).

        QMP errors like DeviceNotFound can contain the phrase "has not been found", which must
        *not* be misclassified as "QMP command not found" just because the error string also
        contains "QMP command '<name>' failed ...".
        """
        start = self.text.index("function Test-AeroQmpCommandNotFound")
        end = self.text.index("function Invoke-AeroQmpHumanMonitorCommand", start)
        func = self.text[start:end]

        self.assertRegex(
            func,
            re.compile(
                r'if\s*\(\s*\$m\.Contains\("devicenotfound"\)\s*\)\s*\{\s*return\s+\$false\s*\}',
                re.IGNORECASE,
            ),
        )

        # Ensure we don't use a blanket "has not been found" substring match (too broad; matches DeviceNotFound).
        self.assertNotIn('if ($m.Contains("has not been found")) { return $true }', func)

        # Ensure we still detect the QEMU-style "The command <cmd> has not been found" phrasing.
        self.assertIn(r"has\s+not\s+been\s+found", func)


if __name__ == "__main__":
    unittest.main()

