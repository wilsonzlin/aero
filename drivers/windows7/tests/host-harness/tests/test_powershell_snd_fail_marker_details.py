#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellSndFailMarkerDetailsTests(unittest.TestCase):
    def setUp(self) -> None:
        self.ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = self.ps_path.read_text(encoding="utf-8", errors="replace")

    def _extract_case_body(self, name: str, next_name: str) -> str:
        m = re.search(
            rf'"{re.escape(name)}"\s*\{{(?P<body>[\s\S]*?)\r?\n\s*\}}\r?\n\s*"{re.escape(next_name)}"\s*\{{',
            self.text,
        )
        self.assertIsNotNone(m)
        assert m is not None
        return m.group("body")

    def test_snd_failed_parses_reason_and_irq_fields(self) -> None:
        body = self._extract_case_body("VIRTIO_SND_FAILED", "VIRTIO_SND_CAPTURE_FAILED")
        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL|"', body)
        self.assertIn("reason=([^|\\r\\n]+)", body)
        self.assertIn("irq_mode=([^|\\r\\n]+)", body)
        self.assertIn("irq_message_count=([^|\\r\\n]+)", body)
        self.assertIn("irq_reason=([^|\\r\\n]+)", body)
        self.assertIn("virtio-snd test reported FAIL$details", body)

    def test_snd_capture_failed_parses_reason_and_hr(self) -> None:
        body = self._extract_case_body("VIRTIO_SND_CAPTURE_FAILED", "VIRTIO_SND_DUPLEX_FAILED")
        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|FAIL|"', body)
        self.assertIn("reason=([^|\\r\\n]+)", body)
        self.assertIn("hr=([^|\\r\\n]+)", body)
        self.assertIn("virtio-snd-capture test reported FAIL$details", body)

    def test_snd_duplex_failed_parses_reason_and_hr(self) -> None:
        body = self._extract_case_body("VIRTIO_SND_DUPLEX_FAILED", "MISSING_VIRTIO_SND")
        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|FAIL|"', body)
        self.assertIn("reason=([^|\\r\\n]+)", body)
        self.assertIn("hr=([^|\\r\\n]+)", body)
        self.assertIn("virtio-snd-duplex test reported FAIL$details", body)

    def test_snd_buffer_limits_failed_parses_reason_and_hr(self) -> None:
        body = self._extract_case_body(
            "VIRTIO_SND_BUFFER_LIMITS_FAILED", "MISSING_VIRTIO_SND_BUFFER_LIMITS"
        )
        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|FAIL|"', body)
        self.assertIn("reason=([^|\\r\\n]+)", body)
        self.assertIn("hr=([^|\\r\\n]+)", body)
        self.assertIn("virtio-snd-buffer-limits test reported FAIL$details", body)


if __name__ == "__main__":
    unittest.main()

