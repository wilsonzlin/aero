#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellBlkResetRecoveryMarkerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = self.ps_path.read_text(encoding="utf-8", errors="replace")

    def test_function_exists(self) -> None:
        self.assertRegex(
            self.text,
            re.compile(r"function\s+Try-EmitAeroVirtioBlkResetRecoveryMarker\b", re.IGNORECASE),
        )

    def test_guest_prefix_is_used(self) -> None:
        self.assertIn(
            'AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|',
            self.text,
        )

    def test_host_marker_is_emitted(self) -> None:
        self.assertIn(
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESET_RECOVERY|",
            self.text,
        )

    def test_falls_back_to_miniport_diagnostic_when_aero_marker_missing(self) -> None:
        m = re.search(
            re.compile(
                r"(?ims)^function\s+Try-EmitAeroVirtioBlkResetRecoveryMarker\b.*?(?=^function\s+)"
            ),
            self.text,
        )
        self.assertIsNotNone(m)
        assert m is not None
        self.assertIn("virtio-blk-miniport-reset-recovery|", m.group(0))

    def test_warn_miniport_diag_maps_to_skip_host_marker(self) -> None:
        # Backward compatibility: the legacy miniport diagnostic uses INFO/WARN levels.
        # The host marker should remain stable as INFO/SKIP, mapping WARN -> SKIP.
        m = re.search(
            re.compile(
                r"(?ims)^function\s+Try-EmitAeroVirtioBlkResetRecoveryMarker\b.*?(?=^function\s+)"
            ),
            self.text,
        )
        self.assertIsNotNone(m)
        assert m is not None
        body = m.group(0)
        self.assertRegex(
            body,
            re.compile(
                r"(?i)if\s*\(\s*\$s\s*-eq\s*\"WARN\"\s*\)\s*\{\s*\$status\s*=\s*\"SKIP\"\s*\}"
            ),
        )


if __name__ == "__main__":
    unittest.main()
