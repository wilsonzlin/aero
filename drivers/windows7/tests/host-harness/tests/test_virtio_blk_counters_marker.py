#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import contextlib
import importlib.util
import io
import sys
import unittest
from pathlib import Path
from typing import Optional


def _load_harness():
    harness_path = Path(__file__).resolve().parents[1] / "invoke_aero_virtio_win7_tests.py"
    spec = importlib.util.spec_from_file_location("invoke_aero_virtio_win7_tests", harness_path)
    assert spec and spec.loader

    module = importlib.util.module_from_spec(spec)
    # Register the module before execution so dataclasses can resolve __module__ correctly.
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class VirtioBlkCountersMarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def _emit(self, tail: bytes, *, line: Optional[str] = None) -> str:
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            if line is None:
                self.harness._emit_virtio_blk_counters_host_marker(tail)
            else:
                self.harness._emit_virtio_blk_counters_host_marker(tail, blk_counters_line=line)
        return buf.getvalue().strip()

    def test_emits_info_with_all_fields(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|INFO|abort=0|reset_device=1|reset_bus=2|"
            b"pnp=3|ioctl_reset=4|capacity_change_events=5\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_COUNTERS|INFO|abort=0|reset_device=1|reset_bus=2|pnp=3|"
            "ioctl_reset=4|capacity_change_events=5",
        )

    def test_emits_skip_on_truncated_payload(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|SKIP|reason=ioctl_payload_truncated|returned_len=16\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_COUNTERS|SKIP|reason=ioctl_payload_truncated|returned_len=16",
        )

    def test_no_output_when_marker_missing(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS\n"
        out = self._emit(tail)
        self.assertEqual(out, "")

    def test_uses_explicit_marker_line_override(self) -> None:
        line = (
            "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-counters|INFO|abort=0|reset_device=0|reset_bus=0|"
            "pnp=0|ioctl_reset=0|capacity_change_events=not_supported"
        )
        out = self._emit(b"", line=line)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_COUNTERS|INFO|abort=0|reset_device=0|reset_bus=0|pnp=0|"
            "ioctl_reset=0|capacity_change_events=not_supported",
        )


if __name__ == "__main__":
    unittest.main()
