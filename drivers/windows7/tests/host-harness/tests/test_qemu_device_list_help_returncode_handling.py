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


class QemuDeviceListHelpReturnCodeHandlingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_qemu_device_list_help_nonzero_exit_is_error(self) -> None:
        """
        If `qemu-system -device help` exits non-zero, don't silently treat the output as a device list.
        This avoids misreporting QEMU execution errors as missing virtio-snd device support.
        """
        h = self.harness
        _clear_qemu_probe_caches(h)

        def fake_run(cmd, **_kwargs):
            # cmd looks like: [qemu, "-device", "<arg>"]
            dev_arg = cmd[2]

            # Simulate QEMU failing for `-device help`.
            if dev_arg == "help":
                return subprocess.CompletedProcess(cmd, 1, stdout="qemu: something went wrong\n")

            # Contract-v1 preflight probes required devices via `-device <name>,help`.
            if dev_arg.endswith(",help"):
                stdout = "options:\n  disable-legacy=bool\n  x-pci-revision=uint16\n"
                return subprocess.CompletedProcess(cmd, 0, stdout=stdout)

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
                "--with-virtio-snd",
                "--virtio-snd-audio-backend",
                "none",
            ]

            stderr = io.StringIO()
            with (
                mock.patch.object(sys, "argv", argv),
                mock.patch.object(h.subprocess, "run", side_effect=fake_run),
                contextlib.redirect_stderr(stderr),
            ):
                rc = h.main()

        self.assertEqual(rc, 2)
        err = stderr.getvalue()
        self.assertIn("failed to query QEMU device list (exit=1)", err)
        self.assertIn("qemu: something went wrong", err)
        self.assertNotIn("does not advertise a virtio-snd PCI device", err)


if __name__ == "__main__":
    unittest.main()

