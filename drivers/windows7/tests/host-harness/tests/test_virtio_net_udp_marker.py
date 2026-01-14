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


class VirtioNetUdpMarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def _emit(self, tail: bytes) -> str:
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.harness._emit_virtio_net_udp_host_marker(tail)
        return buf.getvalue().strip()

    def test_emits_pass(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|PASS|bytes=123|small_bytes=10|mtu_bytes=100|reason=-|wsa=0\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_UDP|PASS|bytes=123|small_bytes=10|mtu_bytes=100|reason=-|wsa=0",
        )

    def test_emits_fail(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|FAIL|bytes=0|small_bytes=0|mtu_bytes=0|reason=timeout|wsa=10060\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_UDP|FAIL|bytes=0|small_bytes=0|mtu_bytes=0|reason=timeout|wsa=10060",
        )

    def test_uses_last_marker(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|FAIL|bytes=0|small_bytes=0|mtu_bytes=0|reason=timeout|wsa=10060\n"
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|PASS|bytes=1|small_bytes=1|mtu_bytes=1|reason=-|wsa=0\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_UDP|PASS|bytes=1|small_bytes=1|mtu_bytes=1|reason=-|wsa=0",
        )

    def test_no_output_when_missing(self) -> None:
        out = self._emit(b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS\n")
        self.assertEqual(out, "")


if __name__ == "__main__":
    unittest.main()

