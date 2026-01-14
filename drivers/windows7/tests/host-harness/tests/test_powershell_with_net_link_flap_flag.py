#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellHarnessNetLinkFlapFlagTests(unittest.TestCase):
    def setUp(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = ps_path.read_text(encoding="utf-8", errors="replace")

    def test_with_net_link_flap_param_exists(self) -> None:
        # Ensure the public harness switch exists (so users can enable the QMP flap + require markers).
        self.assertIn("[switch]$WithNetLinkFlap", self.text)
        # Alias list may grow; keep the test tolerant to additional aliases.
        self.assertRegex(
            self.text,
            r'Alias\([^)]*"WithVirtioNetLinkFlap"[^)]*"EnableVirtioNetLinkFlap"[^)]*\)',
        )

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
        # Allow arbitrary whitespace for aligned hashtable formatting.
        self.assertRegex(self.text, r'execute\s*=\s*"set_link"')
        # Ensure the stable QOM id is actually attempted for set_link targeting.
        self.assertIn('$names = @($script:VirtioNetQmpId, "net0")', self.text)
        # Ensure the set_link command forwards the per-attempt name variable.
        self.assertRegex(self.text, r"name\s*=\s*\$name")


if __name__ == "__main__":
    unittest.main()
