#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellBlkRecoveryGatingTests(unittest.TestCase):
    def setUp(self) -> None:
        self.ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = self.ps_path.read_text(encoding="utf-8", errors="replace")

    def test_switches_exist(self) -> None:
        self.assertRegex(
            self.text,
            re.compile(r"\[switch\]\s*\$RequireNoBlkRecovery\b", re.IGNORECASE),
        )
        self.assertRegex(
            self.text,
            re.compile(r"\[switch\]\s*\$FailOnBlkRecovery\b", re.IGNORECASE),
        )

    def test_fail_on_blk_recovery_falls_back_to_legacy_blk_marker_fields(self) -> None:
        # Backward compatibility: if the guest does not emit the dedicated virtio-blk-counters
        # marker, FailOnBlkRecovery should still be able to gate using legacy counter fields on
        # the virtio-blk per-test marker (abort_srb/reset_device_srb/reset_bus_srb).
        pat = re.compile(
            r"(?is)"
            r"if\s*\(\s*\$FailOnBlkRecovery\s*-and\s*\$result\.Result\s*-eq\s*\"PASS\"\s*\)\s*\{"
            r".*?\$blkPrefix\s*=\s*\"AERO_VIRTIO_SELFTEST\|TEST\|virtio-blk\|\""
            r".*?abort_srb.*?reset_device_srb.*?reset_bus_srb"
        )
        self.assertRegex(self.text, pat)

    def test_fail_on_blk_recovery_does_not_fallback_when_dedicated_marker_skips(self) -> None:
        # If the dedicated virtio-blk-counters marker is present but reports SKIP, the harness
        # should treat counters as unavailable and not fall back to the legacy virtio-blk marker.
        #
        # This test is intentionally structural (regex-based) rather than executing PowerShell.
        pat = re.compile(
            r"(?is)"
            r"if\s*\(\s*\$FailOnBlkRecovery\s*-and\s*\$result\.Result\s*-eq\s*\"PASS\"\s*\)\s*\{"
            r".*?if\s*\(\s*\$null\s*-ne\s*\$line\s*\)\s*\{"
            r".*?if\s*\(\s*\$status\s*-ne\s*\"SKIP\"\s*\)"
        )
        self.assertRegex(self.text, pat)


if __name__ == "__main__":
    unittest.main()
