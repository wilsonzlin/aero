#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellBlkResetRecoveryGatingTests(unittest.TestCase):
    def setUp(self) -> None:
        self.ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = self.ps_path.read_text(encoding="utf-8", errors="replace")

    def test_switches_exist(self) -> None:
        self.assertRegex(
            self.text,
            re.compile(r"\[switch\]\s*\$RequireNoBlkResetRecovery\b", re.IGNORECASE),
        )
        self.assertRegex(
            self.text,
            re.compile(r"\[switch\]\s*\$FailOnBlkResetRecovery\b", re.IGNORECASE),
        )

    def test_failure_tokens_exist(self) -> None:
        self.assertIn("FAIL: VIRTIO_BLK_RESET_RECOVERY_NONZERO:", self.text)
        self.assertIn("FAIL: VIRTIO_BLK_RESET_RECOVERY_DETECTED:", self.text)

    def test_reset_recovery_parser_falls_back_to_miniport_diag(self) -> None:
        # Backward compatibility: when the guest does not emit the dedicated AERO marker
        # (`AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|...`), the harness should
        # still be able to gate using the older miniport diagnostic line.
        m = re.search(
            re.compile(
                r"(?ims)^function\s+Get-AeroVirtioBlkResetRecoveryCounters\b.*?(?=^function\s+)"
            ),
            self.text,
        )
        self.assertIsNotNone(m)
        assert m is not None
        self.assertIn("virtio-blk-miniport-reset-recovery|", m.group(0))


if __name__ == "__main__":
    unittest.main()
