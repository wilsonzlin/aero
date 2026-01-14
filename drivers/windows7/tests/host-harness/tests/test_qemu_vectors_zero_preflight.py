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


class QemuVectorsZeroPreflightTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_disable_msix_missing_vectors_property_fails_fast(self) -> None:
        h = self.harness
        _clear_qemu_probe_caches(h)

        def fake_run(cmd, **_kwargs):
            # cmd looks like: [qemu, "-device", "<dev>,help"]
            dev_arg = cmd[2]

            # virtio-net/blk: missing vectors property
            if dev_arg.startswith("virtio-net-pci,help"):
                stdout = "virtio-net-pci options:\n  x-pci-revision=uint16\n"
                return subprocess.CompletedProcess(cmd, 0, stdout=stdout)
            if dev_arg.startswith("virtio-blk-pci,help"):
                stdout = "virtio-blk-pci options:\n  x-pci-revision=uint16\n"
                return subprocess.CompletedProcess(cmd, 0, stdout=stdout)

            # virtio-input devices: simulate not present so transitional mode won't try to attach them.
            if dev_arg.startswith("virtio-keyboard-pci,help"):
                return subprocess.CompletedProcess(cmd, 1, stdout="Device 'virtio-keyboard-pci' not found\n")
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
                # Use transitional mode to avoid the contract-v1 QEMU feature preflight (we only
                # want to exercise the INTx-only `vectors=0` preflight).
                "--virtio-transitional",
                "--virtio-disable-msix",
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
                rc = h.main()

            self.assertEqual(rc, 2)
            err = stderr.getvalue()
            self.assertIn("--virtio-disable-msix", err)
            self.assertIn("vectors", err)
            self.assertIn("does not advertise a 'vectors' property", err)
            self.assertIn("Upgrade QEMU or omit --virtio-disable-msix", err)
            mock_popen.assert_not_called()
            mock_httpd.assert_not_called()
            mock_udp.assert_not_called()

    def test_disable_msix_rejected_vectors_zero_fails_fast(self) -> None:
        h = self.harness
        _clear_qemu_probe_caches(h)

        def fake_run(cmd, **_kwargs):
            dev_arg = cmd[2]

            # Device advertises vectors, but rejects vectors=0.
            if dev_arg.startswith("virtio-net-pci,help"):
                stdout = "virtio-net-pci options:\n  vectors=uint32\n"
                return subprocess.CompletedProcess(cmd, 0, stdout=stdout)
            if dev_arg.startswith("virtio-net-pci,vectors=0,help"):
                return subprocess.CompletedProcess(
                    cmd, 1, stdout="Property 'vectors' does not accept value 0\n"
                )

            # virtio-blk exists but we should fail before reaching it (net check fails first).
            if dev_arg.startswith("virtio-blk-pci,help"):
                stdout = "virtio-blk-pci options:\n  vectors=uint32\n"
                return subprocess.CompletedProcess(cmd, 0, stdout=stdout)

            # virtio-input devices: simulate not present so transitional mode won't try to attach them.
            if dev_arg.startswith("virtio-keyboard-pci,help"):
                return subprocess.CompletedProcess(cmd, 1, stdout="Device 'virtio-keyboard-pci' not found\n")
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
                "--virtio-transitional",
                "--virtio-disable-msix",
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
                rc = h.main()

            self.assertEqual(rc, 2)
            err = stderr.getvalue()
            self.assertIn("rejected 'vectors=0'", err)
            self.assertIn("virtio-net-pci,vectors=0", err)
            mock_popen.assert_not_called()
            mock_httpd.assert_not_called()
            mock_udp.assert_not_called()


if __name__ == "__main__":
    unittest.main()

