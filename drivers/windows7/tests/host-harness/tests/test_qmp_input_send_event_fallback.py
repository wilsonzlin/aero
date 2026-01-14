#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import importlib.util
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


class QmpInputSendEventFallbackTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_falls_back_to_hmp_when_input_send_event_command_missing(self) -> None:
        h = self.harness

        sent_hmp: list[str] = []
        sent_qmp: list[dict[str, object]] = []

        def fake_connect(endpoint, *, timeout_seconds: float = 5.0):
            return _DummySock()

        def fake_send_command(sock, cmd):
            sent_qmp.append(cmd)
            if cmd.get("execute") == "input-send-event":
                raise h._QmpCommandError(
                    execute="input-send-event",
                    resp={
                        "error": {
                            "class": "CommandNotFound",
                            "desc": "The command input-send-event has not been found",
                        }
                    },
                )
            if cmd.get("execute") == "send-key":
                # Force the harness down the `human-monitor-command: sendkey` path (so the test can
                # validate HMP injection was attempted).
                raise h._QmpCommandError(
                    execute="send-key",
                    resp={
                        "error": {
                            "class": "CommandNotFound",
                            "desc": "The command send-key has not been found",
                        }
                    },
                )
            if cmd.get("execute") == "human-monitor-command":
                sent_hmp.append(str(cmd.get("arguments", {}).get("command-line")))
                return {"return": ""}
            raise AssertionError(f"unexpected QMP command: {cmd}")

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

            self.assertEqual(info.backend, "hmp_fallback")
            # HMP cannot target QOM input device ids.
            self.assertIsNone(info.keyboard_device)
            self.assertIsNone(info.mouse_device)

            # Ensure we ran the expected HMP commands.
            self.assertEqual(
                sent_hmp,
                [
                    "sendkey a",
                    "mouse_move 10 5",
                    "mouse_button 1",
                    "mouse_button 0",
                ],
            )
        finally:
            h._qmp_connect = old_connect
            h._qmp_send_command = old_send
            h.time.sleep = old_sleep


if __name__ == "__main__":
    unittest.main()
