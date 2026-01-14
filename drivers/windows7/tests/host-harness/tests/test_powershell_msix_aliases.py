#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellMsixAliasTests(unittest.TestCase):
    def test_invoke_harness_exposes_require_msix_aliases(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        text = ps_path.read_text(encoding="utf-8", errors="replace")

        # Keep these aliases stable for ergonomics (and to match provisioning script naming).
        # Avoid brittle exact-string matching so whitespace/formatting changes do not break tests.
        self.assertRegex(
            text,
            re.compile(r'\[Alias\s*\((?=[^)]*"RequireNetMsix")[^)]*\)\]', re.IGNORECASE),
        )
        self.assertRegex(
            text,
            re.compile(r'\[Alias\s*\((?=[^)]*"RequireBlkMsix")[^)]*\)\]', re.IGNORECASE),
        )
        self.assertRegex(
            text,
            re.compile(r'\[Alias\s*\((?=[^)]*"RequireSndMsix")[^)]*\)\]', re.IGNORECASE),
        )
        self.assertRegex(
            text,
            re.compile(r'\[Alias\s*\((?=[^)]*"RequireInputMsix")[^)]*\)\]', re.IGNORECASE),
        )


if __name__ == "__main__":
    unittest.main()
