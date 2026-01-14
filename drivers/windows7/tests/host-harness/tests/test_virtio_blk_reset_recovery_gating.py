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


class VirtioBlkResetRecoveryGatingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_gate_ignores_missing_marker(self) -> None:
        h = self.harness
        msg = h._check_no_blk_reset_recovery_requirement(b"AERO_VIRTIO_SELFTEST|RESULT|PASS\n")
        self.assertIsNone(msg)

    def test_gate_ignores_skip_marker(self) -> None:
        h = self.harness
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|SKIP|reason=ioctl_payload_truncated|returned_len=16\n"
        )
        msg = h._check_no_blk_reset_recovery_requirement(tail)
        self.assertIsNone(msg)

    def test_gate_falls_back_to_miniport_diagnostic_marker(self) -> None:
        h = self.harness
        tail = b"virtio-blk-miniport-reset-recovery|INFO|reset_detected=1|hw_reset_bus=0\n"
        msg = h._check_no_blk_reset_recovery_requirement(tail)
        self.assertEqual(
            msg,
            "FAIL: VIRTIO_BLK_RESET_RECOVERY_NONZERO: reset_detected=1 hw_reset_bus=0",
        )

    def test_gate_ignores_warn_miniport_diagnostic_marker(self) -> None:
        h = self.harness
        tail = b"virtio-blk-miniport-reset-recovery|WARN|reason=missing_counters|returned_len=20|expected_min=24\n"
        msg = h._check_no_blk_reset_recovery_requirement(tail)
        self.assertIsNone(msg)

    def test_require_gate_fails_on_reset_detected(self) -> None:
        h = self.harness
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|INFO|reset_detected=1|hw_reset_bus=0\n"
        )
        msg = h._check_no_blk_reset_recovery_requirement(tail)
        self.assertEqual(
            msg,
            "FAIL: VIRTIO_BLK_RESET_RECOVERY_NONZERO: reset_detected=1 hw_reset_bus=0",
        )

    def test_fail_on_gate_checks_hw_reset_bus_only(self) -> None:
        h = self.harness
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|INFO|reset_detected=1|hw_reset_bus=0\n"
        )
        msg = h._check_fail_on_blk_reset_recovery_requirement(tail)
        self.assertIsNone(msg)

        # Backward compatible fallback: parse legacy miniport diagnostic lines.
        tail_diag = b"virtio-blk-miniport-reset-recovery|INFO|reset_detected=1|hw_reset_bus=2\n"
        msg_diag = h._check_fail_on_blk_reset_recovery_requirement(tail_diag)
        self.assertEqual(
            msg_diag,
            "FAIL: VIRTIO_BLK_RESET_RECOVERY_DETECTED: hw_reset_bus=2 reset_detected=1",
        )

        tail2 = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk-reset-recovery|INFO|reset_detected=1|hw_reset_bus=2\n"
        )
        msg2 = h._check_fail_on_blk_reset_recovery_requirement(tail2)
        self.assertEqual(
            msg2,
            "FAIL: VIRTIO_BLK_RESET_RECOVERY_DETECTED: hw_reset_bus=2 reset_detected=1",
        )

    def test_cli_flags_parse(self) -> None:
        h = self.harness
        parser = h._build_arg_parser()

        args, extra = parser.parse_known_args(
            [
                "--qemu-system",
                "qemu-system-x86_64",
                "--disk-image",
                "disk.img",
                "--require-no-blk-reset-recovery",
                "--fail-on-blk-reset-recovery",
            ]
        )
        self.assertEqual(extra, [])
        self.assertTrue(args.require_no_blk_reset_recovery)
        self.assertTrue(args.fail_on_blk_reset_recovery)


if __name__ == "__main__":
    unittest.main()
