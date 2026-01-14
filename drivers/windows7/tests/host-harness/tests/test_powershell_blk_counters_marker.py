#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellBlkCountersMarkerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = self.ps_path.read_text(encoding="utf-8", errors="replace")

    def test_function_exists(self) -> None:
        self.assertRegex(
            self.text,
            re.compile(r"function\s+Try-EmitAeroVirtioBlkCountersMarker\b", re.IGNORECASE),
        )

    def test_falls_back_to_legacy_blk_marker_fields(self) -> None:
        # Backward compatibility: if the guest does not emit virtio-blk-counters, the harness should
        # still be able to emit a VIRTIO_BLK_COUNTERS host marker from legacy abort_srb/reset_* fields.
        m = re.search(
            re.compile(
                r"(?ims)^function\s+Try-EmitAeroVirtioBlkCountersMarker\b.*?(?=^function\s+)"
            ),
            self.text,
        )
        self.assertIsNotNone(m)
        assert m is not None
        body = m.group(0)
        self.assertIn("AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|", body)
        self.assertIn("AERO_VIRTIO_SELFTEST|TEST|virtio-blk|", body)
        for legacy_key in ("abort_srb", "reset_device_srb", "reset_bus_srb", "pnp_srb", "ioctl_reset"):
            with self.subTest(key=legacy_key):
                self.assertIn(legacy_key, body)

    def test_non_skip_guest_status_is_emitted_as_info(self) -> None:
        # Keep the host marker stable: treat any non-SKIP guest status (PASS/FAIL/INFO/etc)
        # as INFO, and only propagate SKIP.
        m = re.search(
            re.compile(
                r"(?ims)^function\s+Try-EmitAeroVirtioBlkCountersMarker\b.*?(?=^function\s+)"
            ),
            self.text,
        )
        self.assertIsNotNone(m)
        assert m is not None
        body = m.group(0)
        self.assertIn('$status = "INFO"', body)
        self.assertRegex(
            body,
            re.compile(
                r"(?i)if\s*\(\s*\$s\s*-eq\s*\"SKIP\"\s*\)\s*\{\s*\$status\s*=\s*\"SKIP\"\s*\}"
            ),
        )


if __name__ == "__main__":
    unittest.main()
