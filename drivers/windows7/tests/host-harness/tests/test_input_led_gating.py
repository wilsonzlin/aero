#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import importlib.util
import re
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


_TOKEN_RE = re.compile(r"^FAIL: [A-Z0-9_]+:")


class VirtioInputLedGatingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_required_pass_marker_satisfies_gating(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|PASS|sent=2\n"
        self.assertIsNone(h._virtio_input_led_required_failure_message(tail))

    def test_required_fail_marker_fails_gating(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|FAIL|reason=timeout|err=1460\n"
        msg = h._virtio_input_led_required_failure_message(tail)
        assert msg is not None
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_INPUT_LED_FAILED:"))

    def test_required_skip_marker_fails_gating(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|SKIP|flag_not_set\n"
        msg = h._virtio_input_led_required_failure_message(tail)
        assert msg is not None
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_INPUT_LED_SKIPPED:"))

    def test_missing_marker_fails_gating(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|RESULT|PASS\n"
        msg = h._virtio_input_led_required_failure_message(tail)
        assert msg is not None
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: MISSING_VIRTIO_INPUT_LED:"))


if __name__ == "__main__":
    unittest.main()

