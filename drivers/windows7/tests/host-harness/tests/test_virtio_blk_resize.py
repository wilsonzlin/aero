#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import importlib.util
import json
import sys
import unittest
from pathlib import Path


def _load_harness():
    harness_path = Path(__file__).resolve().parents[1] / "invoke_aero_virtio_win7_tests.py"
    spec = importlib.util.spec_from_file_location("invoke_aero_virtio_win7_tests", harness_path)
    assert spec and spec.loader

    module = importlib.util.module_from_spec(spec)
    # Register the module before execution so dataclasses can resolve __module__ correctly.
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class VirtioBlkResizeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_extracts_ready_marker(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|READY|disk=2|old_bytes=1073741824\n"
        info = h._try_extract_virtio_blk_resize_ready(tail)
        self.assertIsNotNone(info)
        assert info is not None
        self.assertEqual(info.disk, 2)
        self.assertEqual(info.old_bytes, 1073741824)

    def test_compute_new_bytes_grows_and_aligns(self) -> None:
        h = self.harness
        # Non-sector-aligned delta should be rounded up.
        new_bytes = h._virtio_blk_resize_compute_new_bytes(1000, 1)
        self.assertGreater(new_bytes, 1000)
        self.assertEqual(new_bytes % 512, 0)
        self.assertEqual(new_bytes, 1024)

        # Typical harness delta: 64 MiB.
        old = 1024 * 1024 * 1024
        delta = 64 * 1024 * 1024
        self.assertEqual(h._virtio_blk_resize_compute_new_bytes(old, delta), old + delta)

    def test_builds_qmp_resize_commands(self) -> None:
        h = self.harness
        cmd = h._qmp_blockdev_resize_command(node_name="drive0", size=1234)
        self.assertEqual(cmd["execute"], "blockdev-resize")
        self.assertEqual(cmd["arguments"]["node-name"], "drive0")
        self.assertEqual(cmd["arguments"]["size"], 1234)
        json.dumps(cmd)  # must be JSON serializable

        cmd2 = h._qmp_block_resize_command(device="drive0", size=5678)
        self.assertEqual(cmd2["execute"], "block_resize")
        self.assertEqual(cmd2["arguments"]["device"], "drive0")
        self.assertEqual(cmd2["arguments"]["size"], 5678)
        json.dumps(cmd2)


if __name__ == "__main__":
    unittest.main()

