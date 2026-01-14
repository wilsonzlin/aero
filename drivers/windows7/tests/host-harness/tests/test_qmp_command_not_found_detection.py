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


class QmpCommandNotFoundDetectionTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_detects_command_not_found_from_error_class(self) -> None:
        h = self.harness
        err = h._QmpCommandError(
            execute="input-send-event",
            resp={"error": {"class": "CommandNotFound", "desc": "missing"}},
        )
        self.assertTrue(h._qmp_error_is_command_not_found(err, command="input-send-event"))

    def test_does_not_match_different_command_for_structured_error(self) -> None:
        h = self.harness
        err = h._QmpCommandError(
            execute="send-key",
            resp={"error": {"class": "CommandNotFound", "desc": "missing"}},
        )
        self.assertFalse(h._qmp_error_is_command_not_found(err, command="input-send-event"))

    def test_detects_command_not_found_from_desc_without_class(self) -> None:
        h = self.harness
        err = h._QmpCommandError(
            execute="input-send-event",
            resp={"error": {"class": "GenericError", "desc": "The command input-send-event has not been found"}},
        )
        self.assertTrue(h._qmp_error_is_command_not_found(err, command="input-send-event"))

    def test_does_not_treat_device_not_found_as_missing_command(self) -> None:
        h = self.harness

        # When we have a structured QMP error, the command name is not usually present in the
        # DeviceNotFound description, so this should not be treated as command missing.
        err = h._QmpCommandError(
            execute="input-send-event",
            resp={"error": {"class": "DeviceNotFound", "desc": "Device 'aero_virtio_kbd0' has not been found"}},
        )
        self.assertFalse(h._qmp_error_is_command_not_found(err, command="input-send-event"))

        # Even when only a stringified error is available, avoid misclassifying a DeviceNotFound
        # error as "command not found" just because it contains "QMP command ..." and "has not been found".
        msg = "QMP command 'input-send-event' failed: DeviceNotFound: Device 'aero_virtio_kbd0' has not been found"
        self.assertFalse(h._qmp_error_is_command_not_found(RuntimeError(msg), command="input-send-event"))

    def test_detects_command_not_found_from_stringified_error(self) -> None:
        h = self.harness
        # Older QEMU phrasing may not include CommandNotFound class.
        msg = "The command input-send-event has not been found"
        self.assertTrue(h._qmp_error_is_command_not_found(RuntimeError(msg), command="input-send-event"))


if __name__ == "__main__":
    unittest.main()

