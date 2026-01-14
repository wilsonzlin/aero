#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class PowerShellHarnessNetLinkFlapFlagTests(unittest.TestCase):
    def setUp(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = ps_path.read_text(encoding="utf-8", errors="replace")

    def test_with_net_link_flap_param_exists(self) -> None:
        # Ensure the public harness switch exists (so users can enable the QMP flap + require markers).
        self.assertIn("[switch]$WithNetLinkFlap", self.text)
        self.assertIn('Alias("WithVirtioNetLinkFlap", "EnableVirtioNetLinkFlap")', self.text)

    def test_wait_result_enforces_link_flap_marker_when_required(self) -> None:
        # Ensure the Wait-AeroSelftestResult plumbing exists and returns stable tokens.
        self.assertIn("$RequireVirtioNetLinkFlapPass", self.text)
        for token in (
            "MISSING_VIRTIO_NET_LINK_FLAP",
            "VIRTIO_NET_LINK_FLAP_SKIPPED",
            "VIRTIO_NET_LINK_FLAP_FAILED",
            "QMP_NET_LINK_FLAP_FAILED",
        ):
            self.assertIn(token, self.text)

    def test_qmp_flap_targets_stable_net_device_id(self) -> None:
        # The host harness targets the virtio-net QOM id via QMP set_link.
        self.assertIn('$script:VirtioNetQmpId = "aero_virtio_net0"', self.text)
        self.assertIn('execute = "set_link"', self.text)
        self.assertIn("name = $script:VirtioNetQmpId", self.text)


if __name__ == "__main__":
    unittest.main()

