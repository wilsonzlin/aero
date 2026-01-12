#!/usr/bin/env python3

import re
import unittest
from pathlib import Path


class UninstallCmdCleanupStorageStaticTests(unittest.TestCase):
    def test_usage_includes_cleanupstorage_option(self) -> None:
        repo_root = Path(__file__).resolve().parents[3]
        uninstall_cmd = repo_root / "guest-tools/uninstall.cmd"
        text = uninstall_cmd.read_text(encoding="utf-8", errors="replace")

        # Extract the /? help block between :usage and :fail.
        m = re.search(r"(?ims)^:usage\s*$([\s\S]*?)(?=^:fail\s*$)", text)
        self.assertIsNotNone(m, f"failed to locate :usage block in {uninstall_cmd}")
        usage = m.group(1).lower()

        self.assertIn("/cleanupstorage", usage)
        self.assertIn("/cleanup-storage", usage)

    def test_references_expected_registry_paths(self) -> None:
        repo_root = Path(__file__).resolve().parents[3]
        uninstall_cmd = repo_root / "guest-tools/uninstall.cmd"
        text = uninstall_cmd.read_text(encoding="utf-8", errors="replace").lower()

        # Ensure the script references the relevant registry surfaces touched by
        # setup.cmd's boot-critical virtio-blk preseed.
        self.assertIn("criticaldevicedatabase", text)
        self.assertIn(r"currentcontrolset\services".lower(), text)


if __name__ == "__main__":
    unittest.main()

