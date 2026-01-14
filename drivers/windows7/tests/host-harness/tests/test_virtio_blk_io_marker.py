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


class VirtioBlkIoMarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def _emit(self, tail: bytes) -> str:
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.harness._emit_virtio_blk_io_host_marker(tail)
        return buf.getvalue().strip()

    def test_emits_pass_with_all_fields(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|write_ok=1|write_bytes=33554432|"
            b"write_mbps=123.45|flush_ok=1|read_ok=1|read_bytes=33554432|read_mbps=234.56\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IO|PASS|write_ok=1|write_bytes=33554432|"
            "write_mbps=123.45|flush_ok=1|read_ok=1|read_bytes=33554432|read_mbps=234.56",
        )

    def test_emits_fail_token(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|FAIL|write_ok=0|write_bytes=0|write_mbps=0.00|"
            b"flush_ok=0|read_ok=0|read_bytes=0|read_mbps=0.00\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IO|FAIL|write_ok=0|write_bytes=0|write_mbps=0.00|"
            "flush_ok=0|read_ok=0|read_bytes=0|read_mbps=0.00",
        )

    def test_emits_pass_ignoring_extra_fields(self) -> None:
        # The guest virtio-blk marker may include non-perf diagnostic fields (IRQ mode, recovery counters, etc).
        # Ensure the stable VIRTIO_BLK_IO host marker remains parseable and unchanged.
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|irq_mode=msix|abort_srb=0|reset_device_srb=1|"
            b"reset_bus_srb=0|pnp_srb=0|ioctl_reset=0|write_ok=1|write_bytes=33554432|write_mbps=123.45|"
            b"flush_ok=1|read_ok=1|read_bytes=33554432|read_mbps=234.56\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IO|PASS|write_ok=1|write_bytes=33554432|"
            "write_mbps=123.45|flush_ok=1|read_ok=1|read_bytes=33554432|read_mbps=234.56",
        )

    def test_no_output_when_no_perf_fields(self) -> None:
        # Backward compatible old marker.
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS\n"
        out = self._emit(tail)
        self.assertEqual(out, "")

        # Even if unrelated key/value fields exist, do not emit the IO marker unless the
        # perf keys are included.
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|irq_mode=msix\n"
        out = self._emit(tail)
        self.assertEqual(out, "")


if __name__ == "__main__":
    unittest.main()
