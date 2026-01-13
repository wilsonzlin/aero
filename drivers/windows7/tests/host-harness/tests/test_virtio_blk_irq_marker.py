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


class VirtioBlkIrqMarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def _emit(self, tail: bytes) -> str:
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.harness._emit_virtio_blk_irq_host_marker(tail)
        return buf.getvalue().strip()

    def test_emits_pass_with_msix_fields(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|irq_mode=msix|msix_config_vector=0|"
            b"msix_queue_vector=1\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ|PASS|irq_mode=msix|msix_config_vector=0|msix_queue_vector=1",
        )

    def test_emits_fail_token(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|FAIL|irq_mode=intx\n"
        out = self._emit(tail)
        self.assertEqual(out, "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ|FAIL|irq_mode=intx")

    def test_uses_last_marker(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|irq_mode=intx\n"
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|irq_mode=msix|msix_config_vector=5|msix_queue_vector=6\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ|PASS|irq_mode=msix|msix_config_vector=5|msix_queue_vector=6",
        )

    def test_no_output_when_no_irq_fields(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS\n"
        out = self._emit(tail)
        self.assertEqual(out, "")

        # Even if other key/value fields are present, do not emit the IRQ marker unless
        # interrupt-mode related fields are included.
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|sector_size=512\n"
        out = self._emit(tail)
        self.assertEqual(out, "")


if __name__ == "__main__":
    unittest.main()

