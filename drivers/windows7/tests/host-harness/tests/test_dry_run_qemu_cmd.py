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
from pathlib import Path, PosixPath
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
                mock.patch.object(
                    self.harness.socket, "socket", side_effect=AssertionError("unexpected socket usage in dry-run")
                ),
                contextlib.redirect_stdout(out),
            ):
                rc = self.harness.main()

            self.assertEqual(rc, 0)

            stdout = out.getvalue()
            self.assertIn("qemu-system", stdout)
            # Expect the modern-only virtio-net device arg in default (contract-v1) mode.
            self.assertIn(
                "-device virtio-net-pci,id=aero_virtio_net0,netdev=net0,disable-legacy=on,x-pci-revision=0x01",
                stdout,
            )

            # First line should be JSON argv.
            first_line = stdout.splitlines()[0]
            parsed = json.loads(first_line)
            self.assertIsInstance(parsed, list)
            self.assertIn(
                "virtio-net-pci,id=aero_virtio_net0,netdev=net0,disable-legacy=on,x-pci-revision=0x01",
                parsed,
            )

            mock_run.assert_not_called()
            mock_popen.assert_not_called()
            # Dry-run should not start any host-side servers.
            # (No HTTP server, UDP echo server, etc.)

    def test_dry_run_does_not_start_host_servers(self) -> None:
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
                mock.patch.object(self.harness, "_ReusableTcpServer") as mock_httpd,
                mock.patch.object(self.harness, "_UdpEchoServer") as mock_udp,
                contextlib.redirect_stdout(out),
            ):
                rc = self.harness.main()

            self.assertEqual(rc, 0)
            mock_run.assert_not_called()
            mock_popen.assert_not_called()
            mock_httpd.assert_not_called()
            mock_udp.assert_not_called()

    def test_dry_run_with_virtio_disable_msix_does_not_spawn_subprocess(self) -> None:
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
                "--virtio-disable-msix",
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

    def test_dry_run_windows_cmdline_uses_list2cmdline(self) -> None:
        """
        The harness prints a second-line single-string command for copy/paste.

        On Windows (`os.name == "nt"`) this should use subprocess.list2cmdline rather than POSIX shlex quoting.
        """
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
                mock.patch.object(self.harness.os, "name", "nt"),
                # The harness uses `pathlib.Path`, which selects WindowsPath when os.name == "nt".
                # On non-Windows hosts that raises NotImplementedError. Override the harness's
                # Path binding so we can exercise Windows quoting behavior in CI.
                mock.patch.object(self.harness, "Path", PosixPath),
                mock.patch.object(self.harness.subprocess, "run") as mock_run,
                mock.patch.object(self.harness.subprocess, "Popen") as mock_popen,
                contextlib.redirect_stdout(out),
            ):
                rc = self.harness.main()

            self.assertEqual(rc, 0)
            lines = out.getvalue().splitlines()
            self.assertGreaterEqual(len(lines), 2)
            argv_list = json.loads(lines[0])
            expected = self.harness.subprocess.list2cmdline([str(a) for a in argv_list])
            self.assertEqual(lines[1], expected)

            mock_run.assert_not_called()
            mock_popen.assert_not_called()

    def test_dry_run_with_virtio_msix_vectors_does_not_spawn_subprocess(self) -> None:
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
                "--virtio-msix-vectors",
                "4",
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
            self.assertIn("vectors=4", stdout)

            mock_run.assert_not_called()
            mock_popen.assert_not_called()

    def test_dry_run_with_qemu_preflight_pci_enables_qmp(self) -> None:
        """
        The QMP `query-pci` preflight is optional, but when enabled it must force QMP on even if no
        other harness feature requires it.
        """
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
                "--qemu-preflight-pci",
                "--dry-run",
            ]

            out = io.StringIO()
            with (
                mock.patch.object(sys, "argv", argv),
                mock.patch.object(self.harness.subprocess, "run") as mock_run,
                mock.patch.object(self.harness.subprocess, "Popen") as mock_popen,
                mock.patch.object(
                    self.harness.socket, "socket", side_effect=AssertionError("unexpected socket usage in dry-run")
                )
                if self.harness.os.name != "nt"
                else contextlib.nullcontext(),
                contextlib.redirect_stdout(out),
            ):
                rc = self.harness.main()

            self.assertEqual(rc, 0)

            # First line should be JSON argv.
            parsed = json.loads(out.getvalue().splitlines()[0])
            self.assertIn("-qmp", parsed)
            idx = parsed.index("-qmp")
            self.assertGreater(idx + 1, idx)
            qmp_arg = parsed[idx + 1]
            self.assertIsInstance(qmp_arg, str)
            self.assertTrue(qmp_arg.endswith(",server,nowait"))
            if self.harness.os.name == "nt":
                self.assertRegex(qmp_arg, r"^tcp:127\\.0\\.0\\.1:\\d+,server,nowait$")
            else:
                self.assertTrue(qmp_arg.startswith("unix:"))
                self.assertIn("serial.qmp.sock", qmp_arg)

            mock_run.assert_not_called()
            mock_popen.assert_not_called()


if __name__ == "__main__":
    unittest.main()
