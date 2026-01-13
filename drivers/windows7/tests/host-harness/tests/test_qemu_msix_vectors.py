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


class QemuMsixVectorsTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_appends_vectors_fragment(self) -> None:
        f = self.harness._qemu_device_arg_add_vectors
        self.assertEqual(
            f("virtio-net-pci,netdev=net0,disable-legacy=on,x-pci-revision=0x01", 1),
            "virtio-net-pci,netdev=net0,disable-legacy=on,x-pci-revision=0x01,vectors=1",
        )

    def test_no_change_when_unset(self) -> None:
        f = self.harness._qemu_device_arg_add_vectors
        self.assertEqual(f("virtio-blk-pci,drive=drive0", None), "virtio-blk-pci,drive=drive0")

    def test_preserves_quoted_keyval_values_with_commas(self) -> None:
        quote = self.harness._qemu_quote_keyval_value
        device_arg = f"virtio-test-pci,path={quote(r'C:\\with,comma\\file.bin')},foo=bar"
        out = self.harness._qemu_device_arg_add_vectors(device_arg, 1)
        self.assertEqual(out, device_arg + ",vectors=1")


if __name__ == "__main__":
    unittest.main()
