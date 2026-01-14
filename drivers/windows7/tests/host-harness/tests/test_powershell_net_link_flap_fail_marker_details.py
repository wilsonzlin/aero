#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellNetLinkFlapFailMarkerDetailsTests(unittest.TestCase):
    def setUp(self) -> None:
        self.ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = self.ps_path.read_text(encoding="utf-8", errors="replace")

    def test_net_link_flap_failed_parses_fail_marker_details(self) -> None:
        m = re.search(
            r'"VIRTIO_NET_LINK_FLAP_FAILED"\s*\{(?P<body>[\s\S]*?)\r?\n\s*\}\r?\n\s*"VIRTIO_NET_LINK_FLAP_SKIPPED"\s*\{',
            self.text,
        )
        self.assertIsNotNone(m)
        assert m is not None
        body = m.group("body")

        # Tail-truncation safe extraction of the last FAIL marker line.
        self.assertIn(
            '-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|FAIL|"',
            body,
        )

        # Should parse common reason/counters from the marker.
        for pat in (
            "reason=([^|\\r\\n]+)",
            "down_sec=([^|\\r\\n]+)",
            "up_sec=([^|\\r\\n]+)",
            "cfg_intr_down_delta=([^|\\r\\n]+)",
            "cfg_intr_up_delta=([^|\\r\\n]+)",
        ):
            self.assertIn(pat, body)


if __name__ == "__main__":
    unittest.main()

