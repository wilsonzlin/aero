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


class VirtioBlkResetRecoveryMarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def _emit(self, tail: bytes, *, line: Optional[str] = None) -> str:
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            if line is None:
                self.harness._emit_virtio_blk_reset_recovery_host_marker(tail)
            else:
                self.harness._emit_virtio_blk_reset_recovery_host_marker(
                    tail, blk_reset_recovery_line=line
                )
        return buf.getvalue().strip()

    def test_emits_info_with_all_fields(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|INFO|reset_detected=2|hw_reset_bus=3\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESET_RECOVERY|INFO|reset_detected=2|hw_reset_bus=3",
        )

    def test_coerces_pass_to_info(self) -> None:
        # The guest marker is expected to use INFO/SKIP, but if it ever emits PASS/FAIL
        # (e.g. due to a guest-side change), keep the host marker stable as INFO/SKIP.
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|PASS|reset_detected=0|hw_reset_bus=0\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESET_RECOVERY|INFO|reset_detected=0|hw_reset_bus=0",
        )

    def test_emits_skip_on_truncated_payload(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|SKIP|reason=ioctl_payload_truncated|returned_len=16\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESET_RECOVERY|SKIP|reason=ioctl_payload_truncated|returned_len=16",
        )

    def test_no_output_when_marker_missing(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS\n"
        out = self._emit(tail)
        self.assertEqual(out, "")

    def test_falls_back_to_miniport_info_diagnostic(self) -> None:
        tail = b"virtio-blk-miniport-reset-recovery|INFO|reset_detected=2|hw_reset_bus=3\n"
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESET_RECOVERY|INFO|reset_detected=2|hw_reset_bus=3",
        )

    def test_falls_back_to_miniport_warn_diagnostic_as_skip(self) -> None:
        tail = b"virtio-blk-miniport-reset-recovery|WARN|reason=missing_counters|returned_len=20|expected_min=24\n"
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESET_RECOVERY|SKIP|reason=missing_counters|returned_len=20|expected_min=24",
        )

    def test_uses_explicit_marker_line_override(self) -> None:
        line = (
            "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|INFO|reset_detected=7|hw_reset_bus=8"
        )
        out = self._emit(b"", line=line)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESET_RECOVERY|INFO|reset_detected=7|hw_reset_bus=8",
        )


if __name__ == "__main__":
    unittest.main()
