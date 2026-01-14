#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class NewAeroWin7TestImagePathValidationTests(unittest.TestCase):
    def setUp(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "New-AeroWin7TestImage.ps1"
        self.text = ps_path.read_text(encoding="utf-8", errors="replace")

    def test_rejects_directory_selftest_exe_path(self) -> None:
        self.assertIn("-SelftestExePath must be a file path (got a directory):", self.text)
        self.assertIn("Test-Path -LiteralPath $SelftestExePath -PathType Container", self.text)

    def test_rejects_file_drivers_dir_path(self) -> None:
        self.assertIn("-DriversDir must be a directory path (got a file):", self.text)
        self.assertIn("Test-Path -LiteralPath $DriversDir -PathType Container", self.text)
        self.assertIn("-not (Test-Path -LiteralPath $DriversDir -PathType Container)", self.text)

    def test_rejects_file_output_dir_path(self) -> None:
        self.assertIn("-OutputDir must be a directory path (got a file):", self.text)
        self.assertIn("Test-Path -LiteralPath $OutputDir -PathType Leaf", self.text)

    def test_rejects_directory_output_iso_path(self) -> None:
        self.assertIn("-OutputIsoPath must be a file path (got a directory):", self.text)
        self.assertIn("Test-Path -LiteralPath $OutputIsoPath -PathType Container", self.text)

if __name__ == "__main__":
    unittest.main()
