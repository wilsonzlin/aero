#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellHarnessQemuPciPreflightFlagTests(unittest.TestCase):
    def setUp(self) -> None:
        self.ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = self.ps_path.read_text(encoding="utf-8", errors="replace")

    def test_qemu_preflight_pci_param_exists_with_alias(self) -> None:
        # Ensure the parameter exists and supports the documented alias.
        self.assertRegex(
            self.text,
            r"(?s)\[Alias\(\"QmpPreflightPci\"\)\]\s*\[switch\]\$QemuPreflightPci",
            "missing -QemuPreflightPci parameter (or its -QmpPreflightPci alias)",
        )

    def test_pci_preflight_pass_marker_exists(self) -> None:
        # The marker is used by CI log scrapers; keep it stable.
        self.assertRegex(
            self.text,
            r'Write-Host\s+"AERO_VIRTIO_WIN7_HOST\|QEMU_PCI_PREFLIGHT\|PASS\|mode=transitional\|vendor=1af4\|devices=\$\(Sanitize-AeroMarkerValue \$summary\)"',
        )
        self.assertRegex(
            self.text,
            r'Write-Host\s+"AERO_VIRTIO_WIN7_HOST\|QEMU_PCI_PREFLIGHT\|PASS\|mode=contract-v1\|vendor=1af4\|devices=\$\(Sanitize-AeroMarkerValue \$summary\)"',
        )

    def test_pci_preflight_failed_result_token_exists(self) -> None:
        # The harness should map PCI preflight failures into a stable result token for callers.
        self.assertIn("QEMU_PCI_PREFLIGHT_FAILED", self.text)


if __name__ == "__main__":
    unittest.main()

