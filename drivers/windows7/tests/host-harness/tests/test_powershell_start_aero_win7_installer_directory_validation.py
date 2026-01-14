#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class PowerShellStartAeroWin7InstallerDirectoryValidationTests(unittest.TestCase):
    def setUp(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Start-AeroWin7Installer.ps1"
        self.text = ps_path.read_text(encoding="utf-8", errors="replace")

    def test_rejects_qemu_system_directory_paths(self) -> None:
        self.assertIn("-QemuSystem must be a QEMU system binary path (got a directory):", self.text)
        self.assertIn("-QemuSystem must be a QEMU system binary path (file not found):", self.text)
        self.assertIn("-QemuSystem must be on PATH (qemu-system binary not found):", self.text)
        self.assertIn('Test-Path -LiteralPath $QemuSystem -PathType Container', self.text)
        self.assertIn('$QemuSystem -match \"[\\\\/\\\\\\\\]\"', self.text)

    def test_rejects_qemu_img_directory_paths_when_create_disk_enabled(self) -> None:
        self.assertIn("-QemuImg must be a qemu-img binary path (got a directory):", self.text)
        self.assertIn("-QemuImg must be a qemu-img binary path (file not found):", self.text)
        self.assertIn("-QemuImg must be on PATH (qemu-img binary not found):", self.text)
        self.assertIn("Test-Path -LiteralPath $QemuImg -PathType Container", self.text)
        self.assertIn('$QemuImg -match \"[\\\\/\\\\\\\\]\"', self.text)

    def test_rejects_win7_iso_directory_paths(self) -> None:
        self.assertIn("-Win7IsoPath must be a file path (got a directory):", self.text)
        self.assertIn("Test-Path -LiteralPath $Win7IsoPath -PathType Container", self.text)

    def test_rejects_disk_image_directory_paths(self) -> None:
        self.assertIn("-DiskImagePath must be a disk image file path (got a directory):", self.text)
        self.assertIn("Test-Path -LiteralPath $DiskImagePath -PathType Container", self.text)

    def test_rejects_provisioning_iso_directory_paths(self) -> None:
        self.assertIn("-ProvisioningIsoPath must be a file path (got a directory):", self.text)
        self.assertIn("Test-Path -LiteralPath $ProvisioningIsoPath -PathType Container", self.text)

    def test_validates_numeric_args_are_positive(self) -> None:
        self.assertIn("-DiskSizeGB must be a positive integer.", self.text)
        self.assertIn("if ($DiskSizeGB -le 0)", self.text)

        self.assertIn("-MemoryMB must be a positive integer.", self.text)
        self.assertIn("if ($MemoryMB -le 0)", self.text)

        self.assertIn("-Smp must be a positive integer.", self.text)
        self.assertIn("if ($Smp -le 0)", self.text)


if __name__ == "__main__":
    unittest.main()
