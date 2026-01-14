#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class Win7VirtioHarnessWorkflowInputMarkerLineTests(unittest.TestCase):
    def setUp(self) -> None:
        # Keep this test as a simple text scan (no PyYAML dependency).
        repo_root = Path(__file__).resolve().parents[5]
        self.workflow_path = repo_root / ".github" / "workflows" / "win7-virtio-harness.yml"
        self.text = self.workflow_path.read_text(encoding="utf-8", errors="replace")

    def test_workflow_extracts_input_events_marker_line(self) -> None:
        self.assertIn('input_events_marker_line="$(', self.text)
        self.assertIn(
            "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|",
            self.text,
        )
        self.assertIn("Guest virtio-input-events marker line", self.text)

    def test_workflow_extracts_input_wheel_marker_line(self) -> None:
        self.assertIn('input_wheel_marker_line="$(', self.text)
        self.assertIn(
            "AERO_VIRTIO_SELFTEST|TEST|virtio-input-wheel|",
            self.text,
        )
        self.assertIn("Guest virtio-input-wheel marker line", self.text)

    def test_workflow_extracts_input_media_keys_marker_line(self) -> None:
        self.assertIn('input_media_keys_marker_line="$(', self.text)
        self.assertIn(
            "AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|",
            self.text,
        )
        self.assertIn("Guest virtio-input-media-keys marker line", self.text)

    def test_workflow_extracts_input_tablet_events_marker_line(self) -> None:
        self.assertIn('input_tablet_marker_line="$(', self.text)
        self.assertIn(
            "AERO_VIRTIO_SELFTEST|TEST|virtio-input-tablet-events|",
            self.text,
        )
        self.assertIn("Guest virtio-input-tablet-events marker line", self.text)


if __name__ == "__main__":
    unittest.main()

