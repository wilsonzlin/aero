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


class VirtioNetMsixRequireTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_pass_when_mode_msix(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|PASS|mode=msix|messages=3|config_vector=0|rx_vector=1|tx_vector=2\n"
        ok, reason = self.harness._require_virtio_net_msix_marker(tail)
        self.assertTrue(ok, reason)

    def test_fails_when_mode_intx(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|PASS|mode=intx|messages=0\n"
        ok, reason = self.harness._require_virtio_net_msix_marker(tail)
        self.assertFalse(ok)
        self.assertIn("mode=intx", reason)

    def test_fails_on_skip(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|SKIP|reason=diag_unavailable\n"
        ok, reason = self.harness._require_virtio_net_msix_marker(tail)
        self.assertFalse(ok)
        self.assertIn("SKIP", reason)

    def test_fails_on_fail(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|FAIL|reason=whatever\n"
        ok, reason = self.harness._require_virtio_net_msix_marker(tail)
        self.assertFalse(ok)
        self.assertIn("FAIL", reason)

    def test_fails_when_mode_missing(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-msix|PASS|messages=3|config_vector=0|rx_vector=1|tx_vector=2\n"
        ok, reason = self.harness._require_virtio_net_msix_marker(tail)
        self.assertFalse(ok)
        self.assertIn("missing mode", reason.lower())

    def test_fails_when_missing(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS\n"
        ok, reason = self.harness._require_virtio_net_msix_marker(tail)
        self.assertFalse(ok)
        self.assertIn("missing", reason.lower())


if __name__ == "__main__":
    unittest.main()
