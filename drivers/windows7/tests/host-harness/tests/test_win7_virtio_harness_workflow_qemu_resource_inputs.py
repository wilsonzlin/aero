#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class Win7VirtioHarnessWorkflowQemuResourceInputsTests(unittest.TestCase):
    def setUp(self) -> None:
        # This test is intentionally a simple text scan (no PyYAML dependency).
        # __file__ is:
        #   drivers/windows7/tests/host-harness/tests/test_*.py
        # so repo root is parents[5] (../..../..../..../..).
        repo_root = Path(__file__).resolve().parents[5]
        self.workflow_path = repo_root / ".github" / "workflows" / "win7-virtio-harness.yml"
        self.text = self.workflow_path.read_text(encoding="utf-8", errors="replace")

    def test_workflow_plumbs_memory_mb(self) -> None:
        self.assertIn("memory_mb:", self.text)
        self.assertIn('memory_mb="${{ inputs.memory_mb }}"', self.text)
        self.assertIn('--memory-mb "${memory_mb}"', self.text)

    def test_workflow_plumbs_smp(self) -> None:
        self.assertIn("smp:", self.text)
        self.assertIn('smp="${{ inputs.smp }}"', self.text)
        self.assertIn('--smp "${smp}"', self.text)


if __name__ == "__main__":
    unittest.main()

