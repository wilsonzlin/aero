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


class VirtioInputMsixHostMarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def _emit(self, tail: bytes) -> str:
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.harness._emit_virtio_input_msix_host_marker(tail)
        return buf.getvalue().strip()

    def test_emits_pass_marker(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|PASS|mode=msix|messages=3|mapping=per-queue|used_vectors=3|"
            b"config_vector=0|queue0_vector=1|queue1_vector=2|msix_devices=2|intx_devices=0|unknown_devices=0|"
            b"intx_spurious=0|total_interrupts=10|total_dpcs=10|config_irqs=1|queue0_irqs=2|queue1_irqs=3\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_MSIX|PASS|mode=msix|messages=3|mapping=per-queue|used_vectors=3|"
            "config_vector=0|queue0_vector=1|queue1_vector=2|msix_devices=2|intx_devices=0|unknown_devices=0|"
            "intx_spurious=0|total_interrupts=10|total_dpcs=10|config_irqs=1|queue0_irqs=2|queue1_irqs=3",
        )

    def test_emits_skip_reason(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-msix|SKIP|reason=ioctl_not_supported|err=1\n"
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_MSIX|SKIP|reason=ioctl_not_supported|err=1",
        )

    def test_no_output_when_missing(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-input|PASS\n"
        out = self._emit(tail)
        self.assertEqual(out, "")


if __name__ == "__main__":
    unittest.main()

