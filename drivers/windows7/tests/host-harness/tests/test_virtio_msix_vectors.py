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


class VirtioMsixVectorsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_no_change_when_disabled(self) -> None:
        f = self.harness._qemu_device_arg_add_vectors
        self.assertEqual(f("virtio-net-pci,netdev=net0", None), "virtio-net-pci,netdev=net0")

    def test_appends_vectors_when_enabled(self) -> None:
        f = self.harness._qemu_device_arg_add_vectors

        self.assertEqual(
            f("virtio-net-pci,netdev=net0,disable-legacy=on,x-pci-revision=0x01", 4),
            "virtio-net-pci,netdev=net0,disable-legacy=on,x-pci-revision=0x01,vectors=4",
        )
        self.assertEqual(
            f("virtio-blk-pci,drive=drive0,disable-legacy=on,x-pci-revision=0x01", 4),
            "virtio-blk-pci,drive=drive0,disable-legacy=on,x-pci-revision=0x01,vectors=4",
        )
        self.assertEqual(
            f("virtio-keyboard-pci,id=aero_virtio_kbd0,disable-legacy=on,x-pci-revision=0x01", 4),
            "virtio-keyboard-pci,id=aero_virtio_kbd0,disable-legacy=on,x-pci-revision=0x01,vectors=4",
        )
        self.assertEqual(
            f("virtio-mouse-pci,id=aero_virtio_mouse0,disable-legacy=on,x-pci-revision=0x01", 4),
            "virtio-mouse-pci,id=aero_virtio_mouse0,disable-legacy=on,x-pci-revision=0x01,vectors=4",
        )
        self.assertEqual(
            f("virtio-tablet-pci,id=aero_virtio_tablet0,disable-legacy=on,x-pci-revision=0x01", 4),
            "virtio-tablet-pci,id=aero_virtio_tablet0,disable-legacy=on,x-pci-revision=0x01,vectors=4",
        )
        self.assertEqual(
            f("virtio-sound-pci,audiodev=snd0,disable-legacy=on,x-pci-revision=0x01", 4),
            "virtio-sound-pci,audiodev=snd0,disable-legacy=on,x-pci-revision=0x01,vectors=4",
        )

    def test_avoids_duplicate_commas(self) -> None:
        f = self.harness._qemu_device_arg_add_vectors
        self.assertEqual(
            f("virtio-net-pci,netdev=net0,", 8),
            "virtio-net-pci,netdev=net0,vectors=8",
        )

    def test_does_not_duplicate_vectors_key(self) -> None:
        f = self.harness._qemu_device_arg_add_vectors
        self.assertEqual(
            f("virtio-net-pci,netdev=net0,vectors=2,", 4),
            "virtio-net-pci,netdev=net0,vectors=2",
        )

    def test_rejects_non_positive_vectors(self) -> None:
        f = self.harness._qemu_device_arg_add_vectors
        with self.assertRaises(ValueError):
            f("virtio-net-pci,netdev=net0", 0)
        with self.assertRaises(ValueError):
            f("virtio-net-pci,netdev=net0", -1)


if __name__ == "__main__":
    unittest.main()
