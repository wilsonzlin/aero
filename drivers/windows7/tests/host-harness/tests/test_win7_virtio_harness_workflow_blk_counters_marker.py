#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class Win7VirtioHarnessWorkflowBlkCountersMarkerTests(unittest.TestCase):
    def setUp(self) -> None:
        # Keep this test as a simple text scan (no PyYAML dependency).
        repo_root = Path(__file__).resolve().parents[5]
        self.workflow_path = repo_root / ".github" / "workflows" / "win7-virtio-harness.yml"
        self.text = self.workflow_path.read_text(encoding="utf-8", errors="replace")

    def test_workflow_extracts_blk_counters_guest_marker(self) -> None:
        # Surface the last virtio-blk-counters guest marker in job summaries for debugging
        # reset/recovery regressions.
        self.assertIn('blk_counters_marker="$(', self.text)
        self.assertIn(
            "grep -F 'AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|'",
            self.text,
        )

    def test_workflow_summary_mentions_blk_counters_guest_marker(self) -> None:
        self.assertIn("Guest virtio-blk-counters marker", self.text)


if __name__ == "__main__":
    unittest.main()

