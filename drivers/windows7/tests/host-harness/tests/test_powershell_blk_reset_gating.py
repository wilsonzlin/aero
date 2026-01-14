#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellBlkResetGatingTests(unittest.TestCase):
    def setUp(self) -> None:
        self.ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = self.ps_path.read_text(encoding="utf-8", errors="replace")

    def test_with_blk_reset_switch_exists(self) -> None:
        # Ensure the host harness exposes an opt-in blk reset requirement flag.
        self.assertRegex(self.text, re.compile(r"\[switch\]\s*\$WithBlkReset\b", re.IGNORECASE))

    def test_failure_tokens_exist(self) -> None:
        # The PowerShell harness should emit deterministic failure tokens when blk reset is required.
        self.assertIn("FAIL: MISSING_VIRTIO_BLK_RESET:", self.text)
        self.assertIn("FAIL: VIRTIO_BLK_RESET_SKIPPED:", self.text)
        self.assertIn("FAIL: VIRTIO_BLK_RESET_FAILED:", self.text)

    def test_skip_reason_is_parsed_from_marker(self) -> None:
        # Ensure we parse `reason=` from the guest marker so CI logs surface *why* it was skipped.
        self.assertIn(r"virtio-blk-reset\\|SKIP\\|reason=", self.text)


if __name__ == "__main__":
    unittest.main()
