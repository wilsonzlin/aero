#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import contextlib
import importlib.util
import io
import json
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


class DryRunQemuCmdTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_dry_run_prints_qemu_cmd_and_does_not_spawn_subprocess(self) -> None:
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
                "--dry-run",
            ]

            out = io.StringIO()
            with (
                mock.patch.object(sys, "argv", argv),
                mock.patch.object(self.harness.subprocess, "run") as mock_run,
                mock.patch.object(self.harness.subprocess, "Popen") as mock_popen,
                contextlib.redirect_stdout(out),
            ):
                rc = self.harness.main()

            self.assertEqual(rc, 0)

            stdout = out.getvalue()
            # Expect the modern-only virtio-net device arg in default (contract-v1) mode.
            self.assertIn(
                "-device virtio-net-pci,netdev=net0,disable-legacy=on,x-pci-revision=0x01",
                stdout,
            )

            # First line should be JSON argv.
            first_line = stdout.splitlines()[0]
            parsed = json.loads(first_line)
            self.assertIsInstance(parsed, list)
            self.assertIn(
                "virtio-net-pci,netdev=net0,disable-legacy=on,x-pci-revision=0x01",
                parsed,
            )

            mock_run.assert_not_called()
            mock_popen.assert_not_called()


if __name__ == "__main__":
    unittest.main()

