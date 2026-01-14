#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class Win7VirtioHarnessWorkflowJobSummaryStrictBashTests(unittest.TestCase):
    def setUp(self) -> None:
        repo_root = Path(__file__).resolve().parents[5]
        self.workflow_path = repo_root / ".github" / "workflows" / "win7-virtio-harness.yml"
        self.text = self.workflow_path.read_text(encoding="utf-8", errors="replace")

    def test_job_summary_step_uses_strict_bash(self) -> None:
        # Ensure the Job summary step runs with strict bash settings, so unexpected failures
        # or uninitialized variables don't silently produce partial summaries.
        idx = self.text.index("- name: Job summary")
        window = self.text[idx : idx + 2000]
        self.assertIn("set -euo pipefail", window)


if __name__ == "__main__":
    unittest.main()

