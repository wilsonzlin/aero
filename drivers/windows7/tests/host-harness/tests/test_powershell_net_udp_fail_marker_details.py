#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellNetUdpFailMarkerDetailsTests(unittest.TestCase):
    def setUp(self) -> None:
        self.ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = self.ps_path.read_text(encoding="utf-8", errors="replace")

    def test_fail_details_are_parsed_from_marker(self) -> None:
        m = re.search(
            r'"VIRTIO_NET_UDP_FAILED"\s*\{(?P<body>[\s\S]*?)\r?\n\s*\}\r?\n\s*"VIRTIO_NET_UDP_SKIPPED"\s*\{',
            self.text,
        )
        self.assertIsNotNone(m)
        assert m is not None
        body = m.group("body")

        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|FAIL|"', body)
        self.assertIn("reason=([^|\\r\\n]+)", body)
        self.assertIn("wsa=([^|\\r\\n]+)", body)
        self.assertIn("bytes=([^|\\r\\n]+)", body)
        self.assertIn("small_bytes=([^|\\r\\n]+)", body)
        self.assertIn("mtu_bytes=([^|\\r\\n]+)", body)
        self.assertIn("virtio-net-udp test reported FAIL$details", body)


if __name__ == "__main__":
    unittest.main()

