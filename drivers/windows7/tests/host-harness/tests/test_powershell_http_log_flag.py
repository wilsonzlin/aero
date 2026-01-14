#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellHarnessHttpLogFlagTests(unittest.TestCase):
    def test_http_log_path_param_exists_and_defaults_empty(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        text = ps_path.read_text(encoding="utf-8", errors="replace")

        m = re.search(
            r"(?m)^[ \t]*\[string\]\$HttpLogPath\s*=\s*(?P<q>['\"])(?P<default>.*?)(?P=q)\s*(?:,|\))",
            text,
        )
        self.assertIsNotNone(m, f"missing HttpLogPath parameter in {ps_path.as_posix()}")
        assert m is not None
        self.assertEqual(
            m.group("default"),
            "",
            "HttpLogPath default must be empty so existing behavior stays unchanged",
        )


if __name__ == "__main__":
    unittest.main()
