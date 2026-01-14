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


class UpdateLastMarkerLineFromChunkTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.h = _load_harness()

    def test_handles_crlf_newlines(self) -> None:
        prefix = b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|"
        last, carry = self.h._update_last_marker_line_from_chunk(
            None,
            b"hello\r\nAERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|PASS\r\nbye\r\n",
            prefix=prefix,
            carry=b"",
        )
        self.assertEqual(last, "AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|PASS")
        self.assertEqual(carry, b"")

    def test_handles_cr_only_newlines(self) -> None:
        prefix = b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|"
        last, carry = self.h._update_last_marker_line_from_chunk(
            None,
            b"hello\rAERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|PASS\rbye\r",
            prefix=prefix,
            carry=b"",
        )
        self.assertEqual(last, "AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|PASS")
        self.assertEqual(carry, b"")

    def test_carry_allows_split_lines(self) -> None:
        prefix = b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|"
        last, carry = self.h._update_last_marker_line_from_chunk(
            None,
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|PA",
            prefix=prefix,
            carry=b"",
        )
        self.assertIsNone(last)
        self.assertEqual(carry, b"AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|PA")

        last, carry = self.h._update_last_marker_line_from_chunk(
            last,
            b"SS\r\n",
            prefix=prefix,
            carry=carry,
        )
        self.assertEqual(last, "AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|PASS")
        self.assertEqual(carry, b"")


if __name__ == "__main__":
    unittest.main()

