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


class VirtioIrqMarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def _emit(self, fn, tail: bytes) -> str:
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            fn(tail)
        return buf.getvalue().strip()

    def test_virtio_net_irq_marker_pass(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS|irq_mode=msix|irq_message_count=3\n"
        out = self._emit(self.harness._emit_virtio_net_irq_host_marker, tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_IRQ|PASS|irq_mode=msix|irq_message_count=3",
        )

    def test_virtio_snd_irq_marker_fail(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd|FAIL|irq_mode=msi|irq_message_count=1\n"
        out = self._emit(self.harness._emit_virtio_snd_irq_host_marker, tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_IRQ|FAIL|irq_mode=msi|irq_message_count=1",
        )

    def test_virtio_input_irq_marker_partial_fields(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-input|PASS|irq_mode=intx\n"
        out = self._emit(self.harness._emit_virtio_input_irq_host_marker, tail)
        self.assertEqual(out, "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_IRQ|PASS|irq_mode=intx")

    def test_uses_last_marker_line(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS|irq_mode=msi|irq_message_count=1\n"
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS|irq_mode=msix|irq_message_count=3\n"
        )
        out = self._emit(self.harness._emit_virtio_net_irq_host_marker, tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_IRQ|PASS|irq_mode=msix|irq_message_count=3",
        )

    def test_no_output_when_no_irq_fields(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS|large_bytes=1048576\n"
        out = self._emit(self.harness._emit_virtio_net_irq_host_marker, tail)
        self.assertEqual(out, "")


if __name__ == "__main__":
    unittest.main()

