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


class VirtioNetMsixMarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_returns_none_when_missing(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS\n"
        self.assertIsNone(self.harness._parse_virtio_net_msix_marker(tail))

    def test_parses_status_and_fields(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|PASS|mode=msix|messages=3|"
            b"config_vector=0|rx_vector=1|tx_vector=2\n"
        )
        parsed = self.harness._parse_virtio_net_msix_marker(tail)
        assert parsed is not None
        status, fields = parsed
        self.assertEqual(status, "PASS")
        self.assertEqual(fields.get("mode"), "msix")
        self.assertEqual(fields.get("messages"), "3")
        self.assertEqual(fields.get("config_vector"), "0")
        self.assertEqual(fields.get("rx_vector"), "1")
        self.assertEqual(fields.get("tx_vector"), "2")

    def test_uses_last_marker(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|FAIL|mode=intx|messages=0\n"
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|PASS|mode=msix|messages=3\n"
        )
        parsed = self.harness._parse_virtio_net_msix_marker(tail)
        assert parsed is not None
        status, fields = parsed
        self.assertEqual(status, "PASS")
        self.assertEqual(fields.get("mode"), "msix")


if __name__ == "__main__":
    unittest.main()

