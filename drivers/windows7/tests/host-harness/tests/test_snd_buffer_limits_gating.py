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


class VirtioSndBufferLimitsGatingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_required_marker_pass(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|PASS|mode=large\n"
        self.assertIsNone(h._virtio_snd_buffer_limits_required_failure_message(tail))

    def test_required_marker_fail(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|FAIL|reason=hr|hr=0x80004005\n"
        msg = h._virtio_snd_buffer_limits_required_failure_message(tail)
        self.assertIsNotNone(msg)
        self.assertTrue(str(msg).startswith("FAIL: VIRTIO_SND_BUFFER_LIMITS_FAILED:"))

    def test_required_marker_skip_flag_not_set(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|SKIP|flag_not_set\n"
        msg = h._virtio_snd_buffer_limits_required_failure_message(tail)
        self.assertIsNotNone(msg)
        self.assertTrue(str(msg).startswith("FAIL: VIRTIO_SND_BUFFER_LIMITS_SKIPPED:"))

    def test_required_marker_missing(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|RESULT|PASS\n"
        msg = h._virtio_snd_buffer_limits_required_failure_message(tail)
        self.assertIsNotNone(msg)
        self.assertTrue(str(msg).startswith("FAIL: MISSING_VIRTIO_SND_BUFFER_LIMITS:"))


if __name__ == "__main__":
    unittest.main()

