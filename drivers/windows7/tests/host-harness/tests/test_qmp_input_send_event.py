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


class QmpInputSendEventTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_builds_input_send_event_command(self) -> None:
        h = self.harness

        keyboard_events = h._qmp_deterministic_keyboard_events(qcode="a")
        cmd = h._qmp_input_send_event_command(keyboard_events)

        self.assertEqual(cmd["execute"], "input-send-event")
        self.assertIn("arguments", cmd)
        args = cmd["arguments"]
        self.assertIn("events", args)
        self.assertIsInstance(args["events"], list)

        # Ensure it is JSON-serializable (QMP wire format).
        data = json.dumps(cmd)
        self.assertIn('"execute": "input-send-event"', data)

    def test_keyboard_events_use_qcode(self) -> None:
        h = self.harness

        ev = h._qmp_deterministic_keyboard_events(qcode="a")
        self.assertEqual(len(ev), 2)
        self.assertEqual(ev[0]["type"], "key")
        self.assertTrue(ev[0]["data"]["down"])
        self.assertEqual(ev[0]["data"]["key"]["type"], "qcode")
        self.assertEqual(ev[0]["data"]["key"]["data"], "a")

        self.assertEqual(ev[1]["type"], "key")
        self.assertFalse(ev[1]["data"]["down"])

    def test_mouse_events_include_rel_and_click(self) -> None:
        h = self.harness

        ev = h._qmp_deterministic_mouse_events()
        types = [e["type"] for e in ev]
        self.assertIn("rel", types)
        self.assertIn("btn", types)

        rel_axes = {e["data"]["axis"] for e in ev if e["type"] == "rel"}
        self.assertIn("x", rel_axes)
        self.assertIn("y", rel_axes)


if __name__ == "__main__":
    unittest.main()

