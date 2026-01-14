#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class GuestSelftestVirtioBlkMiniportInfoSkipMarkerTests(unittest.TestCase):
    def setUp(self) -> None:
        cpp_path = (
            Path(__file__).resolve().parents[2] / "guest-selftest" / "src" / "main.cpp"
        )
        self.text = cpp_path.read_text(encoding="utf-8", errors="replace")

    def test_emits_skip_markers_when_miniport_info_unavailable(self) -> None:
        # When the miniport diagnostics payload isn't available, the selftest should emit
        # dedicated SKIP markers for the derived regions so host harnesses can distinguish
        # "missing marker" from "miniport info unavailable".
        self.assertIn(
            "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|SKIP|reason=no_miniport_info",
            self.text,
        )
        self.assertIn(
            "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|SKIP|reason=no_miniport_info",
            self.text,
        )


if __name__ == "__main__":
    unittest.main()

