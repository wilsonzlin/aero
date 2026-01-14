#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class GuestSelftestInputSubtestErrCodesTests(unittest.TestCase):
    def setUp(self) -> None:
        cpp_path = (
            Path(__file__).resolve().parents[2] / "guest-selftest" / "src" / "main.cpp"
        )
        self.text = cpp_path.read_text(encoding="utf-8", errors="replace")

    def test_input_events_timeout_sets_error_timeout(self) -> None:
        # virtio-input-events (and the extended event variants) output `err=...` on FAIL.
        # When the failure reason is a timeout (no reports observed), surface a non-zero error
        # code so the CI token is actionable.
        self.assertRegex(
            self.text,
            r'(?s)static VirtioInputEventsTestResult VirtioInputEventsTest.*?'
            r'out\.reason = "timeout";\s*'
            r'if \(out\.win32_error == ERROR_SUCCESS\) out\.win32_error = ERROR_TIMEOUT;',
        )

    def test_media_keys_missing_consumer_sets_error_not_found(self) -> None:
        self.assertRegex(
            self.text,
            r'out\.reason = "missing_consumer_device";\s*out\.win32_error = ERROR_NOT_FOUND;',
        )

    def test_media_keys_timeout_sets_error_timeout(self) -> None:
        # Ensure the virtio-input-media-keys timeout path sets a non-zero error code.
        self.assertRegex(
            self.text,
            r'(?s)static VirtioInputMediaKeysTestResult VirtioInputMediaKeysTest.*?'
            r'out\.reason = "timeout";\s*'
            r'if \(out\.win32_error == ERROR_SUCCESS\) out\.win32_error = ERROR_TIMEOUT;',
        )

    def test_tablet_events_sets_nonzero_errors_for_common_failures(self) -> None:
        # These failure reasons feed the virtio-input-tablet-events marker (which includes err=...).
        self.assertRegex(
            self.text,
            r'out\.reason = "missing_tablet_device";\s*out\.win32_error = ERROR_NOT_FOUND;',
        )
        self.assertRegex(
            self.text,
            r'out\.reason = "unsupported_report_descriptor";\s*out\.win32_error = ERROR_NOT_SUPPORTED;',
        )
        self.assertRegex(
            self.text,
            r'out\.reason = "unexpected_report_id";\s*out\.win32_error = ERROR_INVALID_DATA;',
        )


if __name__ == "__main__":
    unittest.main()
