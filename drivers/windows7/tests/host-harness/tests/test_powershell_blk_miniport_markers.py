#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellBlkMiniportMarkerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = self.ps_path.read_text(encoding="utf-8", errors="replace")

    def test_flags_function_exists(self) -> None:
        self.assertRegex(
            self.text,
            re.compile(r"function\s+Try-EmitAeroVirtioBlkMiniportFlagsMarker\b", re.IGNORECASE),
        )

    def test_reset_recovery_function_exists(self) -> None:
        self.assertRegex(
            self.text,
            re.compile(
                r"function\s+Try-EmitAeroVirtioBlkMiniportResetRecoveryMarker\b", re.IGNORECASE
            ),
        )

    def test_guest_prefixes_are_used(self) -> None:
        self.assertIn("virtio-blk-miniport-flags|", self.text)
        self.assertIn("virtio-blk-miniport-reset-recovery|", self.text)

    def test_host_markers_are_emitted(self) -> None:
        self.assertIn("AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_MINIPORT_FLAGS|", self.text)
        self.assertIn(
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_MINIPORT_RESET_RECOVERY|",
            self.text,
        )


if __name__ == "__main__":
    unittest.main()

