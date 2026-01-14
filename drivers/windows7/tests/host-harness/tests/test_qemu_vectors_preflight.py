#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import importlib.util
import subprocess
import sys
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


class QemuVectorsPreflightTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_missing_vectors_property_raises(self) -> None:
        self.harness._qemu_device_help_text.cache_clear()

        def fake_run(cmd, **kwargs):
            # cmd looks like: [qemu, "-device", "<dev>,help"]
            dev = cmd[2].split(",", 1)[0]
            stdout = f"{dev} options:\n  x-pci-revision=uint16\n"
            return subprocess.CompletedProcess(cmd, 0, stdout=stdout)

        with mock.patch.object(self.harness.subprocess, "run", side_effect=fake_run):
            with self.assertRaises(RuntimeError) as ctx:
                self.harness._assert_qemu_devices_support_vectors_property(
                    "qemu-system-x86_64",
                    ["virtio-net-pci", "virtio-blk-pci"],
                    requested_by="--virtio-msix-vectors=8",
                )

        msg = str(ctx.exception)
        self.assertIn("vectors", msg)
        self.assertIn("virtio-net-pci", msg)
        self.assertIn("Disable", msg)

    def test_vectors_property_present_ok(self) -> None:
        self.harness._qemu_device_help_text.cache_clear()

        def fake_run(cmd, **kwargs):
            dev = cmd[2].split(",", 1)[0]
            stdout = f"{dev} options:\n  vectors=uint32\n  x-pci-revision=uint16\n"
            return subprocess.CompletedProcess(cmd, 0, stdout=stdout)

        with mock.patch.object(self.harness.subprocess, "run", side_effect=fake_run):
            self.harness._assert_qemu_devices_support_vectors_property(
                "qemu-system-x86_64",
                ["virtio-net-pci", "virtio-blk-pci"],
                requested_by="--virtio-msix-vectors=8",
            )


if __name__ == "__main__":
    unittest.main()
