#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellCoreFailMarkerDetailsTests(unittest.TestCase):
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

    def test_blk_failed_parses_marker_fields(self) -> None:
        body = self._extract_case_body("VIRTIO_BLK_FAILED", "VIRTIO_BLK_RECOVERY_NONZERO")
        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-blk|FAIL|"', body)
        for pat in (
            "write_ok=([^|\\r\\n]+)",
            "flush_ok=([^|\\r\\n]+)",
            "read_ok=([^|\\r\\n]+)",
            "irq_mode=([^|\\r\\n]+)",
            "irq_message_count=([^|\\r\\n]+)",
        ):
            self.assertIn(pat, body)
        self.assertIn("virtio-blk test reported FAIL$details", body)

    def test_input_failed_parses_marker_fields(self) -> None:
        body = self._extract_case_body("VIRTIO_INPUT_FAILED", "MISSING_VIRTIO_INPUT_MSIX")
        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input|FAIL|"', body)
        for pat in (
            "reason=([^|\\r\\n]+)",
            "devices=([^|\\r\\n]+)",
            "keyboard_devices=([^|\\r\\n]+)",
            "mouse_devices=([^|\\r\\n]+)",
            "ambiguous_devices=([^|\\r\\n]+)",
            "keyboard_collections=([^|\\r\\n]+)",
            "irq_mode=([^|\\r\\n]+)",
            "irq_message_count=([^|\\r\\n]+)",
        ):
            self.assertIn(pat, body)
        self.assertIn("virtio-input test reported FAIL$details", body)

    def test_net_failed_parses_marker_fields(self) -> None:
        body = self._extract_case_body("VIRTIO_NET_FAILED", "VIRTIO_NET_LINK_FLAP_FAILED")
        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-net|FAIL|"', body)
        for pat in (
            "large_ok=([^|\\r\\n]+)",
            "upload_ok=([^|\\r\\n]+)",
            "msi_messages=([^|\\r\\n]+)",
            "irq_mode=([^|\\r\\n]+)",
            "irq_message_count=([^|\\r\\n]+)",
        ):
            self.assertIn(pat, body)
        self.assertIn("virtio-net test reported FAIL$details", body)


if __name__ == "__main__":
    unittest.main()
