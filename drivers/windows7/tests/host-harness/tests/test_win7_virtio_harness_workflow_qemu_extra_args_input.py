#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class Win7VirtioHarnessWorkflowQemuExtraArgsInputTests(unittest.TestCase):
    def setUp(self) -> None:
        # This test is intentionally a simple text scan (no PyYAML dependency).
        # __file__ is:
        #   drivers/windows7/tests/host-harness/tests/test_*.py
        # so repo root is parents[5] (../..../..../..../..).
        repo_root = Path(__file__).resolve().parents[5]
        self.workflow_path = repo_root / ".github" / "workflows" / "win7-virtio-harness.yml"
        self.text = self.workflow_path.read_text(encoding="utf-8", errors="replace")

    def test_workflow_plumbs_qemu_extra_args(self) -> None:
        self.assertIn("qemu_extra_args:", self.text)
        self.assertIn('qemu_extra_args="${{ inputs.qemu_extra_args }}"', self.text)
        # Ensure we forward after `--` so argparse does not eat QEMU flags like --help/-h.
        self.assertIn("args+=(--)", self.text)
        # Ensure CRLF pasted input is handled robustly.
        self.assertIn("line=\"${line%$'\\r'}\"", self.text)


if __name__ == "__main__":
    unittest.main()
