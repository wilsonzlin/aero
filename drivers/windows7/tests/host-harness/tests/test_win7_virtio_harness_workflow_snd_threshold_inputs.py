#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class Win7VirtioHarnessWorkflowSndThresholdInputsTests(unittest.TestCase):
    def setUp(self) -> None:
        # Keep this test as a simple text scan (no PyYAML dependency).
        repo_root = Path(__file__).resolve().parents[5]
        self.workflow_path = repo_root / ".github" / "workflows" / "win7-virtio-harness.yml"
        self.text = self.workflow_path.read_text(encoding="utf-8", errors="replace")

    def test_workflow_validates_snd_wav_threshold_inputs(self) -> None:
        # Inputs should be validated early (before launching QEMU) with clear errors.
        self.assertIn("virtio_snd_wav_peak_threshold", self.text)
        self.assertIn("virtio_snd_wav_rms_threshold", self.text)
        self.assertIn("workflow input virtio_snd_wav_peak_threshold requires with_virtio_snd=true", self.text)
        self.assertIn("workflow input virtio_snd_wav_rms_threshold requires with_virtio_snd=true", self.text)
        self.assertIn("virtio_snd_wav_peak_threshold must be a non-negative integer", self.text)
        self.assertIn("virtio_snd_wav_rms_threshold must be a non-negative integer", self.text)

        # And ensure the harness argv plumbing remains intact.
        self.assertIn("--virtio-snd-wav-peak-threshold", self.text)
        self.assertIn("--virtio-snd-wav-rms-threshold", self.text)


if __name__ == "__main__":
    unittest.main()

