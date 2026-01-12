#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import importlib.util
import re
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


_TOKEN_RE = re.compile(r"^FAIL: [A-Z0-9_]+:")


class FailureTokenTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_virtio_snd_skip_tokens(self) -> None:
        h = self.harness

        msg = h._virtio_snd_skip_failure_message(b"virtio-snd: skipped (enable with --test-snd)\n")
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_SND_SKIPPED:"))

        msg = h._virtio_snd_skip_failure_message(b"virtio-snd: disabled by --disable-snd\n")
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_SND_SKIPPED:"))

        msg = h._virtio_snd_skip_failure_message(b"virtio-snd: pci device not detected\n")
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_SND_SKIPPED:"))

    def test_virtio_snd_capture_skip_tokens(self) -> None:
        h = self.harness

        msg = h._virtio_snd_capture_skip_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|endpoint_missing\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_SND_CAPTURE_SKIPPED:"))

        msg = h._virtio_snd_capture_skip_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|flag_not_set\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_SND_CAPTURE_SKIPPED:"))

        msg = h._virtio_snd_capture_skip_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|wrong_service\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_SND_CAPTURE_SKIPPED:"))

    def test_virtio_snd_duplex_skip_tokens(self) -> None:
        h = self.harness

        msg = h._virtio_snd_duplex_skip_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|endpoint_missing\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_SND_DUPLEX_SKIPPED:"))

        msg = h._virtio_snd_duplex_skip_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|flag_not_set\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_SND_DUPLEX_SKIPPED:"))

        msg = h._virtio_snd_duplex_skip_failure_message(
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|device_missing\n"
        )
        self.assertRegex(msg, _TOKEN_RE)
        self.assertTrue(msg.startswith("FAIL: VIRTIO_SND_DUPLEX_SKIPPED:"))


if __name__ == "__main__":
    unittest.main()

