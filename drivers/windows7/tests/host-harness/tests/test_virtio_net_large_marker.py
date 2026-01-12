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


class VirtioNetLargeMarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def _emit(self, tail: bytes) -> str:
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.harness._emit_virtio_net_large_host_marker(tail)
        return buf.getvalue().strip()

    def test_emits_pass_with_large_ok(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS|large_ok=1|large_bytes=1048576|"
            b"large_fnv1a64=0x8505ae4435522325|large_mbps=123.45\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_LARGE|PASS|large_ok=1|large_bytes=1048576|"
            "large_fnv1a64=0x8505ae4435522325|large_mbps=123.45",
        )

    def test_falls_back_to_pass_fail_token_when_large_ok_missing(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS|large_bytes=1048576|large_mbps=99.0\n"
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_LARGE|PASS|large_bytes=1048576|large_mbps=99.0",
        )

        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|FAIL|large_bytes=0|large_mbps=0\n"
        out = self._emit(tail)
        self.assertEqual(out, "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_LARGE|FAIL|large_bytes=0|large_mbps=0")

    def test_uses_last_marker(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|FAIL|large_bytes=0\n"
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS|large_bytes=1048576\n"
        )
        out = self._emit(tail)
        self.assertEqual(out, "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_LARGE|PASS|large_bytes=1048576")

    def test_no_output_when_no_fields(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS\n"
        out = self._emit(tail)
        self.assertEqual(out, "")


if __name__ == "__main__":
    unittest.main()

