#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class Win7VirtioHarnessWorkflowMsixGuestMarkerLineTests(unittest.TestCase):
    def setUp(self) -> None:
        # Keep this test as a simple text scan (no PyYAML dependency).
        repo_root = Path(__file__).resolve().parents[5]
        self.workflow_path = repo_root / ".github" / "workflows" / "win7-virtio-harness.yml"
        self.text = self.workflow_path.read_text(encoding="utf-8", errors="replace")

    def test_workflow_extracts_guest_msix_marker_lines(self) -> None:
        # Surface guest msix markers in job summaries to help debug interrupt mode mismatches.
        for var, marker in (
            ("blk_msix_marker_line", "virtio-blk-msix"),
            ("net_msix_marker_line", "virtio-net-msix"),
            ("snd_msix_marker_line", "virtio-snd-msix"),
            ("input_msix_marker_line", "virtio-input-msix"),
        ):
            with self.subTest(var=var, marker=marker):
                self.assertIn(f'{var}="$(', self.text)
                self.assertIn(f"AERO_VIRTIO_SELFTEST|TEST|{marker}|", self.text)

    def test_workflow_summary_mentions_guest_msix_marker_lines(self) -> None:
        for label in (
            "Guest virtio-blk-msix marker line",
            "Guest virtio-net-msix marker line",
            "Guest virtio-snd-msix marker line",
            "Guest virtio-input-msix marker line",
        ):
            with self.subTest(label=label):
                self.assertIn(label, self.text)


if __name__ == "__main__":
    unittest.main()

