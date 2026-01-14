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


class VirtioNetLinkFlapGatingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_required_marker_pass(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|PASS|down_sec=1.00|up_sec=2.00|ipv4=10.0.2.15\n"
        self.assertIsNone(h._virtio_net_link_flap_required_failure_message(tail))

    def test_required_marker_pass_via_saw_flag_even_if_tail_truncated(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|RESULT|PASS\n"
        self.assertIsNone(h._virtio_net_link_flap_required_failure_message(tail, saw_pass=True))

    def test_required_marker_fail(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|FAIL|reason=http_get_failed\n"
        msg = h._virtio_net_link_flap_required_failure_message(tail)
        self.assertIsNotNone(msg)
        self.assertTrue(str(msg).startswith("FAIL: VIRTIO_NET_LINK_FLAP_FAILED:"))

    def test_required_marker_skip(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|SKIP|flag_not_set\n"
        msg = h._virtio_net_link_flap_required_failure_message(tail)
        self.assertIsNotNone(msg)
        self.assertTrue(str(msg).startswith("FAIL: VIRTIO_NET_LINK_FLAP_SKIPPED:"))

    def test_required_marker_missing(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|RESULT|PASS\n"
        msg = h._virtio_net_link_flap_required_failure_message(tail)
        self.assertIsNotNone(msg)
        self.assertTrue(str(msg).startswith("FAIL: MISSING_VIRTIO_NET_LINK_FLAP:"))


if __name__ == "__main__":
    unittest.main()

