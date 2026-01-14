#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import importlib.util
import subprocess
import sys
import unittest
from pathlib import Path
from unittest import mock


def _load_harness():
    harness_path = Path(__file__).resolve().parents[1] / "invoke_aero_virtio_win7_tests.py"
    spec = importlib.util.spec_from_file_location("invoke_aero_virtio_win7_tests", harness_path)
    assert spec and spec.loader

    module = importlib.util.module_from_spec(spec)
    # Register the module before execution so dataclasses can resolve __module__ correctly.
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class QemuProbeCacheTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def setUp(self) -> None:
        h = self.harness
        # Ensure per-test isolation; the harness caches probe results at module scope
        # (either via explicit dict caches or functools.lru_cache).
        for name in (
            "_QEMU_DEVICE_HELP_TEXT_CACHE",
            "_QEMU_HAS_DEVICE_CACHE",
            "_QEMU_DEVICE_HELP_LIST_CACHE",
        ):
            cache = getattr(h, name, None)
            if isinstance(cache, dict):
                cache.clear()

        for fn_name in (
            "_qemu_device_help_text",
            "_qemu_device_property_names",
            "_qemu_device_list_help_text",
            "_qemu_has_device",
        ):
            fn = getattr(h, fn_name, None)
            cache_clear = getattr(fn, "cache_clear", None)
            if callable(cache_clear):
                cache_clear()

    def test_caches_qemu_device_help_text_by_qemu_and_device(self) -> None:
        h = self.harness
        calls: list[tuple[str, ...]] = []

        def fake_run(cmd, stdout=None, stderr=None, text=False, check=False, **kwargs):
            calls.append(tuple(cmd))
            if cmd[1:3] == ["-device", "help"]:
                out = "virtio-sound-pci\nvirtio-keyboard-pci\nvirtio-mouse-pci\n"
                return subprocess.CompletedProcess(cmd, 0, stdout=out, stderr=None)

            if cmd[1] == "-device" and cmd[2].endswith(",help"):
                dev = cmd[2].split(",", 1)[0]
                out = f"Device '{dev}' help\n  disable-legacy\n  x-pci-revision\n"
                return subprocess.CompletedProcess(cmd, 0, stdout=out, stderr=None)

            raise AssertionError(f"unexpected subprocess invocation: {cmd!r}")

        with mock.patch.object(h.subprocess, "run", side_effect=fake_run):
            a1 = h._qemu_device_help_text("qemu-a", "virtio-net-pci")
            a2 = h._qemu_device_help_text("qemu-a", "virtio-net-pci")
            b1 = h._qemu_device_help_text("qemu-b", "virtio-net-pci")

        self.assertEqual(a1, a2)
        self.assertEqual(
            calls,
            [
                ("qemu-a", "-device", "virtio-net-pci,help"),
                ("qemu-b", "-device", "virtio-net-pci,help"),
            ],
        )
        self.assertEqual(b1, a1)

    def test_caches_qemu_has_device_negative_result(self) -> None:
        h = self.harness
        calls: list[tuple[str, ...]] = []

        def fake_run(cmd, stdout=None, stderr=None, text=False, check=False, **kwargs):
            calls.append(tuple(cmd))
            if cmd[1] == "-device" and cmd[2] == "missing-device,help":
                return subprocess.CompletedProcess(cmd, 1, stdout="Device not found\n", stderr=None)
            raise AssertionError(f"unexpected subprocess invocation: {cmd!r}")

        with mock.patch.object(h.subprocess, "run", side_effect=fake_run):
            self.assertFalse(h._qemu_has_device("qemu-a", "missing-device"))
            # Second probe should hit the cache and not spawn another subprocess.
            self.assertFalse(h._qemu_has_device("qemu-a", "missing-device"))

        self.assertEqual(calls, [("qemu-a", "-device", "missing-device,help")])

    def test_caches_device_help_listing_used_by_virtio_snd_detection(self) -> None:
        h = self.harness
        calls: list[tuple[str, ...]] = []

        def fake_run(cmd, stdout=None, stderr=None, text=False, check=False, **kwargs):
            calls.append(tuple(cmd))
            if cmd[1:3] == ["-device", "help"]:
                out = "virtio-sound-pci\nvirtio-snd-pci\n"
                return subprocess.CompletedProcess(cmd, 0, stdout=out, stderr=None)
            raise AssertionError(f"unexpected subprocess invocation: {cmd!r}")

        with mock.patch.object(h.subprocess, "run", side_effect=fake_run):
            self.assertEqual(h._detect_virtio_snd_device("qemu-a"), "virtio-sound-pci")
            self.assertEqual(h._detect_virtio_snd_device("qemu-a"), "virtio-sound-pci")
            # Different qemu-system path should not share cache entries.
            self.assertEqual(h._detect_virtio_snd_device("qemu-b"), "virtio-sound-pci")

        self.assertEqual(
            calls,
            [
                ("qemu-a", "-device", "help"),
                ("qemu-b", "-device", "help"),
            ],
        )


if __name__ == "__main__":
    unittest.main()
