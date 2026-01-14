#!/usr/bin/env python3

import unittest
from pathlib import Path


class VerifyDbgctlOptionTests(unittest.TestCase):
    def test_verify_ps1_has_run_dbgctl_switch_and_expected_path(self) -> None:
        repo_root = Path(__file__).resolve().parents[3]
        verify_ps1 = repo_root / "guest-tools" / "verify.ps1"
        text = verify_ps1.read_text(encoding="utf-8", errors="replace")

        # Parameter name exists (PowerShell switch).
        self.assertIn("RunDbgctl", text)
        self.assertIn("[switch]$RunDbgctl", text)

        # Optional selftest switch exists.
        self.assertIn("RunDbgctlSelftest", text)
        self.assertIn("[switch]$RunDbgctlSelftest", text)

        # Expected packaged media path is referenced (template string used by the script).
        self.assertIn(
            r"drivers\<arch>\aerogpu\tools\win7_dbgctl\bin\aerogpu_dbgctl.exe",
            text,
        )

        # Legacy packaged location should remain in the fallback search list for
        # backwards compatibility with older Guest Tools media.
        self.assertIn(
            r"drivers\amd64\aerogpu\tools\aerogpu_dbgctl.exe",
            text,
        )
        self.assertIn(
            r"drivers\x86\aerogpu\tools\aerogpu_dbgctl.exe",
            text,
        )

        # Selftest invocation is referenced.
        self.assertIn("--selftest", text)


if __name__ == "__main__":
    unittest.main()
