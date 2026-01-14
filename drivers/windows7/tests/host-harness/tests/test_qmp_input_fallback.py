#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import importlib.util
import json
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


class QmpFallbackCommandFormattingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_builds_send_key_command(self) -> None:
        h = self.harness
        cmd = h._qmp_send_key_command(qcodes=["a"], hold_time_ms=50)
        self.assertEqual(
            cmd,
            {
                "execute": "send-key",
                "arguments": {"keys": [{"type": "qcode", "data": "a"}], "hold-time": 50},
            },
        )
        # Ensure it is JSON-serializable (QMP wire format).
        self.assertIn('"execute":"send-key"', json.dumps(cmd, separators=(",", ":")))

    def test_builds_human_monitor_command(self) -> None:
        h = self.harness
        cmd = h._qmp_human_monitor_command(command_line="mouse_move 10 5")
        self.assertEqual(
            cmd,
            {
                "execute": "human-monitor-command",
                "arguments": {"command-line": "mouse_move 10 5"},
            },
        )
        self.assertIn('"execute":"human-monitor-command"', json.dumps(cmd, separators=(",", ":")))


class QmpFallbackSelectionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_falls_back_when_input_send_event_is_missing_send_key_available(self) -> None:
        h = self.harness
        sent: list[dict[str, object]] = []

        def fake_connect(endpoint, *, timeout_seconds: float = 5.0):
            return _DummySock()

        def fake_send_command(sock, cmd):
            sent.append(cmd)
            if cmd.get("execute") == "input-send-event":
                raise h._QmpCommandError(
                    execute="input-send-event",
                    resp={"error": {"class": "CommandNotFound", "desc": "missing"}},
                )
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

            self.assertIsNone(info.keyboard_device)
            self.assertIsNone(info.mouse_device)

            # 1x failing input-send-event attempt, then:
            # - 1x send-key for keyboard
            # - 3x HMP mouse commands (mouse_move + mouse_button down/up)
            self.assertEqual(len(sent), 5)
            self.assertEqual(sent[0]["execute"], "input-send-event")
            self.assertEqual(sent[1]["execute"], "send-key")
            self.assertEqual(sent[2]["execute"], "human-monitor-command")
            self.assertEqual(sent[2]["arguments"]["command-line"], "mouse_move 10 5")
            self.assertEqual(sent[3]["arguments"]["command-line"], "mouse_button 1")
            self.assertEqual(sent[4]["arguments"]["command-line"], "mouse_button 0")
        finally:
            h._qmp_connect = old_connect
            h._qmp_send_command = old_send
            h.time.sleep = old_sleep

    def test_falls_back_when_input_send_event_and_send_key_are_missing(self) -> None:
        h = self.harness
        sent: list[dict[str, object]] = []

        def fake_connect(endpoint, *, timeout_seconds: float = 5.0):
            return _DummySock()

        def fake_send_command(sock, cmd):
            sent.append(cmd)
            if cmd.get("execute") == "input-send-event":
                raise h._QmpCommandError(
                    execute="input-send-event",
                    resp={"error": {"class": "CommandNotFound", "desc": "missing"}},
                )
            if cmd.get("execute") == "send-key":
                raise h._QmpCommandError(
                    execute="send-key",
                    resp={"error": {"class": "CommandNotFound", "desc": "missing"}},
                )
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

            self.assertIsNone(info.keyboard_device)
            self.assertIsNone(info.mouse_device)

            # 1x failing input-send-event attempt, 1x failing send-key attempt, then:
            # - 1x HMP sendkey
            # - 3x HMP mouse commands
            self.assertEqual(len(sent), 6)
            self.assertEqual(sent[0]["execute"], "input-send-event")
            self.assertEqual(sent[1]["execute"], "send-key")
            self.assertEqual(sent[2]["execute"], "human-monitor-command")
            self.assertEqual(sent[2]["arguments"]["command-line"], "sendkey a")
            self.assertEqual(sent[3]["arguments"]["command-line"], "mouse_move 10 5")
            self.assertEqual(sent[4]["arguments"]["command-line"], "mouse_button 1")
            self.assertEqual(sent[5]["arguments"]["command-line"], "mouse_button 0")
        finally:
            h._qmp_connect = old_connect
            h._qmp_send_command = old_send
            h.time.sleep = old_sleep

    def test_tablet_events_fail_clearly_when_input_send_event_is_missing(self) -> None:
        h = self.harness

        def fake_connect(endpoint, *, timeout_seconds: float = 5.0):
            return _DummySock()

        def fake_send_command(sock, cmd):
            if cmd.get("execute") == "input-send-event":
                raise h._QmpCommandError(
                    execute="input-send-event",
                    resp={"error": {"class": "CommandNotFound", "desc": "missing"}},
                )
            return {"return": {}}

        old_connect = h._qmp_connect
        old_send = h._qmp_send_command
        old_sleep = h.time.sleep
        try:
            h._qmp_connect = fake_connect
            h._qmp_send_command = fake_send_command
            h.time.sleep = lambda _: None

            with self.assertRaises(RuntimeError) as ctx:
                h._try_qmp_input_inject_virtio_input_tablet_events(
                    h._QmpEndpoint(tcp_host="127.0.0.1", tcp_port=4444)
                )
            self.assertIn("input-send-event", str(ctx.exception))
            self.assertIn("--with-input-tablet-events", str(ctx.exception))
        finally:
            h._qmp_connect = old_connect
            h._qmp_send_command = old_send
            h.time.sleep = old_sleep


if __name__ == "__main__":
    unittest.main()
