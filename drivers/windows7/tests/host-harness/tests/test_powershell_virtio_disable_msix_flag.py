#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellVirtioDisableMsixFlagTests(unittest.TestCase):
    def setUp(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = ps_path.read_text(encoding="utf-8", errors="replace")

    def test_param_exists_and_aliases(self) -> None:
        # Public parameter exists.
        self.assertRegex(self.text, re.compile(r"\[switch\]\s*\$VirtioDisableMsix\b", re.IGNORECASE))
        # Alias list may grow; keep the test tolerant to additional aliases.
        self.assertRegex(
            self.text,
            re.compile(r'Alias\([^)]*"ForceIntx"[^)]*"IntxOnly"[^)]*\)', re.IGNORECASE),
        )

    def test_mutually_exclusive_with_vectors_flags(self) -> None:
        # Ensure we fail fast on invalid combinations so users don't get an opaque QEMU startup failure.
        self.assertIn("mutually exclusive", self.text)
        self.assertIn("vectors=0", self.text)

    def test_vectors_zero_preflight_helper_exists(self) -> None:
        # Ensure a dedicated helper exists to validate `vectors=0` is accepted (some QEMU builds reject 0).
        self.assertIn("function Assert-AeroWin7QemuAcceptsVectorsZero", self.text)
        self.assertRegex(
            self.text,
            re.compile(
                r'Get-AeroWin7QemuDeviceHelpText\s+-QemuSystem\s+\$QemuSystem\s+-DeviceName\s+"\$DeviceName,vectors=0"',
                re.IGNORECASE,
            ),
        )

    def test_non_dry_run_preflights_vectors_zero_for_core_devices(self) -> None:
        # Ensure the main harness path probes vectors=0 support before starting QEMU.
        for dev in ("virtio-net-pci", "virtio-blk-pci"):
            with self.subTest(dev=dev):
                self.assertIn(
                    f'Assert-AeroWin7QemuAcceptsVectorsZero -QemuSystem $QemuSystem -DeviceName "{dev}"',
                    self.text,
                )

    def test_dry_run_gates_vectors_property_probing(self) -> None:
        # Dry-run must not spawn QEMU subprocess probes. Resolve-AeroWin7QemuMsixVectors contains an
        # explicit dry-run early return to keep behavior deterministic.
        self.assertRegex(
            self.text,
            re.compile(r"if\s*\(\s*\$DryRun\s*\)\s*\{\s*return\s*\$Vectors\s*\}", re.IGNORECASE),
        )


if __name__ == "__main__":
    unittest.main()

