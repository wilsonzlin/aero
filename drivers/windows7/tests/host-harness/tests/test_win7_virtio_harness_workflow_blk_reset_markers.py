#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class Win7VirtioHarnessWorkflowBlkResetMarkerTests(unittest.TestCase):
    def setUp(self) -> None:
        # Keep this test as a simple text scan (no PyYAML dependency).
        repo_root = Path(__file__).resolve().parents[5]
        self.workflow_path = repo_root / ".github" / "workflows" / "win7-virtio-harness.yml"
        self.text = self.workflow_path.read_text(encoding="utf-8", errors="replace")

    def test_workflow_extracts_blk_reset_host_marker(self) -> None:
        self.assertIn(
            "blk_reset_host_marker=\"$(grep -F 'AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESET|'",
            self.text,
        )

    def test_workflow_extracts_blk_reset_recovery_host_marker(self) -> None:
        # The workflow should surface the host-side marker mirroring the guest reset-recovery counters.
        self.assertIn(
            "blk_reset_recovery_host_marker=\"$(grep -F 'AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESET_RECOVERY|'",
            self.text,
        )

    def test_workflow_extracts_blk_reset_recovery_guest_marker(self) -> None:
        self.assertIn(
            "blk_reset_recovery_marker=\"$(",
            self.text,
        )
        self.assertIn(
            "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|",
            self.text,
        )

    def test_workflow_extracts_blk_reset_guest_marker_line(self) -> None:
        self.assertIn(
            "blk_reset_marker_line=\"$(",
            self.text,
        )
        self.assertIn(
            "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|",
            self.text,
        )

    def test_workflow_summary_mentions_blk_reset_recovery_host_marker(self) -> None:
        self.assertIn(
            "Host virtio-blk-reset-recovery marker",
            self.text,
        )

    def test_workflow_summary_mentions_blk_reset_recovery_guest_marker(self) -> None:
        self.assertIn(
            "Guest virtio-blk-reset-recovery marker",
            self.text,
        )

    def test_workflow_summary_mentions_blk_reset_marker_line(self) -> None:
        self.assertIn(
            "Guest virtio-blk-reset marker line",
            self.text,
        )


if __name__ == "__main__":
    unittest.main()
