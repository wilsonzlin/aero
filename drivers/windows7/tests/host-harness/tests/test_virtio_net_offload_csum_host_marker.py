#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import contextlib
import importlib.util
import io
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


class VirtioNetOffloadCsumHostMarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def _emit(self, tail: bytes) -> str:
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.harness._emit_virtio_net_offload_csum_host_marker(tail)
        return buf.getvalue().strip()

    def test_emits_marker(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-offload-csum|PASS|tx_csum=10|rx_csum=20|fallback=3|"
            b"tx_tcp=4|tx_udp=6|rx_tcp=7|rx_udp=13|tx_tcp4=1|tx_tcp6=3|tx_udp4=2|tx_udp6=4\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_OFFLOAD_CSUM|PASS|tx_csum=10|rx_csum=20|fallback=3|tx_tcp=4|tx_udp=6|"
            "rx_tcp=7|rx_udp=13|tx_tcp4=1|tx_tcp6=3|tx_udp4=2|tx_udp6=4",
        )

    def test_uses_last_marker(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-offload-csum|PASS|tx_csum=1|rx_csum=2|fallback=3\n"
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-offload-csum|FAIL|tx_csum=9|rx_csum=8|fallback=7\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_OFFLOAD_CSUM|FAIL|tx_csum=9|rx_csum=8|fallback=7",
        )

    def test_no_output_when_missing(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS\n"
        out = self._emit(tail)
        self.assertEqual(out, "")


if __name__ == "__main__":
    unittest.main()

