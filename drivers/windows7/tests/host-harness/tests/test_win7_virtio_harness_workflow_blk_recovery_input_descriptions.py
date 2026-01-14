#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class Win7VirtioHarnessWorkflowBlkRecoveryInputDescriptionsTests(unittest.TestCase):
    def setUp(self) -> None:
        # Keep this test as a simple text scan (no PyYAML dependency).
        repo_root = Path(__file__).resolve().parents[5]
        self.workflow_path = repo_root / ".github" / "workflows" / "win7-virtio-harness.yml"
        self.text = self.workflow_path.read_text(encoding="utf-8", errors="replace")

    def test_fail_on_blk_recovery_input_description_mentions_skip_no_fallback(self) -> None:
        # The workflow dispatch UI should document the key backcompat behavior:
        # - prefer virtio-blk-counters
        # - fallback to legacy *_srb fields only if the marker is missing entirely
        # - do not fall back when the marker is present but SKIP
        self.assertIn("fail_on_blk_recovery:", self.text)
        self.assertIn("prefers virtio-blk-counters", self.text)
        self.assertIn("falls back to legacy", self.text)
        self.assertIn("does not fall back when virtio-blk-counters is present but SKIP", self.text)

    def test_blk_reset_recovery_inputs_document_warn_skip_unavailable(self) -> None:
        # Reset-recovery gating can fall back to legacy miniport diagnostics, but WARN/SKIP
        # should be treated as unavailable (best-effort diagnostics).
        self.assertIn("require_no_blk_reset_recovery:", self.text)
        self.assertIn("fail_on_blk_reset_recovery:", self.text)
        self.assertIn("WARN/SKIP treated as unavailable", self.text)
        self.assertIn("virtio-blk-miniport-reset-recovery|INFO|", self.text)


if __name__ == "__main__":
    unittest.main()

