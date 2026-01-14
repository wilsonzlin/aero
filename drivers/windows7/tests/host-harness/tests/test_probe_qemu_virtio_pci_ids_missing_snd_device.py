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


class ProbeQemuVirtioPciIdsMissingSndDeviceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.probe = _load_probe()

    def test_with_snd_requires_qemu_to_advertise_snd_device(self) -> None:
        probe = self.probe
        argv = [
            "probe_qemu_virtio_pci_ids.py",
            "--qemu-system",
            "qemu-system-x86_64",
            "--with-virtio-snd",
        ]

        def fake_run(cmd, **_kwargs):
            # Simulate `qemu-system -device help` output without virtio-snd devices.
            return subprocess.CompletedProcess(cmd, 0, stdout="virtio-net-pci\n")

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
        self.assertIn("ERROR: QEMU does not advertise a virtio-snd PCI device", err)
        mock_popen.assert_not_called()


if __name__ == "__main__":
    unittest.main()

