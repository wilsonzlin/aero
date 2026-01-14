#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class Win7VirtioHarnessWorkflowJobSummaryNoteLineTests(unittest.TestCase):
    def setUp(self) -> None:
        # Keep this test as a simple text scan (no PyYAML dependency).
        repo_root = Path(__file__).resolve().parents[5]
        self.workflow_path = repo_root / ".github" / "workflows" / "win7-virtio-harness.yml"
        self.text = self.workflow_path.read_text(encoding="utf-8", errors="replace")

    def test_job_summary_extracts_note_line(self) -> None:
        self.assertIn("note_line=", self.text)
        self.assertIn("grep -F 'NOTE:'", self.text)

    def test_job_summary_surfaces_note_line(self) -> None:
        self.assertIn("Harness note (last line)", self.text)


if __name__ == "__main__":
    unittest.main()

