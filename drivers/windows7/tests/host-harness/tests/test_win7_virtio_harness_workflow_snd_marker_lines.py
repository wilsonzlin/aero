#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class Win7VirtioHarnessWorkflowSndMarkerLineTests(unittest.TestCase):
    def setUp(self) -> None:
        # Keep this test as a simple text scan (no PyYAML dependency).
        repo_root = Path(__file__).resolve().parents[5]
        self.workflow_path = repo_root / ".github" / "workflows" / "win7-virtio-harness.yml"
        self.text = self.workflow_path.read_text(encoding="utf-8", errors="replace")

    def test_workflow_extracts_snd_marker_line(self) -> None:
        self.assertIn('snd_marker_line="$(', self.text)
        self.assertIn(
            "AERO_VIRTIO_SELFTEST|TEST|virtio-snd|",
            self.text,
        )
        self.assertIn("Guest virtio-snd marker line", self.text)

    def test_workflow_extracts_snd_capture_marker_line(self) -> None:
        self.assertIn('snd_capture_marker_line="$(', self.text)
        self.assertIn(
            "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|",
            self.text,
        )
        self.assertIn("Guest virtio-snd-capture marker line", self.text)

    def test_workflow_extracts_snd_duplex_marker_line(self) -> None:
        self.assertIn('snd_duplex_marker_line="$(', self.text)
        self.assertIn(
            "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|",
            self.text,
        )
        self.assertIn("Guest virtio-snd-duplex marker line", self.text)

    def test_workflow_extracts_snd_buffer_limits_marker_line(self) -> None:
        self.assertIn('snd_buffer_limits_marker_line="$(', self.text)
        self.assertIn(
            "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|",
            self.text,
        )
        self.assertIn("Guest virtio-snd-buffer-limits marker line", self.text)


if __name__ == "__main__":
    unittest.main()

