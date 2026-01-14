#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellHarnessInputInjectMarkerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = self.ps_path.read_text(encoding="utf-8", errors="replace")

    def test_events_inject_markers_exist(self) -> None:
        self.assertRegex(
            self.text,
            r'Write-Host\s+"AERO_VIRTIO_WIN7_HOST\|VIRTIO_INPUT_EVENTS_INJECT\|PASS\|attempt=\$Attempt\|backend=\$backend\|kbd_mode=\$kbdMode\|mouse_mode=\$mouseMode"',
        )
        self.assertRegex(
            self.text,
            r'Write-Host\s+"AERO_VIRTIO_WIN7_HOST\|VIRTIO_INPUT_EVENTS_INJECT\|FAIL\|attempt=\$Attempt\|backend=\$backend\|reason=\$reason"',
        )

    def test_media_keys_inject_markers_exist_and_do_not_include_qcode_field(self) -> None:
        self.assertRegex(
            self.text,
            r'Write-Host\s+"AERO_VIRTIO_WIN7_HOST\|VIRTIO_INPUT_MEDIA_KEYS_INJECT\|PASS\|attempt=\$Attempt\|backend=\$backend\|kbd_mode=\$kbdMode"',
        )
        self.assertRegex(
            self.text,
            r'Write-Host\s+"AERO_VIRTIO_WIN7_HOST\|VIRTIO_INPUT_MEDIA_KEYS_INJECT\|FAIL\|attempt=\$Attempt\|backend=\$backend\|reason=\$reason"',
        )

        # The host marker is a stable log-scraping contract; the injected qcode is an implementation detail.
        for m in re.finditer(
            r'Write-Host\s+"(?P<marker>AERO_VIRTIO_WIN7_HOST\|VIRTIO_INPUT_MEDIA_KEYS_INJECT\|[^"]+)"',
            self.text,
        ):
            marker = m.group("marker")
            self.assertNotIn("qcode=", marker, "media-keys inject marker must not include qcode fields")

    def test_tablet_events_inject_markers_exist(self) -> None:
        self.assertRegex(
            self.text,
            r'Write-Host\s+"AERO_VIRTIO_WIN7_HOST\|VIRTIO_INPUT_TABLET_EVENTS_INJECT\|PASS\|attempt=\$Attempt\|backend=\$backend\|tablet_mode=\$tabletMode"',
        )
        self.assertRegex(
            self.text,
            r'Write-Host\s+"AERO_VIRTIO_WIN7_HOST\|VIRTIO_INPUT_TABLET_EVENTS_INJECT\|FAIL\|attempt=\$Attempt\|backend=\$backend\|reason=\$reason"',
        )


if __name__ == "__main__":
    unittest.main()
