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


class VirtioIrqMarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_parses_per_device_irq_markers(self) -> None:
        tail = (
            b"boot...\n"
            b"virtio-net-irq|INFO|mode=msix|vectors=4|msix_enabled=1\n"
            b"virtio-blk-irq|WARN|mode=intx|reason=msi_disabled\n"
            b"AERO_VIRTIO_SELFTEST|RESULT|PASS\n"
        )

        out = self.harness._parse_virtio_irq_markers(tail)
        self.assertEqual(out["virtio-net"]["level"], "INFO")
        self.assertEqual(out["virtio-net"]["mode"], "msix")
        self.assertEqual(out["virtio-net"]["vectors"], "4")
        self.assertEqual(out["virtio-net"]["msix_enabled"], "1")

        self.assertEqual(out["virtio-blk"]["level"], "WARN")
        self.assertEqual(out["virtio-blk"]["mode"], "intx")
        self.assertEqual(out["virtio-blk"]["reason"], "msi_disabled")

    def test_uses_last_marker_per_device(self) -> None:
        tail = (
            b"virtio-net-irq|INFO|mode=msi|vectors=1\n"
            b"virtio-net-irq|INFO|mode=msix|vectors=8\n"
        )
        out = self.harness._parse_virtio_irq_markers(tail)
        self.assertEqual(out["virtio-net"]["mode"], "msix")
        self.assertEqual(out["virtio-net"]["vectors"], "8")


if __name__ == "__main__":
    unittest.main()

