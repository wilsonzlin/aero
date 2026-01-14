#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class Win7VirtioHarnessWorkflowNoTabsTests(unittest.TestCase):
    def setUp(self) -> None:
        repo_root = Path(__file__).resolve().parents[5]
        self.workflow_path = repo_root / ".github" / "workflows" / "win7-virtio-harness.yml"
        self.text = self.workflow_path.read_text(encoding="utf-8", errors="replace")

    def test_workflow_has_no_tab_characters(self) -> None:
        # Tabs in YAML are easy to introduce accidentally and are hard to see in diffs,
        # but can break indentation-sensitive parsing and make formatting inconsistent.
        self.assertNotIn("\t", self.text)


if __name__ == "__main__":
    unittest.main()

