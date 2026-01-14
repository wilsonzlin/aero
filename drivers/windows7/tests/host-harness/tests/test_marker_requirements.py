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


class MarkerRequirementTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_virtio_input_bind_marker_required_in_strict_mode(self) -> None:
        h = self.harness
        tok = h._check_required_virtio_input_bind_marker(
            require_per_test_markers=True,
            saw_pass=False,
            saw_fail=False,
        )
        self.assertEqual(tok, "MISSING_VIRTIO_INPUT_BIND")

    def test_virtio_input_bind_marker_not_required_in_transitional_mode(self) -> None:
        h = self.harness
        tok = h._check_required_virtio_input_bind_marker(
            require_per_test_markers=False,
            saw_pass=False,
            saw_fail=False,
        )
        self.assertIsNone(tok)

    def test_virtio_input_bind_fail_propagates_in_strict_mode(self) -> None:
        h = self.harness
        tok = h._check_required_virtio_input_bind_marker(
            require_per_test_markers=True,
            saw_pass=True,
            saw_fail=True,
        )
        self.assertEqual(tok, "VIRTIO_INPUT_BIND_FAILED")

    def test_virtio_input_bind_pass_ok(self) -> None:
        h = self.harness
        tok = h._check_required_virtio_input_bind_marker(
            require_per_test_markers=True,
            saw_pass=True,
            saw_fail=False,
        )
        self.assertIsNone(tok)


if __name__ == "__main__":
    unittest.main()

