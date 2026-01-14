#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellBlkRecoveryMarkerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = self.ps_path.read_text(encoding="utf-8", errors="replace")

    def test_function_exists(self) -> None:
        self.assertRegex(
            self.text,
            re.compile(r"function\s+Try-EmitAeroVirtioBlkRecoveryMarker\b", re.IGNORECASE),
        )

    def test_host_marker_is_emitted(self) -> None:
        # The harness should mirror virtio-blk recovery counters into a stable host marker for CI scraping.
        self.assertIn(
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RECOVERY|",
            self.text,
        )

    def test_recovery_counter_parser_falls_back_to_blk_counters_marker(self) -> None:
        # Backward/robustness: if the virtio-blk per-test marker does not include the counters fields
        # (or is missing/truncated), the PowerShell harness should fall back to the dedicated
        # virtio-blk-counters marker.
        m = re.search(
            re.compile(r"(?ims)^function\s+Get-AeroVirtioBlkRecoveryCounters\b.*?(?=^function\s+)"),
            self.text,
        )
        self.assertIsNotNone(m)
        assert m is not None
        body = m.group(0)
        self.assertIn("AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|", body)

        # Ensure the mapping from virtio-blk-counters fields back to legacy names is present.
        for key in ("abort", "reset_device", "reset_bus", "pnp", "ioctl_reset"):
            with self.subTest(key=key):
                self.assertIn(key, body)

    def test_recovery_counter_parser_requires_all_fields(self) -> None:
        # If the counters marker is present but does not include all required fields (e.g. SKIP marker),
        # the parser should return $null rather than producing partial/incorrect results.
        m = re.search(
            re.compile(r"(?ims)^function\s+Get-AeroVirtioBlkRecoveryCounters\b.*?(?=^function\s+)"),
            self.text,
        )
        self.assertIsNotNone(m)
        assert m is not None
        body = m.group(0)
        self.assertRegex(
            body,
            re.compile(
                r"(?i)if\s*\(\s*-not\s*\$cfields\.ContainsKey\(\$src\)\s*\)\s*\{\s*return\s*\$null"
            ),
        )


if __name__ == "__main__":
    unittest.main()
