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


class DisableMsixMissingQemuSystemErrorTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_disable_msix_missing_qemu_system_reports_clear_error(self) -> None:
        """
        In transitional mode we skip the contract-v1 QEMU preflight. Ensure --virtio-disable-msix
        still reports missing qemu-system clearly (instead of attributing it to vectors=0 support).
        """
        h = self.harness
        _clear_qemu_probe_caches(h)

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
                "--virtio-transitional",
                "--virtio-disable-msix",
            ]

            stderr = io.StringIO()
            with (
                mock.patch.object(sys, "argv", argv),
                mock.patch.object(h.subprocess, "run", side_effect=FileNotFoundError()),
                mock.patch.object(h.subprocess, "Popen") as mock_popen,
                mock.patch.object(h, "_ReusableTcpServer") as mock_httpd,
                mock.patch.object(h, "_UdpEchoServer") as mock_udp,
                contextlib.redirect_stderr(stderr),
            ):
                rc = h.main()

            self.assertEqual(rc, 2)
            err = stderr.getvalue()
            self.assertIn("qemu-system binary not found: qemu-system-x86_64", err)
            self.assertNotIn("rejected 'vectors=0'", err)
            self.assertNotIn("does not advertise a 'vectors' property", err)
            mock_popen.assert_not_called()
            mock_httpd.assert_not_called()
            mock_udp.assert_not_called()

            qemu_stderr_log = serial.with_name(serial.stem + ".qemu.stderr.log")
            self.assertTrue(qemu_stderr_log.exists())
            self.assertIn(
                "qemu-system binary not found",
                qemu_stderr_log.read_text(encoding="utf-8", errors="replace"),
            )


if __name__ == "__main__":
    unittest.main()

