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


class VirtioBlkCountersHostMarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def _emit(self, tail: bytes) -> str:
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.harness._emit_virtio_blk_counters_host_marker(tail)
        return buf.getvalue().strip()

    def test_emits_info_marker(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|INFO|abort=0|reset_device=1|"
            b"reset_bus=2|pnp=3|ioctl_reset=4|capacity_change_events=5\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_COUNTERS|INFO|abort=0|reset_device=1|reset_bus=2|pnp=3|ioctl_reset=4|capacity_change_events=5",
        )

    def test_coerces_pass_to_info(self) -> None:
        # The guest marker is specified as INFO/SKIP, but be defensive: if it ever emits
        # PASS/FAIL, the host marker should remain stable and report INFO.
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|PASS|abort=0|reset_device=0|"
            b"reset_bus=0|pnp=0|ioctl_reset=0|capacity_change_events=0\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_COUNTERS|INFO|abort=0|reset_device=0|reset_bus=0|pnp=0|ioctl_reset=0|capacity_change_events=0",
        )

    def test_emits_skip_marker(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|SKIP|reason=ioctl_payload_truncated|returned_len=32\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_COUNTERS|SKIP|reason=ioctl_payload_truncated|returned_len=32",
        )

    def test_no_output_when_missing(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|RESULT|PASS\n"
        out = self._emit(tail)
        self.assertEqual(out, "")


if __name__ == "__main__":
    unittest.main()
