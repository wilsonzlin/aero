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


class QemuStderrLogOpenFailureTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_opening_qemu_stderr_log_failure_is_handled(self) -> None:
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
                "--virtio-transitional",
                "--disable-udp",
            ]

            def fake_run(cmd, **_kwargs):
                # Only QEMU `-device <name>,help` probes should hit subprocess.run in this test.
                return subprocess.CompletedProcess(cmd, 0, stdout="ok\n")

            httpd = _DummyHttpd()
            stdout = io.StringIO()
            stderr = io.StringIO()

            real_open = Path.open

            def fake_open(self: Path, *args, **kwargs):
                # Only fail on the qemu stderr sidecar open.
                mode = args[0] if args else kwargs.get("mode", "r")
                if self.name.endswith(".qemu.stderr.log") and mode == "wb":
                    raise PermissionError("permission denied")
                return real_open(self, *args, **kwargs)

            with (
                mock.patch.object(sys, "argv", argv),
                mock.patch.object(h, "_ReusableTcpServer", return_value=httpd),
                mock.patch.object(h.subprocess, "run", side_effect=fake_run),
                mock.patch.object(h.subprocess, "Popen") as mock_popen,
                mock.patch("pathlib.Path.open", new=fake_open),
                contextlib.redirect_stdout(stdout),
                contextlib.redirect_stderr(stderr),
            ):
                rc = h.main()

            self.assertEqual(rc, 2)
            self.assertIn("failed to open QEMU stderr log", stderr.getvalue())
            self.assertTrue(httpd.shutdown_called)
            mock_popen.assert_not_called()


if __name__ == "__main__":
    unittest.main()
