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


class VirtioIrqModeEnforcementTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_no_enforcement_by_default(self) -> None:
        tail = b"virtio-net-irq|INFO|mode=msi\nAERO_VIRTIO_SELFTEST|RESULT|PASS\n"
        msg = self.harness._check_virtio_irq_mode_enforcement(tail, devices=["virtio-net"])
        self.assertIsNone(msg)

    def test_require_intx_fails_on_msi(self) -> None:
        tail = b"virtio-net-irq|INFO|mode=msi\nAERO_VIRTIO_SELFTEST|RESULT|PASS\n"
        msg = self.harness._check_virtio_irq_mode_enforcement(
            tail, require_intx=True, devices=["virtio-net"]
        )
        self.assertEqual(msg, "FAIL: IRQ_MODE_MISMATCH: virtio-net expected=intx got=msi")

    def test_require_msi_fails_on_intx(self) -> None:
        tail = b"virtio-net-irq|INFO|mode=intx\nAERO_VIRTIO_SELFTEST|RESULT|PASS\n"
        msg = self.harness._check_virtio_irq_mode_enforcement(
            tail, require_msi=True, devices=["virtio-net"]
        )
        self.assertEqual(msg, "FAIL: IRQ_MODE_MISMATCH: virtio-net expected=msi got=intx")

    def test_require_msi_accepts_msix(self) -> None:
        tail = b"virtio-net-irq|INFO|mode=msix\nAERO_VIRTIO_SELFTEST|RESULT|PASS\n"
        msg = self.harness._check_virtio_irq_mode_enforcement(
            tail, require_msi=True, devices=["virtio-net"]
        )
        self.assertIsNone(msg)

    def test_virtio_blk_falls_back_to_test_marker(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|irq_mode=msix\n"
            b"AERO_VIRTIO_SELFTEST|RESULT|PASS\n"
        )
        msg = self.harness._check_virtio_irq_mode_enforcement(
            tail, require_intx=True, devices=["virtio-blk"]
        )
        self.assertEqual(msg, "FAIL: IRQ_MODE_MISMATCH: virtio-blk expected=intx got=msix")

    def test_virtio_blk_prefers_standalone_irq_marker(self) -> None:
        tail = (
            b"virtio-blk-irq|INFO|mode=intx\n"
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|irq_mode=msix\n"
            b"AERO_VIRTIO_SELFTEST|RESULT|PASS\n"
        )
        msg = self.harness._check_virtio_irq_mode_enforcement(
            tail, require_intx=True, devices=["virtio-blk"]
        )
        self.assertIsNone(msg)


if __name__ == "__main__":
    unittest.main()

