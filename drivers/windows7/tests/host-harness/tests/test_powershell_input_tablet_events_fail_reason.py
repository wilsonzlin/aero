#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellInputTabletEventsFailReasonTests(unittest.TestCase):
    def setUp(self) -> None:
        self.ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = self.ps_path.read_text(encoding="utf-8", errors="replace")

    def test_fail_reason_and_err_are_parsed_from_marker(self) -> None:
        m = re.search(
            r'"VIRTIO_INPUT_TABLET_EVENTS_FAILED"\s*\{(?P<body>[\s\S]*?)\r?\n\s*\}\r?\n\s*"QMP_INPUT_TABLET_INJECT_FAILED"\s*\{',
            self.text,
        )
        self.assertIsNotNone(m)
        assert m is not None
        body = m.group("body")

        self.assertIn(
            '-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|FAIL|"',
            body,
        )
        self.assertIn("reason=([^|\\r\\n]+)", body)
        # Backcompat: accept token-only FAIL markers (no `reason=` field).
        self.assertIn("\\|FAIL\\|([^|\\r\\n=]+)(?:\\||$)", body)
        self.assertIn("err=([^|\\r\\n]+)", body)
        self.assertIn("reason=$reason err=$err", body)


if __name__ == "__main__":
    unittest.main()

