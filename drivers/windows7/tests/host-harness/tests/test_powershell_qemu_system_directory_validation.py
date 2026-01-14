#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class PowerShellHarnessQemuSystemDirectoryValidationTests(unittest.TestCase):
    def setUp(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = ps_path.read_text(encoding="utf-8", errors="replace")

    def test_harness_rejects_qemu_system_directory_paths(self) -> None:
        # Keep PowerShell and Python harness validation consistent: when QemuSystem is passed as a
        # path (vs a PATH-resolved command name), it must not point at a directory.
        self.assertIn("-QemuSystem must be a QEMU system binary path (got a directory):", self.text)
        self.assertIn("Test-Path -LiteralPath $QemuSystem -PathType Container", self.text)
        self.assertIn('$QemuSystem -match "[\\\\/\\\\\\\\]"', self.text)


if __name__ == "__main__":
    unittest.main()

