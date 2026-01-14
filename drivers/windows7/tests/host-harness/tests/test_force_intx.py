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


class ForceIntxTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def _parse(self, argv: list[str]):
        parser = self.harness._build_arg_parser()
        args, extra = parser.parse_known_args(argv)
        self.assertEqual(extra, [])
        return parser, args

    def test_force_intx_flag_parses(self) -> None:
        _, args = self._parse(
            [
                "--qemu-system",
                "qemu-system-x86_64",
                "--disk-image",
                "disk.img",
                "--force-intx",
            ]
        )
        self.assertTrue(args.virtio_disable_msix)

    def test_intx_only_alias_parses(self) -> None:
        _, args = self._parse(
            [
                "--qemu-system",
                "qemu-system-x86_64",
                "--disk-image",
                "disk.img",
                "--intx-only",
            ]
        )
        self.assertTrue(args.virtio_disable_msix)

    def test_dry_run_with_force_intx_prints_vectors_zero(self) -> None:
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
                "--force-intx",
                "--dry-run",
            ]

            out = io.StringIO()
            with (
                mock.patch.object(sys, "argv", argv),
                mock.patch.object(self.harness.subprocess, "run") as mock_run,
                mock.patch.object(self.harness.subprocess, "Popen") as mock_popen,
                mock.patch.object(
                    self.harness.socket, "socket", side_effect=AssertionError("unexpected socket usage in dry-run")
                ),
                contextlib.redirect_stdout(out),
                contextlib.redirect_stderr(io.StringIO()),
            ):
                rc = self.harness.main()

            self.assertEqual(rc, 0)
            stdout = out.getvalue()
            self.assertIn("vectors=0", stdout)

            # First line should be JSON argv array and should include vectors=0 on at least one device.
            first_line = stdout.splitlines()[0]
            parsed = json.loads(first_line)
            self.assertTrue(any(isinstance(x, str) and "vectors=0" in x for x in parsed))

            mock_run.assert_not_called()
            mock_popen.assert_not_called()


if __name__ == "__main__":
    unittest.main()
