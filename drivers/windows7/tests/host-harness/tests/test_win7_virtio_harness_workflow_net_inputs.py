#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class Win7VirtioHarnessWorkflowNetInputsTests(unittest.TestCase):
    def setUp(self) -> None:
        # This test is intentionally a simple text scan (no PyYAML dependency).
        # __file__ is:
        #   drivers/windows7/tests/host-harness/tests/test_*.py
        # so repo root is parents[5] (../..../..../..../..).
        repo_root = Path(__file__).resolve().parents[5]
        self.workflow_path = repo_root / ".github" / "workflows" / "win7-virtio-harness.yml"
        self.text = self.workflow_path.read_text(encoding="utf-8", errors="replace")

    def test_workflow_plumbs_with_blk_reset(self) -> None:
        self.assertIn("with_blk_reset:", self.text)
        self.assertIn('with_blk_reset="${{ inputs.with_blk_reset }}"', self.text)
        self.assertIn("args+=(--with-blk-reset)", self.text)

    def test_workflow_plumbs_with_net_link_flap(self) -> None:
        self.assertIn("with_net_link_flap:", self.text)
        self.assertIn('with_net_link_flap="${{ inputs.with_net_link_flap }}"', self.text)
        self.assertIn("args+=(--with-net-link-flap)", self.text)

    def test_workflow_plumbs_require_net_csum_offload(self) -> None:
        self.assertIn("require_net_csum_offload:", self.text)
        self.assertIn('require_net_csum_offload="${{ inputs.require_net_csum_offload }}"', self.text)
        self.assertIn("args+=(--require-net-csum-offload)", self.text)


if __name__ == "__main__":
    unittest.main()

