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

    def test_require_helper_passes_when_set(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|CONFIG|http_url=x|dns_host=z|expect_blk_msi=1\n"
        cfg, msg = h._require_expect_blk_msi_config(tail, expect_blk_msi_config=None)
        self.assertEqual(cfg, "1")
        self.assertIsNone(msg)

        # Should not require a CONFIG marker once we've already observed the value.
        cfg2, msg2 = h._require_expect_blk_msi_config(b"", expect_blk_msi_config="1")
        self.assertEqual(cfg2, "1")
        self.assertIsNone(msg2)

    def test_require_helper_fails_when_missing_or_zero(self) -> None:
        h = self.harness

        # Missing CONFIG marker / field.
        cfg, msg = h._require_expect_blk_msi_config(b"AERO_VIRTIO_SELFTEST|START|version=1\n", expect_blk_msi_config=None)
        self.assertIsNone(cfg)
        self.assertIsNotNone(msg)
        assert msg is not None
        self.assertTrue(msg.startswith("FAIL: EXPECT_BLK_MSI_NOT_SET:"))
        self.assertIn("--expect-blk-msi", msg)
        self.assertIn("--require-expect-blk-msi", msg)

        # Explicit expect_blk_msi=0 in CONFIG marker.
        tail = b"AERO_VIRTIO_SELFTEST|CONFIG|http_url=x|dns_host=z|expect_blk_msi=0\n"
        cfg2, msg2 = h._require_expect_blk_msi_config(tail, expect_blk_msi_config=None)
        self.assertEqual(cfg2, "0")
        self.assertIsNotNone(msg2)
        assert msg2 is not None
        self.assertTrue(msg2.startswith("FAIL: EXPECT_BLK_MSI_NOT_SET:"))
        self.assertIn("expect_blk_msi=1", msg2)

        # Pre-parsed config=0 should still fail even without tail content.
        cfg3, msg3 = h._require_expect_blk_msi_config(b"", expect_blk_msi_config="0")
        self.assertEqual(cfg3, "0")
        self.assertIsNotNone(msg3)


if __name__ == "__main__":
    unittest.main()
