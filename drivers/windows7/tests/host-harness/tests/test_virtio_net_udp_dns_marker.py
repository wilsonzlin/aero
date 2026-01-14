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


class VirtioNetUdpDnsMarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def _emit(self, tail: bytes) -> str:
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.harness._emit_virtio_net_udp_dns_host_marker(tail)
        return buf.getvalue().strip()

    def test_emits_pass(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp-dns|PASS|server=10.0.2.3|query=example.com|"
            b"sent=40|recv=128|rcode=0\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_UDP_DNS|PASS|server=10.0.2.3|query=example.com|sent=40|recv=128|rcode=0",
        )

    def test_emits_fail_with_reason(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp-dns|FAIL|timeout\n"
        )
        out = self._emit(tail)
        self.assertEqual(out, "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_UDP_DNS|FAIL|reason=timeout")

    def test_emits_skip_with_reason(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp-dns|SKIP|no_dns_server\n"
        out = self._emit(tail)
        self.assertEqual(out, "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_UDP_DNS|SKIP|reason=no_dns_server")

    def test_uses_last_marker(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp-dns|FAIL|timeout\n"
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp-dns|PASS|server=10.0.2.3|query=example.com|sent=40|recv=128|rcode=0\n"
        )
        out = self._emit(tail)
        self.assertTrue(out.startswith("AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_UDP_DNS|PASS|"))


if __name__ == "__main__":
    unittest.main()

