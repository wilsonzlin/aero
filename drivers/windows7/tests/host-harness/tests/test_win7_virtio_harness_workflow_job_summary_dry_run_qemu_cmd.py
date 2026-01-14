#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class Win7VirtioHarnessWorkflowJobSummaryDryRunQemuCmdTests(unittest.TestCase):
    def setUp(self) -> None:
        # This test is intentionally a simple text scan (no PyYAML dependency).
        # __file__ is:
        #   drivers/windows7/tests/host-harness/tests/test_*.py
        # so repo root is parents[5] (../..../..../..../..).
        repo_root = Path(__file__).resolve().parents[5]
        self.workflow_path = repo_root / ".github" / "workflows" / "win7-virtio-harness.yml"
        self.text = self.workflow_path.read_text(encoding="utf-8", errors="replace")

    def test_job_summary_parses_dry_run_qemu_cmdline(self) -> None:
        # In dry_run mode, the python harness prints a JSON argv array on the first stdout line
        # and a copy/paste command line on the second stdout line. The workflow should surface
        # that in the job summary.
        self.assertIn("dry_run_qemu_cmdline", self.text)
        self.assertIn("awk '/^\\[\"/{getline", self.text)
        self.assertIn(
            'if [[ "${{ inputs.dry_run }}" == "true" && -z "${qemu_cmdline}" && -n "${dry_run_qemu_cmdline}" ]]; then',
            self.text,
        )

        # Ensure the summary prints the QEMU and harness commands on dry_run success too.
        self.assertIn(
            'if [[ ( "${{ steps.harness.outcome }}" != "success" || "${{ inputs.dry_run }}" == "true" ) && -n "${harness_cmdline}" ]]; then',
            self.text,
        )
        self.assertIn(
            'if [[ ( "${{ steps.harness.outcome }}" != "success" || "${{ inputs.dry_run }}" == "true" ) && -n "${qemu_cmdline}" ]]; then',
            self.text,
        )


if __name__ == "__main__":
    unittest.main()
