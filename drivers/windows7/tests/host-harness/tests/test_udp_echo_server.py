#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import importlib.util
import socket
import sys
import time
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


class HarnessUdpEchoServerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_echo_roundtrip(self) -> None:
        h = self.harness
        server = h._UdpEchoServer("127.0.0.1", 0)
        server.start()
        try:
            port = server.port
            s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            try:
                s.settimeout(1.0)
                payload = b"aero-virtio-udp-echo"
                s.sendto(payload, ("127.0.0.1", port))
                data, _ = s.recvfrom(4096)
                self.assertEqual(data, payload)

                # Also test a near-MTU payload (fragmentation avoidance).
                payload2 = bytes([i & 0xFF for i in range(1400)])
                s.sendto(payload2, ("127.0.0.1", port))
                data, _ = s.recvfrom(4096)
                self.assertEqual(data, payload2)
            finally:
                s.close()
        finally:
            server.close()

    def test_bind_failure(self) -> None:
        h = self.harness
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        try:
            s.bind(("127.0.0.1", 0))
            port = int(s.getsockname()[1])

            server = h._UdpEchoServer("127.0.0.1", port)
            with self.assertRaises(OSError):
                server.start()
        finally:
            s.close()

    def test_close_does_not_hang_without_traffic(self) -> None:
        h = self.harness
        server = h._UdpEchoServer("127.0.0.1", 0, socket_timeout_seconds=0.1)
        server.start()
        try:
            t0 = time.monotonic()
            server.close()
            dt = time.monotonic() - t0
            # This is a best-effort bound; the goal is to ensure the socket timeout path
            # allows prompt shutdown even when no datagrams arrive.
            self.assertLess(dt, 2.0)
        finally:
            server.close()


if __name__ == "__main__":
    unittest.main()

