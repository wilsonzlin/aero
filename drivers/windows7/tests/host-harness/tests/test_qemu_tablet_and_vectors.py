#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path


def _load_harness():
    harness_path = Path(__file__).resolve().parents[1] / "invoke_aero_virtio_win7_tests.py"
    spec = importlib.util.spec_from_file_location("invoke_aero_virtio_win7_tests", harness_path)
    assert spec and spec.loader

    module = importlib.util.module_from_spec(spec)
    # Register the module before execution so dataclasses can resolve __module__ correctly.
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class QemuTabletAndVectorsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_builds_virtio_tablet_device_arg_modern_only(self) -> None:
        h = self.harness
        arg = h._qemu_virtio_tablet_pci_device_arg(disable_legacy=True, pci_revision="0x01")
        self.assertIn("virtio-tablet-pci", arg)
        self.assertIn("id=aero_virtio_tablet0", arg)
        self.assertIn("disable-legacy=on", arg)
        self.assertIn("x-pci-revision=0x01", arg)

    def test_vectors_property_appended_only_when_supported(self) -> None:
        h = self.harness

        old_supports = h._qemu_device_supports_property
        try:
            h._qemu_device_supports_property = lambda *_args, **_kwargs: True
            arg = h._qemu_device_arg_maybe_add_vectors(
                "qemu-system-x86_64",
                "virtio-net-pci",
                "virtio-net-pci,netdev=net0",
                vectors=5,
                flag_name="--virtio-net-vectors",
            )
            self.assertIn("vectors=5", arg)

            # When vectors is unset, nothing should be added.
            arg2 = h._qemu_device_arg_maybe_add_vectors(
                "qemu-system-x86_64",
                "virtio-net-pci",
                "virtio-net-pci,netdev=net0",
                vectors=None,
                flag_name="--virtio-net-vectors",
            )
            self.assertNotIn("vectors=5", arg2)

            h._qemu_device_supports_property = lambda *_args, **_kwargs: False
            with self.assertRaises(RuntimeError) as ctx:
                h._qemu_device_arg_maybe_add_vectors(
                    "qemu-system-x86_64",
                    "virtio-net-pci",
                    "virtio-net-pci,netdev=net0",
                    vectors=5,
                    flag_name="--virtio-net-vectors",
                )
            msg = str(ctx.exception)
            self.assertIn("does not expose the 'vectors' property", msg)
            self.assertIn("virtio-net-pci", msg)
            self.assertIn("--virtio-net-vectors=5", msg)
        finally:
            h._qemu_device_supports_property = old_supports


if __name__ == "__main__":
    unittest.main()
