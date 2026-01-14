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

    def test_injects_wheel_events_when_enabled(self) -> None:
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
                h._QmpEndpoint(tcp_host="127.0.0.1", tcp_port=4444), with_wheel=True
            )

            self.assertEqual(info.keyboard_device, h._VIRTIO_INPUT_QMP_KEYBOARD_ID)
            self.assertEqual(info.mouse_device, h._VIRTIO_INPUT_QMP_MOUSE_ID)

            # 5 commands: key down, key up, mouse rel (x/y/wheel/hscroll), left down, left up.
            self.assertEqual(len(sent), 5)
            rel_events = sent[2]["arguments"]["events"]
            axes = {e["data"]["axis"] for e in rel_events if e["type"] == "rel"}
            self.assertIn("x", axes)
            self.assertIn("y", axes)
            self.assertIn("wheel", axes)
            self.assertIn("hscroll", axes)
        finally:
            h._qmp_connect = old_connect
            h._qmp_send_command = old_send
            h.time.sleep = old_sleep

    def test_falls_back_to_hwheel_axis_when_hscroll_rejected(self) -> None:
        h = self.harness

        sent: list[dict[str, object]] = []

        def fake_connect(endpoint, *, timeout_seconds: float = 5.0):
            return _DummySock()

        def fake_send_command(sock, cmd):
            sent.append(cmd)
            # Simulate a QEMU build that rejects `axis=hscroll` but accepts `axis=hwheel`.
            evs = cmd.get("arguments", {}).get("events", [])
            for ev in evs:
                if ev.get("type") == "rel" and ev.get("data", {}).get("axis") == "hscroll":
                    raise RuntimeError("axis hscroll unsupported")
            return {"return": {}}

        old_connect = h._qmp_connect
        old_send = h._qmp_send_command
        old_sleep = h.time.sleep
        try:
            h._qmp_connect = fake_connect
            h._qmp_send_command = fake_send_command
            h.time.sleep = lambda _: None

            info = h._try_qmp_input_inject_virtio_input_events(
                h._QmpEndpoint(tcp_host="127.0.0.1", tcp_port=4444), with_wheel=True
            )

            self.assertEqual(info.keyboard_device, h._VIRTIO_INPUT_QMP_KEYBOARD_ID)
            self.assertEqual(info.mouse_device, h._VIRTIO_INPUT_QMP_MOUSE_ID)

            # 7 commands total:
            # - key down, key up
            # - mouse rel: device attempt (fails), broadcast attempt (fails), fallback axis attempt (succeeds)
            # - left down, left up
            self.assertEqual(len(sent), 7)

            rel_cmds = [
                cmd
                for cmd in sent
                if any(e.get("type") == "rel" for e in cmd.get("arguments", {}).get("events", []))
            ]
            self.assertEqual(len(rel_cmds), 3)

            rel_axes_sets = [
                {e["data"]["axis"] for e in cmd["arguments"]["events"] if e["type"] == "rel"}
                for cmd in rel_cmds
            ]
            # First two rel sends contain hscroll and fail.
            self.assertTrue(any("hscroll" in axes for axes in rel_axes_sets))
            # Final rel send should use hwheel fallback and succeed.
            self.assertTrue(any("hwheel" in axes for axes in rel_axes_sets))
        finally:
            h._qmp_connect = old_connect
            h._qmp_send_command = old_send
            h.time.sleep = old_sleep

    def test_falls_back_to_vscroll_axis_when_wheel_rejected(self) -> None:
        h = self.harness

        sent: list[dict[str, object]] = []

        def fake_connect(endpoint, *, timeout_seconds: float = 5.0):
            return _DummySock()

        def fake_send_command(sock, cmd):
            sent.append(cmd)
            # Simulate a QEMU build that rejects `axis=wheel` but accepts `axis=vscroll`.
            evs = cmd.get("arguments", {}).get("events", [])
            for ev in evs:
                if ev.get("type") == "rel" and ev.get("data", {}).get("axis") == "wheel":
                    raise RuntimeError("axis wheel unsupported")
            return {"return": {}}

        old_connect = h._qmp_connect
        old_send = h._qmp_send_command
        old_sleep = h.time.sleep
        try:
            h._qmp_connect = fake_connect
            h._qmp_send_command = fake_send_command
            h.time.sleep = lambda _: None

            info = h._try_qmp_input_inject_virtio_input_events(
                h._QmpEndpoint(tcp_host="127.0.0.1", tcp_port=4444), with_wheel=True
            )

            self.assertEqual(info.keyboard_device, h._VIRTIO_INPUT_QMP_KEYBOARD_ID)
            self.assertEqual(info.mouse_device, h._VIRTIO_INPUT_QMP_MOUSE_ID)

            rel_cmds = [
                cmd
                for cmd in sent
                if any(e.get("type") == "rel" for e in cmd.get("arguments", {}).get("events", []))
            ]
            rel_axes_sets = [
                {e["data"]["axis"] for e in cmd["arguments"]["events"] if e["type"] == "rel"}
                for cmd in rel_cmds
            ]
            self.assertTrue(any("wheel" in axes for axes in rel_axes_sets))
            self.assertTrue(any("vscroll" in axes for axes in rel_axes_sets))
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

    def test_injects_extended_events_when_requested(self) -> None:
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
                h._QmpEndpoint(tcp_host="127.0.0.1", tcp_port=4444),
                extended=True,
            )

            self.assertEqual(info.keyboard_device, h._VIRTIO_INPUT_QMP_KEYBOARD_ID)
            self.assertEqual(info.mouse_device, h._VIRTIO_INPUT_QMP_MOUSE_ID)

            expected = (
                5
                + len(h._qmp_deterministic_keyboard_modifier_events())
                + len(h._qmp_deterministic_mouse_extra_button_events())
            )
            self.assertEqual(len(sent), expected)

            # Extended injection should also include wheel/hscroll rel axes.
            rel_events = sent[2]["arguments"]["events"]
            axes = {e["data"]["axis"] for e in rel_events if e["type"] == "rel"}
            self.assertIn("wheel", axes)
            self.assertIn("hscroll", axes)

            # Ensure at least one side/extra button event was included.
            all_events = [e for cmd in sent for e in cmd["arguments"]["events"]]
            buttons = [e["data"]["button"] for e in all_events if e["type"] == "btn"]
            self.assertIn("side", buttons)
            self.assertIn("extra", buttons)
        finally:
            h._qmp_connect = old_connect
            h._qmp_send_command = old_send
            h.time.sleep = old_sleep

    def test_injects_tablet_events_targeted_by_device_id_when_supported(self) -> None:
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

            info = h._try_qmp_input_inject_virtio_input_tablet_events(
                h._QmpEndpoint(tcp_host="127.0.0.1", tcp_port=4444)
            )

            self.assertEqual(info.tablet_device, h._VIRTIO_INPUT_QMP_TABLET_ID)
            self.assertEqual(len(sent), 4)
            for cmd in sent:
                self.assertEqual(cmd["execute"], "input-send-event")
                self.assertEqual(cmd["arguments"]["device"], h._VIRTIO_INPUT_QMP_TABLET_ID)

            # Reset move, target move, click down, click up.
            self.assertEqual(
                sent[0]["arguments"]["events"],
                [
                    {"type": "abs", "data": {"axis": "x", "value": 0}},
                    {"type": "abs", "data": {"axis": "y", "value": 0}},
                ],
            )
            self.assertEqual(
                sent[1]["arguments"]["events"],
                [
                    {"type": "abs", "data": {"axis": "x", "value": 10000}},
                    {"type": "abs", "data": {"axis": "y", "value": 20000}},
                ],
            )
            self.assertEqual(
                sent[2]["arguments"]["events"],
                [{"type": "btn", "data": {"down": True, "button": "left"}}],
            )
            self.assertEqual(
                sent[3]["arguments"]["events"],
                [{"type": "btn", "data": {"down": False, "button": "left"}}],
            )
        finally:
            h._qmp_connect = old_connect
            h._qmp_send_command = old_send
            h.time.sleep = old_sleep

    def test_injects_tablet_events_falls_back_to_broadcast_when_device_routing_rejected(self) -> None:
        h = self.harness

        sent: list[dict[str, object]] = []

        def fake_connect(endpoint, *, timeout_seconds: float = 5.0):
            return _DummySock()

        def fake_send_command(sock, cmd):
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
                info = h._try_qmp_input_inject_virtio_input_tablet_events(
                    h._QmpEndpoint(tcp_host="127.0.0.1", tcp_port=4444)
                )

            self.assertIsNone(info.tablet_device)
            # First send should attempt device + broadcast, remaining sends should be broadcast-only.
            self.assertEqual(len(sent), 5)
            device_attempts = [cmd for cmd in sent if "device" in cmd.get("arguments", {})]
            self.assertEqual(len(device_attempts), 1)
            self.assertEqual(device_attempts[0]["arguments"]["device"], h._VIRTIO_INPUT_QMP_TABLET_ID)
        finally:
            h._qmp_connect = old_connect
            h._qmp_send_command = old_send
            h.time.sleep = old_sleep


if __name__ == "__main__":
    unittest.main()
