#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class PowerShellHarnessQemuSystemExistenceValidationTests(unittest.TestCase):
    def setUp(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = ps_path.read_text(encoding="utf-8", errors="replace")

    def test_rejects_missing_qemu_system_paths_unless_dry_run(self) -> None:
        # If QemuSystem is given as a path, it should exist (and not be a directory); otherwise, it must
        # be resolvable via PATH. Dry-run skips this requirement.
        self.assertIn("-QemuSystem must be a QEMU system binary path (file not found):", self.text)
        self.assertIn("-QemuSystem must be on PATH (qemu-system binary not found):", self.text)
        self.assertIn("if (-not $DryRun)", self.text)
        self.assertIn("Test-Path -LiteralPath $QemuSystem -PathType Leaf", self.text)


if __name__ == "__main__":
    unittest.main()

