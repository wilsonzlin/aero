#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class ReadmeWorkflowJobSummaryMentionBlkDiagnosticsTests(unittest.TestCase):
    def test_job_summary_mentions_blk_counters_and_miniport_diagnostics(self) -> None:
        readme_path = Path(__file__).resolve().parents[1] / "README.md"
        readme = readme_path.read_text(encoding="utf-8", errors="replace")

        job_summary_line: str | None = None
        for line in readme.splitlines():
            if line.strip().startswith("- Job summary:"):
                job_summary_line = line
                break

        self.assertIsNotNone(job_summary_line, "README missing the GitHub Actions 'Job summary' bullet")
        assert job_summary_line is not None

        # The workflow job summary step surfaces these guest-side diagnostics for quick debugging.
        self.assertIn("virtio-blk-counters", job_summary_line)
        self.assertIn("miniport", job_summary_line.lower())
        self.assertIn("diag", job_summary_line.lower())


if __name__ == "__main__":
    unittest.main()

