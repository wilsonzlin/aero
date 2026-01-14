#!/usr/bin/env python3

import unittest
from pathlib import Path


class GuestToolsReadmeDbgctlPathTests(unittest.TestCase):
    def test_readme_mentions_current_dbgctl_path(self) -> None:
        repo_root = Path(__file__).resolve().parents[3]
        readme = repo_root / "guest-tools" / "README.md"
        text = readme.read_text(encoding="utf-8", errors="replace")

        # Canonical shipped location (matches Guest Tools media layout).
        self.assertIn(
            r"drivers\<arch>\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe",
            text,
        )

        # The -RunDbgctl section should show the current invocation with a timeout.
        self.assertIn(
            r"drivers\<arch>\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe --status --timeout-ms 2000",
            text,
        )

        # Guard against regressions back to the legacy doc path.
        self.assertNotIn(
            r"drivers\<arch>\aerogpu\tools\aerogpu_dbgctl.exe",
            text,
        )


if __name__ == "__main__":
    unittest.main()

