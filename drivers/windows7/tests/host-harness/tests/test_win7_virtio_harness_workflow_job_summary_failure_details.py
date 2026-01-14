#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class Win7VirtioHarnessWorkflowJobSummaryFailureDetailsTests(unittest.TestCase):
    def setUp(self) -> None:
        # Keep this test as a simple text scan (no PyYAML dependency).
        repo_root = Path(__file__).resolve().parents[5]
        self.workflow_path = repo_root / ".github" / "workflows" / "win7-virtio-harness.yml"
        self.text = self.workflow_path.read_text(encoding="utf-8", errors="replace")

    def test_job_summary_always_surfaces_failure_line_when_present(self) -> None:
        # The job summary should print both the structured token and the last full `FAIL:` line
        # (the full line often includes counter values and other context).
        self.assertNotIn(
            'if [[ -z "${failure_token}" && -n "${failure_line}" ]]; then',
            self.text,
        )
        self.assertIn('if [[ -n "${failure_line}" ]]; then', self.text)
        self.assertIn('echo "- Harness failure: \\`${failure_line}\\`"', self.text)

    def test_job_summary_surfaces_error_line_even_on_structured_failures(self) -> None:
        # When a structured failure token exists, we still want to surface the last explicit ERROR
        # line for quick debugging.
        self.assertNotIn(
            'if [[ -z "${failure_token}" && -z "${failure_line}" && -n "${error_line}" ]]; then',
            self.text,
        )
        self.assertIn('if [[ -n "${error_line}" ]]; then', self.text)
        self.assertIn('echo "- Harness error: \\`${error_line}\\`"', self.text)


if __name__ == "__main__":
    unittest.main()
