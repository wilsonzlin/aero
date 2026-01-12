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


class _DummySock:
    def __enter__(self) -> "_DummySock":
        return self

    def __exit__(self, exc_type, exc, tb) -> bool:
        return False


class QmpInputInjectionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_injects_events_targeted_by_device_id_when_supported(self) -> None:
        h = self.harness

        sent: list[dict[str, object]] = []

        def fake_connect(endpoint, *, timeout_seconds: float = 5.0):
            return _DummySock()

        def fake_send_command(sock, cmd):
            sent.append(cmd)
            return {"return": {}}

        old_connect = h._qmp_connect
        old_send = h._qmp_send_command
        old_sleep = h.time.sleep
        try:
            h._qmp_connect = fake_connect
            h._qmp_send_command = fake_send_command
            h.time.sleep = lambda _: None

            info = h._try_qmp_input_inject_virtio_input_events(
                h._QmpEndpoint(tcp_host="127.0.0.1", tcp_port=4444)
            )

            self.assertEqual(info.keyboard_device, h._VIRTIO_INPUT_QMP_KEYBOARD_ID)
            self.assertEqual(info.mouse_device, h._VIRTIO_INPUT_QMP_MOUSE_ID)

            # 5 commands: key down, key up, mouse move (2 rel events), left down, left up.
            self.assertEqual(len(sent), 5)
            for cmd in sent:
                self.assertEqual(cmd["execute"], "input-send-event")
                self.assertIn("arguments", cmd)
                self.assertIn("events", cmd["arguments"])

            self.assertEqual(sent[0]["arguments"]["device"], h._VIRTIO_INPUT_QMP_KEYBOARD_ID)
            self.assertEqual(sent[1]["arguments"]["device"], h._VIRTIO_INPUT_QMP_KEYBOARD_ID)
            self.assertEqual(sent[2]["arguments"]["device"], h._VIRTIO_INPUT_QMP_MOUSE_ID)
            self.assertEqual(sent[3]["arguments"]["device"], h._VIRTIO_INPUT_QMP_MOUSE_ID)
            self.assertEqual(sent[4]["arguments"]["device"], h._VIRTIO_INPUT_QMP_MOUSE_ID)
        finally:
            h._qmp_connect = old_connect
            h._qmp_send_command = old_send
            h.time.sleep = old_sleep

    def test_falls_back_to_broadcast_when_device_routing_rejected(self) -> None:
        h = self.harness

        sent: list[dict[str, object]] = []

        def fake_connect(endpoint, *, timeout_seconds: float = 5.0):
            return _DummySock()

        def fake_send_command(sock, cmd):
            # Record the attempted command even when we reject it.
            sent.append(cmd)
            if "device" in cmd.get("arguments", {}):
                raise RuntimeError("device routing unsupported")
            return {"return": {}}

        old_connect = h._qmp_connect
        old_send = h._qmp_send_command
        old_sleep = h.time.sleep
        try:
            h._qmp_connect = fake_connect
            h._qmp_send_command = fake_send_command
            h.time.sleep = lambda _: None

            with contextlib.redirect_stderr(io.StringIO()):
                info = h._try_qmp_input_inject_virtio_input_events(
                    h._QmpEndpoint(tcp_host="127.0.0.1", tcp_port=4444)
                )

            self.assertIsNone(info.keyboard_device)
            self.assertIsNone(info.mouse_device)

            # We should have attempted device-targeted sends for the first keyboard and first mouse command,
            # and then retried without `device`.
            #
            # Total:
            # - key down: 2 attempts (device rejected + broadcast)
            # - key up: 1 attempt (broadcast, since device became None)
            # - mouse move: 2 attempts (device rejected + broadcast)
            # - mouse down: 1 attempt (broadcast)
            # - mouse up: 1 attempt (broadcast)
            self.assertEqual(len(sent), 7)

            device_attempts = [cmd for cmd in sent if "device" in cmd.get("arguments", {})]
            self.assertEqual(len(device_attempts), 2)
            self.assertEqual(device_attempts[0]["arguments"]["device"], h._VIRTIO_INPUT_QMP_KEYBOARD_ID)
            self.assertEqual(device_attempts[1]["arguments"]["device"], h._VIRTIO_INPUT_QMP_MOUSE_ID)
        finally:
            h._qmp_connect = old_connect
            h._qmp_send_command = old_send
            h.time.sleep = old_sleep


if __name__ == "__main__":
    unittest.main()

