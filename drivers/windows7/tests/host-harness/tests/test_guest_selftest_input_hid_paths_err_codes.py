#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class GuestSelftestInputHidPathsErrCodesTests(unittest.TestCase):
    def setUp(self) -> None:
        cpp_path = (
            Path(__file__).resolve().parents[2] / "guest-selftest" / "src" / "main.cpp"
        )
        self.text = cpp_path.read_text(encoding="utf-8", errors="replace")

    def test_missing_keyboard_reason_sets_nonzero_error(self) -> None:
        # FindVirtioInputHidPaths() is used by virtio-input-* markers that include `err=...`.
        # When no keyboard HID interface is detected, ensure we set a non-zero error code for
        # diagnosability (instead of leaving `err=0`).
        self.assertRegex(
            self.text,
            r'out\.reason = "missing_keyboard_device";\s*out\.win32_error = ERROR_NOT_FOUND;',
        )

    def test_missing_mouse_reason_sets_nonzero_error(self) -> None:
        self.assertRegex(
            self.text,
            r'out\.reason = "missing_mouse_device";\s*out\.win32_error = ERROR_NOT_FOUND;',
        )

    def test_ioctl_or_open_failed_reason_sets_nonzero_error(self) -> None:
        # Ensure we populate an error code when enumeration/open/descriptor IOCTLs failed.
        self.assertRegex(
            self.text,
            r'out\.reason = "ioctl_or_open_failed";\s*out\.win32_error = \(',
        )


if __name__ == "__main__":
    unittest.main()

