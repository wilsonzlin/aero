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


class VirtioBlkResizeHostMarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def _emit(self, tail: bytes) -> str:
        h = self.harness
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            h._emit_virtio_blk_resize_host_marker(tail)
        return buf.getvalue().strip()

    def test_emits_pass_marker(self) -> None:
        out = self._emit(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|PASS|disk=2|old_bytes=1000|new_bytes=2000|elapsed_ms=123\n"
        )
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESIZE|PASS|disk=2|old_bytes=1000|new_bytes=2000|elapsed_ms=123",
        )

    def test_emits_ready_marker(self) -> None:
        out = self._emit(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|READY|disk=0|old_bytes=4096\n"
        )
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESIZE|READY|disk=0|old_bytes=4096",
        )

    def test_emits_skip_reason(self) -> None:
        out = self._emit(b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|SKIP|flag_not_set\n")
        self.assertEqual(out, "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESIZE|SKIP|reason=flag_not_set")

    def test_emits_fail_marker(self) -> None:
        out = self._emit(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|FAIL|reason=timeout|disk=1|old_bytes=512|last_bytes=512|err=1460\n"
        )
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESIZE|FAIL|disk=1|old_bytes=512|last_bytes=512|err=1460|reason=timeout",
        )

    def test_accepts_explicit_marker_line(self) -> None:
        h = self.harness
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            h._emit_virtio_blk_resize_host_marker(
                b"",
                blk_resize_line=(
                    "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|PASS|disk=3|old_bytes=1|new_bytes=2|elapsed_ms=9"
                ),
            )
        self.assertEqual(
            buf.getvalue().strip(),
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_RESIZE|PASS|disk=3|old_bytes=1|new_bytes=2|elapsed_ms=9",
        )


if __name__ == "__main__":
    unittest.main()
