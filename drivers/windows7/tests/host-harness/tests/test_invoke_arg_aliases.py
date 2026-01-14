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


class HarnessArgAliasTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def _parse(self, argv: list[str]):
        parser = self.harness._build_arg_parser()
        args, extra = parser.parse_known_args(argv)
        self.assertEqual(extra, [])
        return args

    def test_with_tablet_events_alias_sets_flag(self) -> None:
        args = self._parse(
            [
                "--qemu-system",
                "qemu-system-x86_64",
                "--disk-image",
                "disk.img",
                "--with-tablet-events",
            ]
        )
        self.assertTrue(args.with_input_tablet_events)

    def test_with_input_tablet_events_sets_flag(self) -> None:
        args = self._parse(
            [
                "--qemu-system",
                "qemu-system-x86_64",
                "--disk-image",
                "disk.img",
                "--with-input-tablet-events",
            ]
        )
        self.assertTrue(args.with_input_tablet_events)

    def test_virtio_input_tablet_events_aliases_set_flag(self) -> None:
        for flag in (
            "--with-virtio-input-tablet-events",
            "--enable-virtio-input-tablet-events",
            "--require-virtio-input-tablet-events",
        ):
            with self.subTest(flag=flag):
                args = self._parse(
                    [
                        "--qemu-system",
                        "qemu-system-x86_64",
                        "--disk-image",
                        "disk.img",
                        flag,
                    ]
                )
                self.assertTrue(args.with_input_tablet_events)

    def test_virtio_input_events_aliases_set_flag(self) -> None:
        for flag in (
            "--with-virtio-input-events",
            "--enable-virtio-input-events",
            "--require-virtio-input-events",
        ):
            with self.subTest(flag=flag):
                args = self._parse(
                    [
                        "--qemu-system",
                        "qemu-system-x86_64",
                        "--disk-image",
                        "disk.img",
                        flag,
                    ]
                )
                self.assertTrue(args.with_input_events)

    def test_virtio_input_wheel_aliases_set_flag(self) -> None:
        for flag in ("--with-virtio-input-wheel", "--enable-virtio-input-wheel"):
            with self.subTest(flag=flag):
                args = self._parse(
                    [
                        "--qemu-system",
                        "qemu-system-x86_64",
                        "--disk-image",
                        "disk.img",
                        flag,
                    ]
                )
                self.assertTrue(args.with_input_wheel)


if __name__ == "__main__":
    unittest.main()
