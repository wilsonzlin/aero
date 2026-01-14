#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import contextlib
import importlib.util
import io
import subprocess
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


class QemuVirtioInputLedPreflightTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_with_input_led_requires_keyboard_and_mouse(self) -> None:
        h = self.harness
        _clear_qemu_probe_caches(h)

        def fake_run(cmd, **_kwargs):
            # cmd looks like: [qemu, "-device", "<dev>,help"]
            dev_arg = cmd[2]

            if dev_arg.startswith("virtio-keyboard-pci,help"):
                return subprocess.CompletedProcess(cmd, 0, stdout="virtio-keyboard-pci options:\n")
            if dev_arg.startswith("virtio-mouse-pci,help"):
                return subprocess.CompletedProcess(cmd, 1, stdout="Device 'virtio-mouse-pci' not found\n")

            return subprocess.CompletedProcess(cmd, 0, stdout="")

        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            disk = tmp / "win7.qcow2"
            disk.write_bytes(b"")
            serial = tmp / "serial.log"

            argv = [
                "invoke_aero_virtio_win7_tests.py",
                "--qemu-system",
                "qemu-system-x86_64",
                "--disk-image",
                str(disk),
                "--serial-log",
                str(serial),
                # Use transitional mode to avoid the contract-v1 QEMU feature preflight; this test
                # focuses on the virtio-input device existence check.
                "--virtio-transitional",
                "--with-input-led",
            ]

            stderr = io.StringIO()
            with (
                mock.patch.object(sys, "argv", argv),
                mock.patch.object(h.subprocess, "run", side_effect=fake_run),
                mock.patch.object(h.subprocess, "Popen") as mock_popen,
                mock.patch.object(h, "_ReusableTcpServer") as mock_httpd,
                mock.patch.object(h, "_UdpEchoServer") as mock_udp,
                contextlib.redirect_stderr(stderr),
            ):
                with self.assertRaises(SystemExit) as cm:
                    h.main()

            self.assertEqual(cm.exception.code, 2)
            err = stderr.getvalue()
            self.assertIn(
                "--with-input-led/--with-virtio-input-led/--require-virtio-input-led/--enable-virtio-input-led requires QEMU virtio-keyboard-pci and virtio-mouse-pci support",
                err,
            )
            mock_popen.assert_not_called()
            mock_httpd.assert_not_called()
            mock_udp.assert_not_called()


if __name__ == "__main__":
    unittest.main()
