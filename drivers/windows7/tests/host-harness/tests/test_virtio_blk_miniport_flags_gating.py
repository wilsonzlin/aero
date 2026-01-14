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


class VirtioBlkMiniportFlagsGatingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_gate_ignores_missing_marker(self) -> None:
        h = self.harness
        msg = h._check_no_blk_miniport_flags_requirement(b"AERO_VIRTIO_SELFTEST|RESULT|PASS\n")
        self.assertIsNone(msg)

    def test_gate_ignores_warn_marker(self) -> None:
        h = self.harness
        tail = b"virtio-blk-miniport-flags|WARN|reason=missing_flags|returned_len=16|expected_min=20\n"
        msg = h._check_no_blk_miniport_flags_requirement(tail)
        self.assertIsNone(msg)
        msg2 = h._check_fail_on_blk_miniport_flags_requirement(tail)
        self.assertIsNone(msg2)

    def test_require_gate_fails_on_reset_pending(self) -> None:
        h = self.harness
        tail = (
            b"virtio-blk-miniport-flags|INFO|raw=0x00000008|removed=0|surprise_removed=0|"
            b"reset_in_progress=0|reset_pending=1\n"
        )
        msg = h._check_no_blk_miniport_flags_requirement(tail)
        self.assertEqual(
            msg,
            "FAIL: VIRTIO_BLK_MINIPORT_FLAGS_NONZERO: raw=0x00000008 removed=0 surprise_removed=0 "
            "reset_in_progress=0 reset_pending=1",
        )

        # Looser mode ignores reset_pending.
        msg2 = h._check_fail_on_blk_miniport_flags_requirement(tail)
        self.assertIsNone(msg2)

    def test_fail_on_gate_fails_on_removed(self) -> None:
        h = self.harness
        tail = (
            b"virtio-blk-miniport-flags|INFO|raw=0x00000001|removed=1|surprise_removed=0|"
            b"reset_in_progress=0|reset_pending=0\n"
        )
        msg = h._check_fail_on_blk_miniport_flags_requirement(tail)
        self.assertEqual(
            msg,
            "FAIL: VIRTIO_BLK_MINIPORT_FLAGS_REMOVED: raw=0x00000001 removed=1 surprise_removed=0",
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
                "--require-no-blk-miniport-flags",
                "--fail-on-blk-miniport-flags",
            ]
        )
        self.assertEqual(extra, [])
        self.assertTrue(args.require_no_blk_miniport_flags)
        self.assertTrue(args.fail_on_blk_miniport_flags)


if __name__ == "__main__":
    unittest.main()

