#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class Win7VirtioHarnessWorkflowBlkResizeMarkerLineTests(unittest.TestCase):
    def setUp(self) -> None:
        # Keep this test as a simple text scan (no PyYAML dependency).
        repo_root = Path(__file__).resolve().parents[5]
        self.workflow_path = repo_root / ".github" / "workflows" / "win7-virtio-harness.yml"
        self.text = self.workflow_path.read_text(encoding="utf-8", errors="replace")

    def test_workflow_extracts_blk_resize_marker_line(self) -> None:
        self.assertIn('blk_resize_marker_line="$(', self.text)
        self.assertIn(
            "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|",
            self.text,
        )

    def test_workflow_summary_mentions_blk_resize_marker_line(self) -> None:
        self.assertIn("Guest virtio-blk-resize marker line", self.text)


if __name__ == "__main__":
    unittest.main()

