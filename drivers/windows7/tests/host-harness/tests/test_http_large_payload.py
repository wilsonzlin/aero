#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import http.client
import importlib.util
import socket
import sys
import unittest
from pathlib import Path
from threading import Thread


def _load_harness():
    harness_path = Path(__file__).resolve().parents[1] / "invoke_aero_virtio_win7_tests.py"
    spec = importlib.util.spec_from_file_location("invoke_aero_virtio_win7_tests", harness_path)
    assert spec and spec.loader

    module = importlib.util.module_from_spec(spec)
    # Register the module before execution so dataclasses can resolve __module__ correctly.
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def _fnv1a64(data: bytes) -> int:
    h = 0xCBF29CE484222325
    prime = 0x100000001B3
    for b in data:
        h ^= b
        h = (h * prime) & 0xFFFFFFFFFFFFFFFF
    return h


class HarnessHttpLargePayloadTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def _start_server(self, expected_path: str):
        handler = type(
            "_Handler",
            (self.harness._QuietHandler,),
            {
                "expected_path": expected_path,
                "http_log_path": None,
            },
        )
        httpd = self.harness._ReusableTcpServer(("127.0.0.1", 0), handler)
        port = int(httpd.server_address[1])

        thread = Thread(target=httpd.serve_forever, daemon=True)
        thread.start()
        return httpd, thread, port

    def test_small_and_large_endpoints(self) -> None:
        httpd, thread, port = self._start_server("/aero-virtio-selftest")
        with httpd:
            try:
                # Small marker endpoint.
                c = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
                c.request("GET", "/aero-virtio-selftest")
                r = c.getresponse()
                body = r.read()
                c.close()
                self.assertEqual(r.status, 200)
                self.assertEqual(body, b"OK\n")
                self.assertEqual(r.getheader("Content-Type"), "text/plain")
                self.assertEqual(r.getheader("Content-Length"), str(len(body)))
                self.assertEqual(r.getheader("Cache-Control"), "no-store")
                self.assertIsNone(r.getheader("ETag"))

                # HEAD should return headers only.
                c = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
                c.request("HEAD", "/aero-virtio-selftest")
                r = c.getresponse()
                body = r.read()
                c.close()
                self.assertEqual(r.status, 200)
                self.assertEqual(body, b"")
                self.assertEqual(r.getheader("Content-Type"), "text/plain")
                self.assertEqual(r.getheader("Content-Length"), "3")
                self.assertEqual(r.getheader("Cache-Control"), "no-store")
                self.assertIsNone(r.getheader("ETag"))

                # Large deterministic payload endpoint.
                c = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
                c.request("GET", "/aero-virtio-selftest-large")
                r = c.getresponse()
                body = r.read()
                c.close()
                self.assertEqual(r.status, 200)
                self.assertEqual(r.getheader("Content-Type"), "application/octet-stream")
                self.assertEqual(r.getheader("Content-Length"), "1048576")
                self.assertEqual(r.getheader("ETag"), '"8505ae4435522325"')
                self.assertEqual(r.getheader("Cache-Control"), "no-store")
                self.assertEqual(len(body), 1048576)

                # Payload is 0..255 repeating.
                pat = bytes(range(256))
                self.assertEqual(body[:256], pat)
                self.assertEqual(body[256:512], pat)
                self.assertEqual(body[-256:], pat)

                # FNV-1a 64-bit (matches the guest selftest constant).
                self.assertEqual(_fnv1a64(body), 0x8505AE4435522325)

                # Large endpoint HEAD: headers only, still reports Content-Length.
                c = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
                c.request("HEAD", "/aero-virtio-selftest-large")
                r = c.getresponse()
                body = r.read()
                c.close()
                self.assertEqual(r.status, 200)
                self.assertEqual(body, b"")
                self.assertEqual(r.getheader("Content-Type"), "application/octet-stream")
                self.assertEqual(r.getheader("Content-Length"), "1048576")
                self.assertEqual(r.getheader("ETag"), '"8505ae4435522325"')
                self.assertEqual(r.getheader("Cache-Control"), "no-store")

                # Unknown path should 404.
                c = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
                c.request("GET", "/does-not-exist")
                r = c.getresponse()
                body = r.read()
                c.close()
                self.assertEqual(r.status, 404)
                self.assertEqual(body, b"NOT_FOUND\n")
                self.assertEqual(r.getheader("Content-Type"), "text/plain")
                self.assertEqual(r.getheader("Content-Length"), str(len(body)))
                self.assertEqual(r.getheader("Cache-Control"), "no-store")
                self.assertIsNone(r.getheader("ETag"))
            finally:
                httpd.shutdown()
                thread.join(timeout=2)

    def test_large_endpoint_with_query_string(self) -> None:
        # If the harness is configured with an HttpPath that includes a query string,
        # the large endpoint should insert "-large" before the query.
        httpd, thread, port = self._start_server("/aero-virtio-selftest?foo=bar")
        with httpd:
            try:
                c = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
                c.request("GET", "/aero-virtio-selftest?foo=bar")
                r = c.getresponse()
                body = r.read()
                c.close()
                self.assertEqual(r.status, 200)
                self.assertEqual(body, b"OK\n")

                # Preferred form (matches guest selftest URL suffix insertion).
                c = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
                c.request("GET", "/aero-virtio-selftest-large?foo=bar")
                r = c.getresponse()
                body = r.read()
                c.close()
                self.assertEqual(r.status, 200)
                self.assertEqual(len(body), 1048576)
                self.assertEqual(r.getheader("ETag"), '"8505ae4435522325"')
                self.assertEqual(r.getheader("Cache-Control"), "no-store")

                # Backcompat: also accept naive string concatenation.
                c = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
                c.request("GET", "/aero-virtio-selftest?foo=bar-large")
                r = c.getresponse()
                body = r.read()
                c.close()
                self.assertEqual(r.status, 200)
                self.assertEqual(len(body), 1048576)
            finally:
                httpd.shutdown()
                thread.join(timeout=2)

    def test_large_endpoint_client_disconnect_does_not_kill_server(self) -> None:
        # Regression test: if a client disconnects mid-transfer, the server should keep running
        # and continue servicing subsequent requests.
        httpd, thread, port = self._start_server("/aero-virtio-selftest")
        with httpd:
            try:
                s = socket.create_connection(("127.0.0.1", port), timeout=2)
                try:
                    s.sendall(b"GET /aero-virtio-selftest-large HTTP/1.1\r\nHost: localhost\r\n\r\n")
                    # Read a small prefix (header + some body), then disconnect abruptly.
                    _ = s.recv(1024)
                finally:
                    s.close()

                # Server should still respond to new requests.
                c = http.client.HTTPConnection("127.0.0.1", port, timeout=2)
                c.request("GET", "/aero-virtio-selftest")
                r = c.getresponse()
                body = r.read()
                c.close()
                self.assertEqual(r.status, 200)
                self.assertEqual(body, b"OK\n")
            finally:
                httpd.shutdown()
                thread.join(timeout=2)


if __name__ == "__main__":
    unittest.main()
