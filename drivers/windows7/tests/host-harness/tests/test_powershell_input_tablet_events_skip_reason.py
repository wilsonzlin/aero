#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellInputTabletEventsSkipReasonTests(unittest.TestCase):
    def setUp(self) -> None:
        self.ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = self.ps_path.read_text(encoding="utf-8", errors="replace")

    def test_skip_reason_is_parsed_from_marker(self) -> None:
        # Ensure the VIRTIO_INPUT_TABLET_EVENTS_SKIPPED token includes the guest skip reason (e.g.
        # no_tablet_device) instead of always assuming flag_not_set.
        m = re.search(
            r'"VIRTIO_INPUT_TABLET_EVENTS_SKIPPED"\s*\{(?P<body>[\s\S]*?)\r?\n\s*\}\r?\n\s*"VIRTIO_INPUT_TABLET_EVENTS_FAILED"\s*\{',
            self.text,
        )
        self.assertIsNotNone(m)
        assert m is not None
        body = m.group("body")

        self.assertIn(
            '-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|SKIP|"',
            body,
        )
        self.assertIn('reason=([^|\\r\\n]+)', body)
        # Backcompat: `...|SKIP|flag_not_set` (no `reason=` field).
        self.assertIn("\\|SKIP\\|([^|\\r\\n=]+)(?:\\||$)", body)
        self.assertIn('$reason -eq "flag_not_set"', body)
        self.assertIn('$reason -eq "no_tablet_device"', body)
        self.assertIn("-WithVirtioTablet", body)
        self.assertIn("virtio-input-tablet-events test was skipped ($reason)", body)


if __name__ == "__main__":
    unittest.main()
