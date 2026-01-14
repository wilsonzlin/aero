#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path
import sys


def _load_harness():
    harness_path = Path(__file__).resolve().parents[1] / "invoke_aero_virtio_win7_tests.py"
    spec = importlib.util.spec_from_file_location("invoke_aero_virtio_win7_tests", harness_path)
    assert spec and spec.loader

    module = importlib.util.module_from_spec(spec)
    # Register the module before execution so dataclasses can resolve __module__ correctly.
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class VirtioInputLedsGatingSawFlagsTests(unittest.TestCase):
    """
    test_failure_tokens.py covers tail-scanning behavior (PASS/FAIL/SKIP/missing marker).

    This file focuses on the additional `saw_*` boolean flags that the main harness loop passes into
    the helper to survive rolling-tail truncation.
    """

    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_saw_fail_flag_fails_even_if_tail_lacks_marker(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|RESULT|PASS\n"
        msg = h._virtio_input_leds_required_failure_message(tail, saw_fail=True)
        self.assertIsNotNone(msg)
        self.assertTrue(str(msg).startswith("FAIL: VIRTIO_INPUT_LEDS_FAILED:"))

    def test_saw_skip_flag_fails_even_if_tail_lacks_marker(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|RESULT|PASS\n"
        msg = h._virtio_input_leds_required_failure_message(tail, saw_skip=True)
        self.assertIsNotNone(msg)
        self.assertTrue(str(msg).startswith("FAIL: VIRTIO_INPUT_LEDS_SKIPPED:"))

    def test_saw_pass_takes_precedence_over_saw_fail(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|RESULT|PASS\n"
        # The helper is used as a final gate when RESULT=PASS; if we saw PASS earlier, allow it even
        # if the rolling tail lost the marker.
        self.assertIsNone(
            h._virtio_input_leds_required_failure_message(tail, saw_pass=True, saw_fail=True)
        )


if __name__ == "__main__":
    unittest.main()

