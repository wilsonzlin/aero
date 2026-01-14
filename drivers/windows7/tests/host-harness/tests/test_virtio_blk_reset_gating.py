#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import importlib.util
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


class VirtioBlkResetGatingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_required_marker_pass(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|PASS|performed=1|counter_before=1|counter_after=2\n"
        self.assertIsNone(h._virtio_blk_reset_required_failure_message(tail))

    def test_required_marker_pass_via_saw_flag_even_if_tail_truncated(self) -> None:
        h = self.harness
        # The main harness loop uses a rolling tail buffer; this unit test ensures the helper honors
        # the tracked `saw_pass` flag so PASS cannot be lost due to tail truncation.
        tail = b"AERO_VIRTIO_SELFTEST|RESULT|PASS\n"
        self.assertIsNone(
            h._virtio_blk_reset_required_failure_message(
                tail,
                saw_pass=True,
            )
        )

    def test_required_marker_fail(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|FAIL|reason=query_after_reset_failed|err=5\n"
        msg = h._virtio_blk_reset_required_failure_message(tail)
        self.assertIsNotNone(msg)
        self.assertTrue(str(msg).startswith("FAIL: VIRTIO_BLK_RESET_FAILED:"))

    def test_required_marker_skip(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|SKIP|reason=not_supported\n"
        msg = h._virtio_blk_reset_required_failure_message(tail)
        self.assertIsNotNone(msg)
        self.assertTrue(str(msg).startswith("FAIL: VIRTIO_BLK_RESET_SKIPPED:"))

    def test_required_marker_missing(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|RESULT|PASS\n"
        msg = h._virtio_blk_reset_required_failure_message(tail)
        self.assertIsNotNone(msg)
        self.assertTrue(str(msg).startswith("FAIL: MISSING_VIRTIO_BLK_RESET:"))


if __name__ == "__main__":
    unittest.main()
