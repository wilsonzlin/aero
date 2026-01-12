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


class QmpInputEventFormattingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_key_event(self) -> None:
        f = self.harness._qmp_key_event
        self.assertEqual(
            f("a", down=True),
            {
                "type": "key",
                "data": {"down": True, "key": {"type": "qcode", "data": "a"}},
            },
        )

    def test_btn_event(self) -> None:
        f = self.harness._qmp_btn_event
        self.assertEqual(
            f("left", down=False),
            {"type": "btn", "data": {"down": False, "button": "left"}},
        )

    def test_rel_event(self) -> None:
        f = self.harness._qmp_rel_event
        self.assertEqual(
            f("x", 10),
            {"type": "rel", "data": {"axis": "x", "value": 10}},
        )

    def test_input_send_event_cmd_with_device(self) -> None:
        f = self.harness._qmp_input_send_event_cmd
        evt = self.harness._qmp_key_event("a", down=True)
        self.assertEqual(
            f([evt], device="kbd0"),
            {"execute": "input-send-event", "arguments": {"device": "kbd0", "events": [evt]}},
        )

    def test_input_send_event_cmd_without_device(self) -> None:
        f = self.harness._qmp_input_send_event_cmd
        evt = self.harness._qmp_key_event("a", down=True)
        self.assertEqual(
            f([evt], device=None),
            {"execute": "input-send-event", "arguments": {"events": [evt]}},
        )


if __name__ == "__main__":
    unittest.main()

