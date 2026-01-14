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


class VirtioInputBindingMarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def _emit(self, tail: bytes) -> str:
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.harness._emit_virtio_input_binding_host_marker(tail)
        return buf.getvalue().strip()

    def test_emits_pass_marker(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|PASS|service=aero_virtio_input|"
            b"pnp_id=PCI\\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_BINDING|PASS|service=aero_virtio_input|"
            "pnp_id=PCI\\VEN_1AF4&DEV_1052&SUBSYS_00101AF4&REV_01",
        )

    def test_emits_fail_marker_wrong_service(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|FAIL|reason=wrong_service|"
            b"expected=aero_virtio_input|actual=vioinput|"
            b"pnp_id=PCI\\VEN_1AF4&DEV_1052&REV_01\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_BINDING|FAIL|reason=wrong_service|"
            "expected=aero_virtio_input|actual=vioinput|pnp_id=PCI\\VEN_1AF4&DEV_1052&REV_01",
        )


class VirtioInputBindingGatingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_required_marker_pass(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|PASS|service=aero_virtio_input|pnp_id=PCI\\VEN_1AF4&DEV_1052\n"
        self.assertIsNone(h._virtio_input_binding_required_failure_message(tail))

    def test_required_marker_fail(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|FAIL|reason=wrong_service|expected=aero_virtio_input|actual=vioinput\n"
        msg = h._virtio_input_binding_required_failure_message(tail)
        self.assertIsNotNone(msg)
        self.assertTrue(str(msg).startswith("FAIL: VIRTIO_INPUT_BINDING_FAILED:"))

    def test_required_marker_missing(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|RESULT|PASS\n"
        msg = h._virtio_input_binding_required_failure_message(tail)
        self.assertIsNotNone(msg)
        self.assertTrue(str(msg).startswith("FAIL: MISSING_VIRTIO_INPUT_BINDING:"))


if __name__ == "__main__":
    unittest.main()

