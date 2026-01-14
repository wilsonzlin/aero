#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellVirtioSndBufferLimitsReasonTokenTests(unittest.TestCase):
    def setUp(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = ps_path.read_text(encoding="utf-8", errors="replace")

    def _extract_function_text(self, name: str) -> str:
        m = re.search(rf"(?m)^\s*function\s+{re.escape(name)}\b", self.text)
        self.assertIsNotNone(m, f"missing PowerShell function {name}")
        assert m is not None
        start = m.start()

        m2 = re.search(r"(?m)^\s*function\s+", self.text[m.end() :])
        end = len(self.text) if m2 is None else (m.end() + m2.start())
        return self.text[start:end]

    def test_buffer_limits_mirrors_plain_reason_for_fail_or_skip(self) -> None:
        fn = self._extract_function_text("Try-EmitAeroVirtioSndBufferLimitsMarker")

        # The guest may emit SKIP/FAIL markers like:
        #   AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|SKIP|flag_not_set
        # so the harness should mirror the plain token into reason=... for stable log scraping.
        # Allow either FAIL/SKIP ordering in the condition.
        self.assertRegex(
            fn,
            r"(?:"
            r"\$status\s*-eq\s*['\"]FAIL['\"]\s*-or\s*\$status\s*-eq\s*['\"]SKIP['\"]"
            r"|"
            r"\$status\s*-eq\s*['\"]SKIP['\"]\s*-or\s*\$status\s*-eq\s*['\"]FAIL['\"]"
            r")",
        )
        self.assertRegex(fn, r"\$toks\[\$i\]\.Trim\(\)\s*-eq\s*\$status")
        self.assertRegex(fn, r"\$fields\[\s*['\"]reason['\"]\s*\]\s*=")


if __name__ == "__main__":
    unittest.main()
