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


class QmpSetLinkTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_builds_set_link_command(self) -> None:
        h = self.harness
        cmd = h._qmp_set_link_command(name="aero_virtio_net0", up=False)
        self.assertEqual(cmd["execute"], "set_link")
        self.assertIn("arguments", cmd)
        args = cmd["arguments"]
        self.assertEqual(args["name"], "aero_virtio_net0")
        self.assertIs(args["up"], False)

        # Ensure it is JSON-serializable (QMP wire format).
        data = json.dumps(cmd)
        self.assertIn('"execute": "set_link"', data)


if __name__ == "__main__":
    unittest.main()
