#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import contextlib
import importlib.util
import io
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock


def _load_harness():
    harness_path = Path(__file__).resolve().parents[1] / "invoke_aero_virtio_win7_tests.py"
    spec = importlib.util.spec_from_file_location("invoke_aero_virtio_win7_tests", harness_path)
    assert spec and spec.loader

    module = importlib.util.module_from_spec(spec)
    # Register the module before execution so dataclasses can resolve __module__ correctly.
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def _clear_qemu_probe_caches(h) -> None:
    for name in (
        "_qemu_device_help_text",
        "_qemu_device_property_names",
        "_qemu_device_list_help_text",
        "_qemu_has_device",
    ):
        fn = getattr(h, name, None)
        if fn is not None and hasattr(fn, "cache_clear"):
            fn.cache_clear()  # type: ignore[attr-defined]


class InputEventsMissingQemuSystemErrorTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_missing_qemu_system_surfaces_clear_error(self) -> None:
        """
        When QEMU is missing, required-device preflights (e.g. --with-input-events) should report that
        qemu-system itself is not available, rather than a misleading 'device not advertised' error.
        """
        h = self.harness
        _clear_qemu_probe_caches(h)

        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            disk = tmp / "win7.qcow2"
            disk.write_bytes(b"")

            argv = [
                "invoke_aero_virtio_win7_tests.py",
                "--qemu-system",
                "qemu-system-x86_64",
                "--disk-image",
                str(disk),
                "--with-input-events",
            ]

            stderr = io.StringIO()
            with (
                mock.patch.object(sys, "argv", argv),
                mock.patch.object(h.subprocess, "run", side_effect=FileNotFoundError()),
                contextlib.redirect_stderr(stderr),
            ):
                with self.assertRaises(SystemExit) as cm:
                    h.main()

        self.assertEqual(cm.exception.code, 2)
        err = stderr.getvalue()
        self.assertIn("qemu-system binary not found: qemu-system-x86_64", err)
        self.assertNotIn("requires QEMU virtio-keyboard-pci", err)


if __name__ == "__main__":
    unittest.main()

