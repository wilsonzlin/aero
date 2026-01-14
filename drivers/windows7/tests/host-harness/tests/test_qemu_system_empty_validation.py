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


class QemuSystemEmptyValidationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_empty_qemu_system_fails_fast(self) -> None:
        h = self.harness
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            disk = tmp / "disk.img"
            disk.write_bytes(b"")

            argv = [
                "invoke_aero_virtio_win7_tests.py",
                "--qemu-system",
                "",
                "--disk-image",
                str(disk),
                "--dry-run",
            ]

            stderr = io.StringIO()
            with (
                mock.patch.object(sys, "argv", argv),
                contextlib.redirect_stderr(stderr),
                mock.patch.object(h.subprocess, "Popen") as mock_popen,
                mock.patch.object(h, "_ReusableTcpServer") as mock_httpd,
                mock.patch.object(h, "_UdpEchoServer") as mock_udp,
            ):
                rc = h.main()

        self.assertEqual(rc, 2)
        self.assertIn("qemu system binary is empty", stderr.getvalue())
        mock_popen.assert_not_called()
        mock_httpd.assert_not_called()
        mock_udp.assert_not_called()


if __name__ == "__main__":
    unittest.main()

