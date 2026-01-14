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

    def test_emits_pass_marker_with_extra_fields(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|PASS|service=aero_virtio_input|"
            b"pnp_id=PCI\\VEN_1AF4&DEV_1052&REV_01|hwid0=PCI\\VEN_1AF4&DEV_1052&REV_01|"
            b"cm_problem=0|cm_status=0x00000000\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_BINDING|PASS|service=aero_virtio_input|"
            "pnp_id=PCI\\VEN_1AF4&DEV_1052&REV_01|hwid0=PCI\\VEN_1AF4&DEV_1052&REV_01|"
            "cm_problem=0|cm_status=0x00000000",
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

    def test_emits_fail_marker_device_error_fields(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|FAIL|reason=device_error|"
            b"service=aero_virtio_input|pnp_id=PCI\\VEN_1AF4&DEV_1052&REV_01|"
            b"cm_problem=10|cm_status=0x0000000A\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_BINDING|FAIL|reason=device_error|"
            "service=aero_virtio_input|pnp_id=PCI\\VEN_1AF4&DEV_1052&REV_01|"
            "cm_problem=10|cm_status=0x0000000A",
        )

    def test_emits_marker_from_marker_line_even_when_tail_missing(self) -> None:
        # The host harness may pass an incrementally captured marker line even when the rolling tail
        # buffer no longer contains it (tail truncation). Ensure this still emits the correct host
        # marker.
        marker_line = (
            "AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|PASS|service=aero_virtio_input|"
            "pnp_id=PCI\\VEN_1AF4&DEV_1052&REV_01"
        )
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.harness._emit_virtio_input_binding_host_marker(b"", marker_line=marker_line)
        self.assertEqual(
            buf.getvalue().strip(),
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_BINDING|PASS|service=aero_virtio_input|"
            "pnp_id=PCI\\VEN_1AF4&DEV_1052&REV_01",
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

    def test_required_marker_pass_from_marker_line(self) -> None:
        h = self.harness
        msg = h._virtio_input_binding_required_failure_message(
            b"AERO_VIRTIO_SELFTEST|RESULT|PASS\n",
            marker_line=(
                "AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|PASS|service=aero_virtio_input|"
                "pnp_id=PCI\\VEN_1AF4&DEV_1052"
            ),
        )
        self.assertIsNone(msg)

    def test_required_marker_fail_from_marker_line(self) -> None:
        h = self.harness
        msg = h._virtio_input_binding_required_failure_message(
            b"",
            marker_line=(
                "AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|FAIL|reason=wrong_service|"
                "expected=aero_virtio_input|actual=vioinput|pnp_id=PCI\\VEN_1AF4&DEV_1052"
            ),
        )
        self.assertIsNotNone(msg)
        self.assertIn("FAIL: VIRTIO_INPUT_BINDING_FAILED:", str(msg))
        self.assertIn("reason=wrong_service", str(msg))
        self.assertIn("expected=aero_virtio_input", str(msg))
        self.assertIn("actual=vioinput", str(msg))

    def test_required_marker_skip_from_marker_line(self) -> None:
        h = self.harness
        msg = h._virtio_input_binding_required_failure_message(
            b"",
            marker_line="AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|SKIP|reason=flag_not_set",
        )
        self.assertIsNotNone(msg)
        self.assertIn("FAIL: VIRTIO_INPUT_BINDING_SKIPPED:", str(msg))

    def test_required_marker_pass_from_saw_flag(self) -> None:
        h = self.harness
        msg = h._virtio_input_binding_required_failure_message(b"", saw_pass=True)
        self.assertIsNone(msg)


if __name__ == "__main__":
    unittest.main()
