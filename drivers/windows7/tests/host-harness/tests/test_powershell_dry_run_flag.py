#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellDryRunFlagTests(unittest.TestCase):
    def test_powershell_harness_has_dry_run_switch_and_exits_early(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        text = ps_path.read_text(encoding="utf-8", errors="replace")

        # Parameter exists.
        self.assertRegex(text, re.compile(r"\[switch\]\s*\$DryRun\b", re.IGNORECASE))
        # Alias exists (parity with `-DryRun` / `-PrintQemuArgs` requirements).
        self.assertRegex(text, re.compile(r"\[Alias\(\s*\"PrintQemuArgs\"\s*\)\]", re.IGNORECASE))

        # The dry-run block exists and exits 0 (so QEMU/servers are not started).
        m = re.search(r"if\s*\(\s*\$DryRun\s*\)\s*\{", text, flags=re.IGNORECASE)
        self.assertIsNotNone(m, "expected an `if ($DryRun) { ... }` block")
        assert m is not None
        self.assertRegex(text[m.start() :], re.compile(r"\bexit\s+0\b", re.IGNORECASE))

        # Ensure virtio-snd capability probing is gated so dry-run doesn't execute QEMU subprocess calls.
        self.assertRegex(
            text,
            re.compile(r"if\s*\(\s*\$WithVirtioSnd\s*-and\s*\(-not\s*\$DryRun\)\s*\)", re.IGNORECASE),
        )


if __name__ == "__main__":
    unittest.main()
