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


class UpdateVirtioSndSkipReasonFromChunkTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.h = _load_harness()

    def test_detects_guest_not_configured_with_test_snd(self) -> None:
        reason, carry = self.h._update_virtio_snd_skip_reason_from_chunk(
            None,
            b"virtio-snd: skipped (enable with --test-snd)\r\n",
            carry=b"",
        )
        self.assertEqual(reason, "guest_not_configured_with_--test-snd")
        self.assertEqual(carry, b"")

    def test_detects_device_missing(self) -> None:
        reason, carry = self.h._update_virtio_snd_skip_reason_from_chunk(
            None,
            b"virtio-snd: pci device not detected\n",
            carry=b"",
        )
        self.assertEqual(reason, "device_missing")
        self.assertEqual(carry, b"")

    def test_carry_allows_split_lines(self) -> None:
        reason, carry = self.h._update_virtio_snd_skip_reason_from_chunk(
            None,
            b"virtio-snd: disabled by --disable",
            carry=b"",
        )
        self.assertIsNone(reason)
        self.assertEqual(carry, b"virtio-snd: disabled by --disable")

        reason, carry = self.h._update_virtio_snd_skip_reason_from_chunk(
            reason,
            b"-snd\r\n",
            carry=carry,
        )
        self.assertEqual(reason, "--disable-snd")
        self.assertEqual(carry, b"")


if __name__ == "__main__":
    unittest.main()

