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

    def test_with_blk_reset_aliases_exist(self) -> None:
        # Keep parity with the Python harness: accept WithVirtioBlkReset/EnableVirtioBlkReset/RequireVirtioBlkReset.
        self.assertRegex(
            self.text,
            re.compile(
                r'\[Alias\("WithVirtioBlkReset",\s*"EnableVirtioBlkReset",\s*"RequireVirtioBlkReset"\)\]\s*\r?\n\s*\[switch\]\s*\$WithBlkReset\b',
                re.IGNORECASE,
            ),
        )

    def test_failure_tokens_exist(self) -> None:
        # The PowerShell harness should emit deterministic failure tokens when blk reset is required.
        self.assertIn("FAIL: MISSING_VIRTIO_BLK_RESET:", self.text)
        self.assertIn("FAIL: VIRTIO_BLK_RESET_SKIPPED:", self.text)
        self.assertIn("FAIL: VIRTIO_BLK_RESET_FAILED:", self.text)

    def test_skip_reason_is_parsed_from_marker(self) -> None:
        # Ensure we parse `reason=` from the guest marker so CI logs surface *why* it was skipped.
        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|SKIP|"', self.text)
        self.assertIn('reason=([^|\\r\\n]+)', self.text)

    def test_skip_reason_fallback_handles_legacy_marker_token(self) -> None:
        # Backcompat: older guest selftests may emit `...|SKIP|flag_not_set` (no `reason=` field).
        # Ensure the harness has a fallback parser for that token form.
        self.assertIn("\\|SKIP\\|([^|\\r\\n=]+)(?:\\||$)", self.text)

    def test_fail_reason_and_err_are_parsed_from_marker(self) -> None:
        # Ensure we surface fail reason/err in the deterministic failure token.
        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|FAIL|"', self.text)
        self.assertIn('reason=([^|\\r\\n]+)', self.text)
        self.assertIn('err=([^|\\r\\n]+)', self.text)
        # Backcompat: older guest selftests may emit `...|FAIL|post_reset_io_failed` (no `reason=` field).
        self.assertIn("\\|FAIL\\|([^|\\r\\n=]+)(?:\\||$)", self.text)

    def test_host_marker_is_emitted(self) -> None:
        # The PowerShell harness should mirror the guest marker into a stable host marker
        # for log scraping/debugging (best-effort; does not affect PASS/FAIL).
        self.assertIn("AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESET|", self.text)

    def test_flag_not_set_skip_includes_provisioning_hint(self) -> None:
        # When the guest emits SKIP|reason=flag_not_set and -WithBlkReset is enabled, the harness
        # should surface a clear provisioning hint.
        self.assertIn("flag_not_set", self.text)
        self.assertIn("--test-blk-reset", self.text)


if __name__ == "__main__":
    unittest.main()
