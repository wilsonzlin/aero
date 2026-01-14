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


class HarnessDryRunTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_dry_run_prints_qemu_args_and_exits_before_side_effects(self) -> None:
        h = self.harness

        with tempfile.TemporaryDirectory() as td:
            disk = Path(td) / "disk.img"
            disk.write_bytes(b"")
            serial = Path(td) / "serial.log"

            old_argv = sys.argv
            try:
                sys.argv = [
                    "invoke_aero_virtio_win7_tests.py",
                    "--qemu-system",
                    "qemu-system-x86_64",
                    "--disk-image",
                    str(disk),
                    "--serial-log",
                    str(serial),
                    "--dry-run",
                ]

                stdout = io.StringIO()
                stderr = io.StringIO()
                with (
                    mock.patch.object(
                        h.subprocess,
                        "run",
                        side_effect=AssertionError("subprocess.run should not be called in --dry-run"),
                    ) as run_mock,
                    mock.patch.object(
                        h.subprocess,
                        "Popen",
                        side_effect=AssertionError("subprocess.Popen should not be called in --dry-run"),
                    ) as popen_mock,
                    mock.patch.object(
                        h.socket.socket,
                        "bind",
                        side_effect=AssertionError("socket.bind should not be called in --dry-run"),
                    ) as bind_mock,
                    contextlib.redirect_stdout(stdout),
                    contextlib.redirect_stderr(stderr),
                ):
                    rc = h.main()

                self.assertEqual(rc, 0)
                self.assertEqual(run_mock.call_count, 0)
                self.assertEqual(popen_mock.call_count, 0)
                self.assertEqual(bind_mock.call_count, 0)

                out = stdout.getvalue()
                self.assertIn("qemu-system-x86_64", out)
                self.assertIn("-device virtio-net-pci", out)
            finally:
                sys.argv = old_argv


if __name__ == "__main__":
    unittest.main()

