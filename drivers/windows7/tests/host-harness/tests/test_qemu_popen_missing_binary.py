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


class _DummyHttpd:
    def __init__(self) -> None:
        self.shutdown_called = False

    def __enter__(self):
        return self

    def __exit__(self, _exc_type, _exc, _tb):
        return False

    def serve_forever(self) -> None:
        return None

    def shutdown(self) -> None:
        self.shutdown_called = True


class QemuPopenMissingBinaryTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_missing_qemu_system_binary_fails_cleanly_and_writes_stderr_log(self) -> None:
        h = self.harness

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
                # Transitional mode avoids the contract-v1 QEMU preflight so we can exercise
                # the `subprocess.Popen` launch error handling.
                "--virtio-transitional",
                "--disable-udp",
            ]

            httpd = _DummyHttpd()
            stdout = io.StringIO()
            stderr = io.StringIO()
            with (
                mock.patch.object(sys, "argv", argv),
                mock.patch.object(h, "_ReusableTcpServer", return_value=httpd),
                mock.patch.object(h.subprocess, "Popen", side_effect=FileNotFoundError()),
                contextlib.redirect_stdout(stdout),
                contextlib.redirect_stderr(stderr),
            ):
                rc = h.main()

            self.assertEqual(rc, 2)
            self.assertIn("ERROR: qemu-system binary not found:", stderr.getvalue())
            self.assertTrue(httpd.shutdown_called)

            qemu_stderr_log = serial.with_name(serial.stem + ".qemu.stderr.log")
            self.assertTrue(qemu_stderr_log.exists())
            self.assertIn(
                "qemu-system binary not found", qemu_stderr_log.read_text(encoding="utf-8", errors="replace")
            )


if __name__ == "__main__":
    unittest.main()
