#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class Win7VirtioHarnessWorkflowBlkMiniportDiagLinesTests(unittest.TestCase):
    def setUp(self) -> None:
        # Keep this test as a simple text scan (no PyYAML dependency).
        repo_root = Path(__file__).resolve().parents[5]
        self.workflow_path = repo_root / ".github" / "workflows" / "win7-virtio-harness.yml"
        self.text = self.workflow_path.read_text(encoding="utf-8", errors="replace")

    def test_workflow_extracts_legacy_blk_miniport_diag_lines(self) -> None:
        # Surface best-effort legacy diagnostics in job summaries, since older guest selftests
        # may rely on these lines for blk reset-recovery fallbacks.
        self.assertIn('blk_miniport_flags_marker="$(', self.text)
        self.assertIn("grep -F 'virtio-blk-miniport-flags|'", self.text)

        self.assertIn('blk_miniport_reset_recovery_marker="$(', self.text)
        self.assertIn("grep -F 'virtio-blk-miniport-reset-recovery|'", self.text)

    def test_workflow_summary_mentions_legacy_blk_miniport_diag_lines(self) -> None:
        self.assertIn("Guest virtio-blk-miniport-flags diagnostic", self.text)
        self.assertIn("Guest virtio-blk-miniport-reset-recovery diagnostic", self.text)


if __name__ == "__main__":
    unittest.main()

