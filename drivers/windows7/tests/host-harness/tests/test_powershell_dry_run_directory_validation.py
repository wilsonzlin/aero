#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class PowerShellHarnessDryRunDirectoryValidationTests(unittest.TestCase):
    def setUp(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = ps_path.read_text(encoding="utf-8", errors="replace")

    def test_dry_run_rejects_directory_disk_image_and_serial_log_paths(self) -> None:
        marker = "# Dry-run should not require that the disk image exists"
        start = self.text.index(marker)
        end = self.text.index("} else {", start)
        dry_run_block = self.text[start:end]

        # Keep PowerShell and Python harness behaviour consistent:
        # passing an existing directory for either path should fail fast even in -DryRun mode.
        self.assertIn("Test-Path -LiteralPath $DiskImagePath -PathType Container", dry_run_block)
        self.assertIn("-DiskImagePath must be a disk image file path (got a directory):", dry_run_block)

        self.assertIn("Test-Path -LiteralPath $SerialLogPath -PathType Container", dry_run_block)
        self.assertIn("-SerialLogPath must be a file path (got a directory):", dry_run_block)


if __name__ == "__main__":
    unittest.main()

