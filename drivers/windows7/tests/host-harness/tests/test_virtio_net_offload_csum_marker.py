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


class VirtioNetOffloadCsumMarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_extracts_stats(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-offload-csum|PASS|tx_csum=123|rx_csum=456|fallback=7\n"
        )
        stats = self.harness._extract_virtio_net_offload_csum_stats(tail)
        assert stats is not None
        self.assertEqual(stats["status"], "PASS")
        self.assertEqual(stats["tx_csum"], 123)
        self.assertEqual(stats["rx_csum"], 456)
        self.assertEqual(stats["fallback"], 7)

    def test_uses_last_marker(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-offload-csum|PASS|tx_csum=1|rx_csum=2|fallback=3\n"
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-offload-csum|PASS|tx_csum=9|rx_csum=8|fallback=7\n"
        )
        stats = self.harness._extract_virtio_net_offload_csum_stats(tail)
        assert stats is not None
        self.assertEqual(stats["tx_csum"], 9)
        self.assertEqual(stats["rx_csum"], 8)
        self.assertEqual(stats["fallback"], 7)

    def test_returns_none_when_missing(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS\n"
        stats = self.harness._extract_virtio_net_offload_csum_stats(tail)
        self.assertIsNone(stats)


if __name__ == "__main__":
    unittest.main()

