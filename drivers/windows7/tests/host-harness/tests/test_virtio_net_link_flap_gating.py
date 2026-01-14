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

    def test_extract_ready_marker(self) -> None:
        h = self.harness
        tail = (
            b"hello\n"
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|READY|adapter=Ethernet|guid={01234567-89ab-cdef-0123-456789abcdef}\n"
        )
        info = h._try_extract_virtio_net_link_flap_ready(tail)
        self.assertIsNotNone(info)
        assert info is not None
        self.assertEqual(info.adapter, "Ethernet")
        self.assertEqual(info.guid, "{01234567-89ab-cdef-0123-456789abcdef}")

    def test_extract_ready_marker_missing(self) -> None:
        h = self.harness
        self.assertIsNone(h._try_extract_virtio_net_link_flap_ready(b"no markers here\n"))

    def test_required_failure_message_pass(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|PASS|elapsed_ms=1\n"
        self.assertIsNone(h._virtio_net_link_flap_required_failure_message(tail))

    def test_required_failure_message_fail(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|FAIL|reason=timeout\n"
        msg = h._virtio_net_link_flap_required_failure_message(tail)
        self.assertIsNotNone(msg)
        self.assertIn("VIRTIO_NET_LINK_FLAP_FAILED", msg or "")

    def test_required_failure_message_skip_flag_not_set(self) -> None:
        h = self.harness
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|SKIP|flag_not_set\n"
        msg = h._virtio_net_link_flap_required_failure_message(tail)
        self.assertEqual(
            msg,
            "FAIL: VIRTIO_NET_LINK_FLAP_SKIPPED: virtio-net-link-flap test was skipped (flag_not_set) but "
            "--with-net-link-flap was enabled (provision the guest with --test-net-link-flap)",
        )

    def test_required_failure_message_missing(self) -> None:
        h = self.harness
        msg = h._virtio_net_link_flap_required_failure_message(b"some other output\n")
        self.assertEqual(
            msg,
            "FAIL: MISSING_VIRTIO_NET_LINK_FLAP: did not observe virtio-net-link-flap PASS marker while "
            "--with-net-link-flap was enabled (provision the guest with --test-net-link-flap)",
        )


if __name__ == "__main__":
    unittest.main()

