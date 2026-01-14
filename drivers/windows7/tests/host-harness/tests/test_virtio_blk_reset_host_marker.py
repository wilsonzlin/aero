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


class VirtioBlkResetHostMarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def _emit(self, tail: bytes) -> str:
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.harness._emit_virtio_blk_reset_host_marker(tail)
        return buf.getvalue().strip()

    def test_emits_pass_marker(self) -> None:
        out = self._emit(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|PASS|performed=1|counter_before=1|counter_after=2\n"
        )
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESET|PASS|performed=1|counter_before=1|counter_after=2",
        )

    def test_emits_skip_marker(self) -> None:
        out = self._emit(b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|SKIP|reason=not_supported\n")
        self.assertEqual(out, "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESET|SKIP|reason=not_supported")

    def test_emits_fail_marker(self) -> None:
        out = self._emit(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|FAIL|reason=post_reset_io_failed|err=123\n"
        )
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESET|FAIL|err=123|reason=post_reset_io_failed",
        )

    def test_no_output_when_marker_missing(self) -> None:
        out = self._emit(b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS\n")
        self.assertEqual(out, "")

    def test_accepts_explicit_marker_line(self) -> None:
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.harness._emit_virtio_blk_reset_host_marker(
                b"",
                blk_reset_line=(
                    "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset|PASS|performed=1|counter_before=not_supported|counter_after=not_supported"
                ),
            )
        self.assertEqual(
            buf.getvalue().strip(),
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESET|PASS|performed=1|counter_before=not_supported|counter_after=not_supported",
        )


if __name__ == "__main__":
    unittest.main()

