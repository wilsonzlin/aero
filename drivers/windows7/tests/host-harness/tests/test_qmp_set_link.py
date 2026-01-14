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

    def test_try_qmp_set_link_treats_generic_error_command_not_found_as_unsupported(self) -> None:
        """
        Older QEMU builds may report unknown QMP commands as GenericError with a descriptive
        "The command <name> has not been found" string (instead of CommandNotFound class).
        Ensure the harness still reports set_link as unsupported in that case.
        """
        h = self.harness

        class _DummySock:
            def __enter__(self):  # noqa: ANN001
                return self

            def __exit__(self, exc_type, exc, tb):  # noqa: ANN001
                return False

        resp = {
            "error": {
                "class": "GenericError",
                "desc": "The command set_link has not been found",
            }
        }

        old_connect = h._qmp_connect
        old_send_raw = h._qmp_send_command_raw
        try:
            h._qmp_connect = lambda endpoint, timeout_seconds=5.0: _DummySock()  # type: ignore[assignment]
            h._qmp_send_command_raw = lambda sock, cmd: resp  # type: ignore[assignment]

            with self.assertRaises(RuntimeError) as cm:
                h._try_qmp_set_link(
                    h._QmpEndpoint(tcp_host="127.0.0.1", tcp_port=1),
                    name="aero_virtio_net0",
                    up=False,
                )
            self.assertIn("unsupported QEMU", str(cm.exception))
            self.assertIn("set_link", str(cm.exception))
        finally:
            h._qmp_connect = old_connect
            h._qmp_send_command_raw = old_send_raw


if __name__ == "__main__":
    unittest.main()
