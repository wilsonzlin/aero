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


class ExpectBlkMsiConfigTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_missing_config_marker(self) -> None:
        h = self.harness
        self.assertIsNone(h._try_get_selftest_config_expect_blk_msi(b""))
        self.assertIsNone(h._try_get_selftest_config_expect_blk_msi(b"AERO_VIRTIO_SELFTEST|START|version=1\n"))

    def test_parses_expect_blk_msi_true(self) -> None:
        h = self.harness
        tail = (
            b"AERO_VIRTIO_SELFTEST|START|version=1\n"
            b"AERO_VIRTIO_SELFTEST|CONFIG|http_url=x|http_url_large=y|dns_host=z|blk_root=|expect_blk_msi=1\n"
        )
        self.assertEqual(h._try_get_selftest_config_expect_blk_msi(tail), "1")

    def test_parses_expect_blk_msi_false(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|CONFIG|http_url=x|dns_host=z|blk_root=|expect_blk_msi=0\n"
        self.assertEqual(h._try_get_selftest_config_expect_blk_msi(tail), "0")

    def test_returns_none_when_field_missing(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|CONFIG|http_url=x|dns_host=z\n"
        self.assertIsNone(h._try_get_selftest_config_expect_blk_msi(tail))

    def test_uses_last_config_marker(self) -> None:
        h = self.harness
        tail = (
            b"AERO_VIRTIO_SELFTEST|CONFIG|http_url=x|dns_host=z|expect_blk_msi=0\n"
            b"AERO_VIRTIO_SELFTEST|CONFIG|http_url=x|dns_host=z|expect_blk_msi=1\n"
        )
        self.assertEqual(h._try_get_selftest_config_expect_blk_msi(tail), "1")


if __name__ == "__main__":
    unittest.main()

