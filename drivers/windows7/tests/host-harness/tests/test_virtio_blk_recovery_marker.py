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


class VirtioBlkRecoveryMarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def _emit(self, tail: bytes) -> str:
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.harness._emit_virtio_blk_recovery_host_marker(tail)
        return buf.getvalue().strip()

    def test_emits_info_with_all_fields(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|irq_mode=msix|abort_srb=0|"
            b"reset_device_srb=1|reset_bus_srb=2|pnp_srb=3|ioctl_reset=4\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RECOVERY|INFO|abort_srb=0|reset_device_srb=1|reset_bus_srb=2|pnp_srb=3|ioctl_reset=4",
        )

    def test_no_output_when_counters_missing(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|irq_mode=msix\n"
        out = self._emit(tail)
        self.assertEqual(out, "")

    def test_gate_passes_on_all_zero(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|abort_srb=0|reset_device_srb=0|"
            b"reset_bus_srb=0|pnp_srb=0|ioctl_reset=0\n"
        )
        msg = self.harness._check_no_blk_recovery_requirement(tail)
        self.assertIsNone(msg)

    def test_gate_fails_on_nonzero(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|abort_srb=0|reset_device_srb=1|"
            b"reset_bus_srb=0|pnp_srb=0|ioctl_reset=0\n"
        )
        msg = self.harness._check_no_blk_recovery_requirement(tail)
        self.assertIsNotNone(msg)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_BLK_RECOVERY_NONZERO:"))

    def test_gate_uses_preparsed_blk_marker_line(self) -> None:
        # Simulate the harness tail buffer not containing the virtio-blk marker (e.g. truncated),
        # but the caller still providing the last observed virtio-blk marker line.
        tail = b"AERO_VIRTIO_SELFTEST|RESULT|PASS\n"
        blk_line = (
            "AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|abort_srb=0|reset_device_srb=0|"
            "reset_bus_srb=0|pnp_srb=0|ioctl_reset=2"
        )
        msg = self.harness._check_no_blk_recovery_requirement(tail, blk_test_line=blk_line)
        self.assertIsNotNone(msg)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_BLK_RECOVERY_NONZERO:"))

    def test_emit_uses_preparsed_blk_marker_line(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|RESULT|PASS\n"
        blk_line = (
            "AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|abort_srb=0|reset_device_srb=1|"
            "reset_bus_srb=2|pnp_srb=3|ioctl_reset=4"
        )
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.harness._emit_virtio_blk_recovery_host_marker(tail, blk_test_line=blk_line)
        self.assertEqual(
            buf.getvalue().strip(),
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RECOVERY|INFO|abort_srb=0|reset_device_srb=1|reset_bus_srb=2|pnp_srb=3|ioctl_reset=4",
        )


if __name__ == "__main__":
    unittest.main()
