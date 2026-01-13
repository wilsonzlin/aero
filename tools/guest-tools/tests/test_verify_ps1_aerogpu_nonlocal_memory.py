#!/usr/bin/env python3

import re
import unittest
from pathlib import Path


class VerifyPs1AeroGpuNonLocalMemoryStaticTests(unittest.TestCase):
    def test_verify_ps1_surfaces_nonlocalmemorysize_setting(self) -> None:
        repo_root = Path(__file__).resolve().parents[3]
        verify_ps1 = repo_root / "guest-tools" / "verify.ps1"
        text = verify_ps1.read_text(encoding="utf-8", errors="replace")

        # The script should mention the registry value name and expose it in report.json.
        self.assertIn("NonLocalMemorySizeMB", text)

        # Ensure the stable JSON field name exists in the report schema.
        self.assertRegex(
            text,
            re.compile(
                r"(?ims)\baerogpu\s*=\s*@\{[\s\S]*?\bnon_local_memory_size_mb\s*=",
            ),
        )


if __name__ == "__main__":
    unittest.main()

