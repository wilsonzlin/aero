#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellNetUdpCsumOffloadGatingTests(unittest.TestCase):
    def setUp(self) -> None:
        self.ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = self.ps_path.read_text(encoding="utf-8", errors="replace")

    def test_require_net_udp_csum_offload_switch_exists(self) -> None:
        self.assertRegex(
            self.text,
            re.compile(r"\[switch\]\s*\$RequireNetUdpCsumOffload\b", re.IGNORECASE),
        )
        self.assertRegex(
            self.text,
            re.compile(
                r'\[Alias\s*\((?=[^)]*"RequireVirtioNetUdpCsumOffload")[^)]*\)\]\s*\r?\n\s*\[switch\]\s*\$RequireNetUdpCsumOffload\b',
                re.IGNORECASE,
            ),
        )

    def test_wait_aero_selftest_result_signature_has_param(self) -> None:
        # Ensure the gating flag is plumbed into the core serial marker wait loop.
        self.assertRegex(
            self.text,
            re.compile(
                r"function\s+Wait-AeroSelftestResult\s*\{[\s\S]*?\[bool\]\$RequireNetUdpCsumOffload\b",
                re.IGNORECASE,
            ),
        )

    def test_wait_aero_selftest_result_call_plumbs_param(self) -> None:
        # Ensure the top-level invocation passes the flag through (so users can enable it).
        self.assertRegex(
            self.text,
            re.compile(
                r"Wait-AeroSelftestResult[\s\S]*?-RequireNetUdpCsumOffload\b",
                re.IGNORECASE,
            ),
        )

    def test_failure_tokens_exist_in_output_cases(self) -> None:
        # The PowerShell harness should emit deterministic failure tokens matching the Python harness.
        for tok in (
            "MISSING_VIRTIO_NET_UDP_CSUM_OFFLOAD",
            "VIRTIO_NET_UDP_CSUM_OFFLOAD_FAILED",
            "VIRTIO_NET_UDP_CSUM_OFFLOAD_MISSING_FIELDS",
            "VIRTIO_NET_UDP_CSUM_OFFLOAD_ZERO",
        ):
            self.assertIn(f"FAIL: {tok}:", self.text)

    def test_docs_mention_powershell_flag(self) -> None:
        readme_path = Path(__file__).resolve().parents[1] / "README.md"
        readme = readme_path.read_text(encoding="utf-8", errors="replace")
        self.assertIn("-RequireNetUdpCsumOffload", readme)


if __name__ == "__main__":
    unittest.main()
