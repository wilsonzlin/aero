#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class ProbeQemuVirtioPciIdsQemuSystemDirectoryValidationTests(unittest.TestCase):
    def setUp(self) -> None:
        script_path = Path(__file__).resolve().parents[1] / "probe_qemu_virtio_pci_ids.py"
        self.text = script_path.read_text(encoding="utf-8", errors="replace")

    def test_rejects_directory_qemu_system_paths(self) -> None:
        self.assertIn("qemu system binary path is a directory", self.text)
        self.assertIn("os.sep in args.qemu_system", self.text)
        self.assertIn("Path(args.qemu_system)", self.text)


if __name__ == "__main__":
    unittest.main()

