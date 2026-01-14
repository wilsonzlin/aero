#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class Win7VirtioHarnessWorkflowJobSummarySndMarkersTests(unittest.TestCase):
    def setUp(self) -> None:
        # This test is intentionally a simple text scan (no PyYAML dependency).
        # __file__ is:
        #   drivers/windows7/tests/host-harness/tests/test_*.py
        # so repo root is parents[5] (../..../..../..../..).
        repo_root = Path(__file__).resolve().parents[5]
        self.workflow_path = repo_root / ".github" / "workflows" / "win7-virtio-harness.yml"
        self.text = self.workflow_path.read_text(encoding="utf-8", errors="replace")

    def test_job_summary_surfaces_host_snd_markers(self) -> None:
        for marker in (
            "VIRTIO_SND|",
            "VIRTIO_SND_MSIX|",
            "VIRTIO_SND_CAPTURE|",
            "VIRTIO_SND_DUPLEX|",
            "VIRTIO_SND_BUFFER_LIMITS|",
            "VIRTIO_SND_FORMAT|",
            "VIRTIO_SND_EVENTQ|",
        ):
            with self.subTest(marker=marker):
                self.assertIn(marker, self.text)

        self.assertIn("Host virtio-snd marker", self.text)
        self.assertIn("Host virtio-snd MSI-X marker", self.text)
        self.assertIn("Host virtio-snd-capture marker", self.text)
        self.assertIn("Host virtio-snd-duplex marker", self.text)
        self.assertIn("Host virtio-snd-buffer-limits marker", self.text)
        self.assertIn("Host virtio-snd-format marker", self.text)
        self.assertIn("Host virtio-snd-eventq marker", self.text)


if __name__ == "__main__":
    unittest.main()
