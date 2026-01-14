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


class VirtioInputInjectMarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_events_inject_pass_marker_format(self) -> None:
        h = self.harness
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            h._emit_virtio_input_events_inject_host_marker(
                ok=True, attempt=3, kbd_mode="device", mouse_mode="broadcast"
            )
        self.assertEqual(
            buf.getvalue().strip(),
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_EVENTS_INJECT|PASS|attempt=3|kbd_mode=device|mouse_mode=broadcast",
        )

    def test_events_inject_fail_sanitizes_reason(self) -> None:
        h = self.harness
        buf = io.StringIO()
        with contextlib.redirect_stderr(buf):
            h._emit_virtio_input_events_inject_host_marker(ok=False, attempt=1, reason="bad|value\n")
        self.assertEqual(
            buf.getvalue().strip(),
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_EVENTS_INJECT|FAIL|attempt=1|reason=bad/value",
        )

    def test_media_keys_inject_pass_marker_format(self) -> None:
        h = self.harness
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            h._emit_virtio_input_media_keys_inject_host_marker(ok=True, attempt=2, kbd_mode="broadcast")
        out = buf.getvalue().strip()
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_MEDIA_KEYS_INJECT|PASS|attempt=2|kbd_mode=broadcast",
        )
        self.assertNotIn("qcode=", out, "media-keys inject marker should not include qcode fields")

    def test_media_keys_inject_fail_marker_format(self) -> None:
        h = self.harness
        buf = io.StringIO()
        with contextlib.redirect_stderr(buf):
            h._emit_virtio_input_media_keys_inject_host_marker(ok=False, attempt=9, reason="oops")
        self.assertEqual(
            buf.getvalue().strip(),
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_MEDIA_KEYS_INJECT|FAIL|attempt=9|reason=oops",
        )

    def test_tablet_events_inject_pass_marker_format(self) -> None:
        h = self.harness
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            h._emit_virtio_input_tablet_events_inject_host_marker(ok=True, attempt=4, tablet_mode="device")
        self.assertEqual(
            buf.getvalue().strip(),
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_TABLET_EVENTS_INJECT|PASS|attempt=4|tablet_mode=device",
        )

    def test_tablet_events_inject_fail_marker_format(self) -> None:
        h = self.harness
        buf = io.StringIO()
        with contextlib.redirect_stderr(buf):
            h._emit_virtio_input_tablet_events_inject_host_marker(
                ok=False, attempt=5, reason="bad|tablet\n"
            )
        self.assertEqual(
            buf.getvalue().strip(),
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_INPUT_TABLET_EVENTS_INJECT|FAIL|attempt=5|reason=bad/tablet",
        )


if __name__ == "__main__":
    unittest.main()

