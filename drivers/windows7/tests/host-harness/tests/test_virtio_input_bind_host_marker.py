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


class VirtioInputBindHostMarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def _emit(self, tail: bytes) -> str:
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.harness._emit_virtio_input_bind_host_marker(tail)
        return buf.getvalue().strip()

    def test_emits_pass_marker(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|PASS|devices=2\n"
        out = self._emit(tail)
        self.assertEqual(out, "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_BIND|PASS|devices=2")

    def test_emits_pass_marker_with_service(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|PASS|service=aero_virtio_input|"
            b"pnp_id=PCI\\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01|devices=2|wrong_service=0|missing_service=0|problem=0\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_BIND|PASS|service=aero_virtio_input|"
            "pnp_id=PCI\\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01|devices=2|wrong_service=0|missing_service=0|problem=0",
        )

    def test_emits_fail_marker(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|FAIL|devices=2|wrong_service=1|missing_service=0|problem=1\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_BIND|FAIL|devices=2|wrong_service=1|missing_service=0|problem=1",
        )

    def test_emits_fail_marker_with_expected_actual(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|FAIL|reason=wrong_service|expected=aero_virtio_input|"
            b"actual=vioinput|pnp_id=PCI\\VEN_1AF4&DEV_1052&REV_01|devices=2|wrong_service=2|missing_service=0|problem=0\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_BIND|FAIL|reason=wrong_service|expected=aero_virtio_input|"
            "actual=vioinput|pnp_id=PCI\\VEN_1AF4&DEV_1052&REV_01|devices=2|wrong_service=2|missing_service=0|problem=0",
        )

    def test_uses_last_marker(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|PASS|service=aero_virtio_input|pnp_id=PCI\\A|devices=1\n"
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-bind|FAIL|reason=device_missing|expected=aero_virtio_input|devices=0\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_BIND|FAIL|reason=device_missing|expected=aero_virtio_input|devices=0",
        )

    def test_no_output_when_missing(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-input|PASS\n"
        out = self._emit(tail)
        self.assertEqual(out, "")


if __name__ == "__main__":
    unittest.main()
