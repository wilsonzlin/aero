#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellNetOffloadCsumTokenDetailsTests(unittest.TestCase):
    def setUp(self) -> None:
        self.ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = self.ps_path.read_text(encoding="utf-8", errors="replace")

    def _extract_case_body(self, case_name: str, next_case: str) -> str:
        m = re.search(
            rf'"{re.escape(case_name)}"\s*\{{(?P<body>[\s\S]*?)\r?\n\s*\}}\r?\n\s*"{re.escape(next_case)}"\s*\{{',
            self.text,
        )
        self.assertIsNotNone(m, f"failed to locate PowerShell case {case_name}")
        assert m is not None
        return m.group("body")

    def test_net_csum_offload_failed_includes_marker_fields(self) -> None:
        body = self._extract_case_body(
            "VIRTIO_NET_CSUM_OFFLOAD_FAILED",
            "VIRTIO_NET_CSUM_OFFLOAD_MISSING_FIELDS",
        )
        self.assertIn("Get-AeroVirtioNetOffloadCsumStatsFromTail", body)
        self.assertIn("-SerialLogPath $SerialLogPath", body)
        for s in ("status=", "tx_csum=", "rx_csum=", "fallback="):
            self.assertIn(s, body)

    def test_net_csum_offload_zero_includes_marker_fields(self) -> None:
        body = self._extract_case_body(
            "VIRTIO_NET_CSUM_OFFLOAD_ZERO",
            "MISSING_VIRTIO_NET_UDP_CSUM_OFFLOAD",
        )
        self.assertIn("Get-AeroVirtioNetOffloadCsumStatsFromTail", body)
        self.assertIn("-SerialLogPath $SerialLogPath", body)
        for s in ("tx_csum=", "rx_csum=", "fallback="):
            self.assertIn(s, body)

    def test_net_udp_csum_offload_failed_includes_marker_fields(self) -> None:
        body = self._extract_case_body(
            "VIRTIO_NET_UDP_CSUM_OFFLOAD_FAILED",
            "VIRTIO_NET_UDP_CSUM_OFFLOAD_MISSING_FIELDS",
        )
        self.assertIn("Get-AeroVirtioNetOffloadCsumStatsFromTail", body)
        self.assertIn("-SerialLogPath $SerialLogPath", body)
        for s in ("status=", "tx_udp=", "tx_udp4=", "tx_udp6=", "fallback="):
            self.assertIn(s, body)

    def test_net_udp_csum_offload_zero_includes_marker_fields(self) -> None:
        body = self._extract_case_body(
            "VIRTIO_NET_UDP_CSUM_OFFLOAD_ZERO",
            "VIRTIO_SND_SKIPPED",
        )
        self.assertIn("Get-AeroVirtioNetOffloadCsumStatsFromTail", body)
        self.assertIn("-SerialLogPath $SerialLogPath", body)
        for s in ("tx_udp=", "tx_udp4=", "tx_udp6=", "fallback="):
            self.assertIn(s, body)


if __name__ == "__main__":
    unittest.main()

