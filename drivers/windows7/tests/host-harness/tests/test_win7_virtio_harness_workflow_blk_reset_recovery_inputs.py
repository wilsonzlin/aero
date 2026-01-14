#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class Win7VirtioHarnessWorkflowBlkResetRecoveryInputsTests(unittest.TestCase):
    def setUp(self) -> None:
        # This test is intentionally a simple text scan (no PyYAML dependency).
        # __file__ is:
        #   drivers/windows7/tests/host-harness/tests/test_*.py
        # so repo root is parents[5] (../..../..../..../..).
        repo_root = Path(__file__).resolve().parents[5]
        self.workflow_path = repo_root / ".github" / "workflows" / "win7-virtio-harness.yml"
        self.text = self.workflow_path.read_text(encoding="utf-8", errors="replace")

    def test_workflow_plumbs_require_no_blk_reset_recovery(self) -> None:
        self.assertIn("require_no_blk_reset_recovery:", self.text)
        self.assertIn(
            'require_no_blk_reset_recovery="${{ inputs.require_no_blk_reset_recovery }}"', self.text
        )
        self.assertIn("args+=(--require-no-blk-reset-recovery)", self.text)

    def test_workflow_plumbs_fail_on_blk_reset_recovery(self) -> None:
        self.assertIn("fail_on_blk_reset_recovery:", self.text)
        self.assertIn(
            'fail_on_blk_reset_recovery="${{ inputs.fail_on_blk_reset_recovery }}"', self.text
        )
        self.assertIn("args+=(--fail-on-blk-reset-recovery)", self.text)


if __name__ == "__main__":
    unittest.main()

