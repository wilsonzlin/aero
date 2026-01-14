#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class Win7VirtioHarnessWorkflowJobSummaryLogTailsTests(unittest.TestCase):
    def setUp(self) -> None:
        # This test is intentionally a simple text scan (no PyYAML dependency).
        # __file__ is:
        #   drivers/windows7/tests/host-harness/tests/test_*.py
        # so repo root is parents[5] (../..../..../..../..).
        repo_root = Path(__file__).resolve().parents[5]
        self.workflow_path = repo_root / ".github" / "workflows" / "win7-virtio-harness.yml"
        self.text = self.workflow_path.read_text(encoding="utf-8", errors="replace")

    def test_job_summary_includes_log_tails_on_failure(self) -> None:
        # The workflow should embed useful, collapsible log tails in the job summary when the
        # harness step fails so debugging does not require downloading the artifact.
        self.assertIn('if [[ "${{ steps.harness.outcome }}" != "success" ]]; then', self.text)

        self.assertIn("<details><summary>harness.log (tail)</summary>", self.text)
        self.assertIn('tail -n 200 "${harness_log}"', self.text)

        self.assertIn("<details><summary>serial.log (tail)</summary>", self.text)
        self.assertIn('tail -n 200 "${serial_log}"', self.text)

        self.assertIn("<details><summary>qemu.stderr.log (tail)</summary>", self.text)
        self.assertIn('tail -n 200 "${qemu_stderr_log}"', self.text)


if __name__ == "__main__":
    unittest.main()

