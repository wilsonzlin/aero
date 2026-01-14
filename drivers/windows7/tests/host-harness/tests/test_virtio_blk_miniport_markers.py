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


class VirtioBlkMiniportMarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def _emit_flags(self, tail: bytes, *, line: str | None = None) -> str:
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            if line is None:
                self.harness._emit_virtio_blk_miniport_flags_host_marker(tail)
            else:
                self.harness._emit_virtio_blk_miniport_flags_host_marker(tail, marker_line=line)
        return buf.getvalue().strip()

    def _emit_reset_recovery(self, tail: bytes, *, line: str | None = None) -> str:
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            if line is None:
                self.harness._emit_virtio_blk_miniport_reset_recovery_host_marker(tail)
            else:
                self.harness._emit_virtio_blk_miniport_reset_recovery_host_marker(tail, marker_line=line)
        return buf.getvalue().strip()

    def test_flags_emits_info_marker(self) -> None:
        tail = (
            b"virtio-blk-miniport-flags|INFO|raw=0x00000001|removed=0|surprise_removed=1|"
            b"reset_in_progress=0|reset_pending=1\n"
        )
        out = self._emit_flags(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_MINIPORT_FLAGS|INFO|raw=0x00000001|removed=0|"
            "surprise_removed=1|reset_in_progress=0|reset_pending=1",
        )

    def test_flags_emits_warn_marker(self) -> None:
        tail = b"virtio-blk-miniport-flags|WARN|reason=missing_flags|returned_len=16|expected_min=20\n"
        out = self._emit_flags(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_MINIPORT_FLAGS|WARN|reason=missing_flags|returned_len=16|expected_min=20",
        )

    def test_flags_uses_last_marker(self) -> None:
        tail = (
            b"virtio-blk-miniport-flags|WARN|reason=missing_flags|returned_len=16|expected_min=20\n"
            b"virtio-blk-miniport-flags|INFO|raw=0x2|removed=0|surprise_removed=0|reset_in_progress=0|reset_pending=0\n"
        )
        out = self._emit_flags(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_MINIPORT_FLAGS|INFO|raw=0x2|removed=0|"
            "surprise_removed=0|reset_in_progress=0|reset_pending=0",
        )

    def test_flags_no_output_when_missing(self) -> None:
        out = self._emit_flags(b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS\n")
        self.assertEqual(out, "")

    def test_flags_uses_explicit_marker_line_override(self) -> None:
        line = (
            "virtio-blk-miniport-flags|INFO|raw=0x3|removed=0|surprise_removed=0|"
            "reset_in_progress=1|reset_pending=0"
        )
        out = self._emit_flags(b"", line=line)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_MINIPORT_FLAGS|INFO|raw=0x3|removed=0|"
            "surprise_removed=0|reset_in_progress=1|reset_pending=0",
        )

    def test_reset_recovery_emits_info_marker(self) -> None:
        tail = b"virtio-blk-miniport-reset-recovery|INFO|reset_detected=3|hw_reset_bus=4\n"
        out = self._emit_reset_recovery(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_MINIPORT_RESET_RECOVERY|INFO|reset_detected=3|hw_reset_bus=4",
        )

    def test_reset_recovery_emits_warn_marker(self) -> None:
        tail = (
            b"virtio-blk-miniport-reset-recovery|WARN|reason=missing_counters|returned_len=20|expected_min=24\n"
        )
        out = self._emit_reset_recovery(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_MINIPORT_RESET_RECOVERY|WARN|reason=missing_counters|returned_len=20|expected_min=24",
        )

    def test_reset_recovery_no_output_when_missing(self) -> None:
        out = self._emit_reset_recovery(b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS\n")
        self.assertEqual(out, "")


if __name__ == "__main__":
    unittest.main()

