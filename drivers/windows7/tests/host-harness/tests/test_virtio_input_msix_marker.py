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


class VirtioInputMsixMarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_parses_pass_marker(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|PASS|mode=msix|messages=3|mapping=per-queue|"
            b"config_vector=0|queue0_vector=1|queue1_vector=2\n"
        )
        marker = self.harness._parse_virtio_input_msix_marker(tail)
        self.assertIsNotNone(marker)
        assert marker is not None
        self.assertEqual(marker.status, "PASS")
        self.assertEqual(marker.fields.get("mode"), "msix")
        self.assertEqual(marker.fields.get("messages"), "3")

    def test_uses_last_marker(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|PASS|mode=intx|messages=0\n"
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|PASS|mode=msix|messages=3\n"
        )
        marker = self.harness._parse_virtio_input_msix_marker(tail)
        self.assertIsNotNone(marker)
        assert marker is not None
        self.assertEqual(marker.fields.get("mode"), "msix")
        self.assertEqual(marker.fields.get("messages"), "3")

    def test_returns_none_when_missing(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-input|PASS\n"
        marker = self.harness._parse_virtio_input_msix_marker(tail)
        self.assertIsNone(marker)


if __name__ == "__main__":
    unittest.main()

