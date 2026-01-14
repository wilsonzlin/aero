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


class VirtioSndMsixHostMarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def _emit(self, tail: bytes) -> str:
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.harness._emit_virtio_snd_msix_host_marker(tail)
        return buf.getvalue().strip()

    def test_emits_pass_marker(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|PASS|mode=msix|messages=5|config_vector=0|queue0_vector=1|"
            b"queue1_vector=2|queue2_vector=3|queue3_vector=4|interrupts=10|dpcs=10|drain0=1|drain1=2|drain2=3|drain3=4\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_MSIX|PASS|mode=msix|messages=5|config_vector=0|queue0_vector=1|"
            "queue1_vector=2|queue2_vector=3|queue3_vector=4|interrupts=10|dpcs=10|drain0=1|drain1=2|drain2=3|drain3=4",
        )

    def test_emits_skip_marker(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-msix|SKIP|reason=diag_unavailable|err=2\n"
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_MSIX|SKIP|reason=diag_unavailable|err=2",
        )

    def test_no_output_when_missing(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd|PASS\n"
        out = self._emit(tail)
        self.assertEqual(out, "")


if __name__ == "__main__":
    unittest.main()

