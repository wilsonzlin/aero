#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellBlkMiniportFlagsGatingTests(unittest.TestCase):
    def setUp(self) -> None:
        self.ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = self.ps_path.read_text(encoding="utf-8", errors="replace")

    def test_switches_exist(self) -> None:
        self.assertRegex(
            self.text,
            re.compile(r"\[switch\]\s*\$RequireNoBlkMiniportFlags\b", re.IGNORECASE),
        )
        self.assertRegex(
            self.text,
            re.compile(r"\[switch\]\s*\$FailOnBlkMiniportFlags\b", re.IGNORECASE),
        )

    def test_failure_tokens_exist(self) -> None:
        self.assertIn("FAIL: VIRTIO_BLK_MINIPORT_FLAGS_NONZERO:", self.text)
        self.assertIn("FAIL: VIRTIO_BLK_MINIPORT_FLAGS_REMOVED:", self.text)


if __name__ == "__main__":
    unittest.main()

