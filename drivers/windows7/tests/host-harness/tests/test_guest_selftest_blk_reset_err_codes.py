#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class GuestSelftestVirtioBlkResetErrCodesTests(unittest.TestCase):
    def setUp(self) -> None:
        cpp_path = (
            Path(__file__).resolve().parents[2] / "guest-selftest" / "src" / "main.cpp"
        )
        self.text = cpp_path.read_text(encoding="utf-8", errors="replace")

    def test_blk_reset_prereq_fail_markers_do_not_hardcode_err_zero(self) -> None:
        # The virtio-blk-reset test is opt-in. When enabled, it can fail early before the
        # actual reset attempt (e.g. virtio-blk prereq test failed, or disk target resolution
        # failed). Those FAIL markers must include a non-zero err=... for diagnosability.
        self.assertNotIn(
            "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|FAIL|reason=blk_test_failed|err=0",
            self.text,
        )
        self.assertNotIn(
            "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|FAIL|reason=resolve_target_failed|err=0",
            self.text,
        )

    def test_open_physical_drive_preserves_last_error_for_callers(self) -> None:
        # Some callers (e.g. virtio-blk-resize) use GetLastError() after OpenPhysicalDriveForIoctl fails.
        # Ensure the helper restores the CreateFile() error code after logging so the marker err=... is not
        # clobbered to 0 by unrelated Win32/CRT calls inside the logger.
        self.assertRegex(
            self.text,
            r"(?s)virtio-blk: CreateFile\(PhysicalDrive%lu\) failed err=%lu.{0,200}SetLastError\(err\);",
        )


if __name__ == "__main__":
    unittest.main()
