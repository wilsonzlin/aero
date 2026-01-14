#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class Win7VirtioHarnessWorkflowForceIntxTests(unittest.TestCase):
    def setUp(self) -> None:
        # This test is intentionally a simple text scan (no PyYAML dependency).
        # __file__ is:
        #   drivers/windows7/tests/host-harness/tests/test_*.py
        # so repo root is parents[5] (../..../..../..../..).
        repo_root = Path(__file__).resolve().parents[5]
        self.workflow_path = repo_root / ".github" / "workflows" / "win7-virtio-harness.yml"
        self.text = self.workflow_path.read_text(encoding="utf-8", errors="replace")

    def test_workflow_declares_input(self) -> None:
        self.assertIn("force_intx:", self.text)

    def test_workflow_passes_force_intx_flag(self) -> None:
        self.assertIn('force_intx="${{ inputs.force_intx }}"', self.text)
        self.assertIn("args+=(--force-intx)", self.text)


if __name__ == "__main__":
    unittest.main()

