#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import contextlib
import importlib.util
import io
import subprocess
import sys
import unittest
from pathlib import Path
from unittest import mock


def _load_probe():
    script_path = Path(__file__).resolve().parents[1] / "probe_qemu_virtio_pci_ids.py"
    spec = importlib.util.spec_from_file_location("probe_qemu_virtio_pci_ids", script_path)
    assert spec and spec.loader

    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class ProbeQemuVirtioPciIdsDeviceListHelpReturnCodeTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.probe = _load_probe()

    def test_device_list_help_nonzero_exit_is_error(self) -> None:
        probe = self.probe

        argv = [
            "probe_qemu_virtio_pci_ids.py",
            "--qemu-system",
            "qemu-system-x86_64",
            "--with-virtio-snd",
        ]

        def fake_run(cmd, **_kwargs):
            # cmd looks like [qemu, "-device", "help"]
            return subprocess.CompletedProcess(cmd, 1, stdout="qemu: failed\n")

        stderr = io.StringIO()
        with (
            mock.patch.object(sys, "argv", argv),
            mock.patch.object(probe.subprocess, "run", side_effect=fake_run),
            mock.patch.object(probe.subprocess, "Popen") as mock_popen,
            contextlib.redirect_stderr(stderr),
        ):
            rc = probe.main()

        self.assertEqual(rc, 2)
        err = stderr.getvalue()
        self.assertIn("failed to query QEMU device list (exit=1)", err)
        self.assertIn("qemu: failed", err)
        mock_popen.assert_not_called()


if __name__ == "__main__":
    unittest.main()

