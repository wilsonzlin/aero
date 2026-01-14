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

    def test_mouse_events_include_wheel_when_enabled(self) -> None:
        h = self.harness

        ev = h._qmp_deterministic_mouse_events(with_wheel=True)
        rel_axes = {e["data"]["axis"] for e in ev if e["type"] == "rel"}
        self.assertIn("wheel", rel_axes)
        self.assertIn("hwheel", rel_axes)

    def test_tablet_events_include_abs_and_click(self) -> None:
        h = self.harness

        ev = h._qmp_deterministic_tablet_events()
        self.assertEqual(len(ev), 6)

        # Reset move (0,0) so repeated injections still produce movement reports.
        self.assertEqual(ev[0]["type"], "abs")
        self.assertEqual(ev[0]["data"]["axis"], "x")
        self.assertEqual(ev[0]["data"]["value"], 0)
        self.assertEqual(ev[1]["type"], "abs")
        self.assertEqual(ev[1]["data"]["axis"], "y")
        self.assertEqual(ev[1]["data"]["value"], 0)

        # Target move.
        self.assertEqual(ev[2]["type"], "abs")
        self.assertEqual(ev[2]["data"]["axis"], "x")
        self.assertEqual(ev[2]["data"]["value"], 10000)
        self.assertEqual(ev[3]["type"], "abs")
        self.assertEqual(ev[3]["data"]["axis"], "y")
        self.assertEqual(ev[3]["data"]["value"], 20000)

        # Left click.
        self.assertEqual(ev[4]["type"], "btn")
        self.assertEqual(ev[4]["data"]["button"], "left")
        self.assertTrue(ev[4]["data"]["down"])
        self.assertEqual(ev[5]["type"], "btn")
        self.assertEqual(ev[5]["data"]["button"], "left")
        self.assertFalse(ev[5]["data"]["down"])

        abs_axes = {e["data"]["axis"] for e in ev if e["type"] == "abs"}
        self.assertIn("x", abs_axes)
        self.assertIn("y", abs_axes)

    def test_tablet_events_support_custom_target_coords(self) -> None:
        h = self.harness

        ev = h._qmp_deterministic_tablet_events(x=1234, y=5678)
        self.assertEqual(ev[2]["type"], "abs")
        self.assertEqual(ev[2]["data"]["axis"], "x")
        self.assertEqual(ev[2]["data"]["value"], 1234)
        self.assertEqual(ev[3]["type"], "abs")
        self.assertEqual(ev[3]["data"]["axis"], "y")
        self.assertEqual(ev[3]["data"]["value"], 5678)


if __name__ == "__main__":
    unittest.main()
