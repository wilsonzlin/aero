#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class Win7VirtioHarnessWorkflowDryRunInputTests(unittest.TestCase):
    def setUp(self) -> None:
        # This test is intentionally a simple text scan (no PyYAML dependency).
        # __file__ is:
        #   drivers/windows7/tests/host-harness/tests/test_*.py
        # so repo root is parents[5] (../..../..../..../..).
        repo_root = Path(__file__).resolve().parents[5]
        self.workflow_path = repo_root / ".github" / "workflows" / "win7-virtio-harness.yml"
        self.text = self.workflow_path.read_text(encoding="utf-8", errors="replace")

    def test_workflow_plumbs_dry_run(self) -> None:
        self.assertIn("dry_run:", self.text)
        self.assertIn('echo "  dry_run: \'${{ inputs.dry_run }}\'"', self.text)
        self.assertIn('dry_run="${{ inputs.dry_run }}"', self.text)
        self.assertIn("args+=(--dry-run)", self.text)
        # When dry_run is enabled, the workflow should not fail fast if the disk image path is missing.
        self.assertIn("WARNING: disk image path does not exist on the runner:", self.text)
        self.assertIn('if [[ "${dry_run}" == "true" ]]; then', self.text)


if __name__ == "__main__":
    unittest.main()
