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
        for flag in (
            "--with-virtio-input-wheel",
            "--require-virtio-input-wheel",
            "--enable-virtio-input-wheel",
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
                self.assertTrue(args.with_input_wheel)

    def test_snd_buffer_limits_aliases_set_flag(self) -> None:
        for flag in (
            "--with-snd-buffer-limits",
            "--with-virtio-snd-buffer-limits",
            "--enable-snd-buffer-limits",
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
                self.assertTrue(args.with_snd_buffer_limits)

                self.assertTrue(args.with_snd_buffer_limits)

    def test_virtio_input_media_keys_aliases_set_flag(self) -> None:
        for flag in (
            "--with-input-media-keys",
            "--with-virtio-input-media-keys",
            "--enable-virtio-input-media-keys",
            "--require-virtio-input-media-keys",
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
                self.assertTrue(args.with_input_media_keys)

    def test_virtio_input_led_aliases_set_flag(self) -> None:
        for flag in (
            "--with-input-led",
            "--with-virtio-input-led",
            "--enable-virtio-input-led",
            "--require-virtio-input-led",
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
                self.assertTrue(args.with_input_led)

    def test_with_input_events_extended_aliases_set_flag(self) -> None:
        for flag in ("--with-input-events-extended", "--with-input-events-extra"):
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
                self.assertTrue(args.with_input_events_extended)

    def test_with_virtio_tablet_sets_flag(self) -> None:
        args = self._parse(
            [
                "--qemu-system",
                "qemu-system-x86_64",
                "--disk-image",
                "disk.img",
                "--with-virtio-tablet",
            ]
        )
        self.assertTrue(args.with_virtio_tablet)

    def test_qmp_preflight_pci_aliases_set_flag(self) -> None:
        for flag in ("--qemu-preflight-pci", "--qmp-preflight-pci"):
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
                self.assertTrue(args.qemu_preflight_pci)

    def test_print_qemu_cmd_alias_sets_dry_run(self) -> None:
        for flag in ("--dry-run", "--print-qemu-cmd"):
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
                self.assertTrue(args.dry_run)

    def test_require_expect_blk_msi_sets_flag(self) -> None:
        args = self._parse(
            [
                "--qemu-system",
                "qemu-system-x86_64",
                "--disk-image",
                "disk.img",
                "--require-expect-blk-msi",
            ]
        )
        self.assertTrue(args.require_expect_blk_msi)

    def test_vectors_override_flags_parse(self) -> None:
        args = self._parse(
            [
                "--qemu-system",
                "qemu-system-x86_64",
                "--disk-image",
                "disk.img",
                "--virtio-msix-vectors",
                "4",
                "--virtio-net-vectors",
                "2",
                "--virtio-blk-vectors",
                "3",
                "--virtio-input-vectors",
                "5",
                "--virtio-snd-vectors",
                "6",
            ]
        )
        self.assertEqual(args.virtio_msix_vectors, 4)
        self.assertEqual(args.virtio_net_vectors, 2)
        self.assertEqual(args.virtio_blk_vectors, 3)
        self.assertEqual(args.virtio_input_vectors, 5)
        self.assertEqual(args.virtio_snd_vectors, 6)

    def test_vectors_override_alias_flags_parse(self) -> None:
        args = self._parse(
            [
                "--qemu-system",
                "qemu-system-x86_64",
                "--disk-image",
                "disk.img",
                "--with-virtio-snd",
                "--virtio-net-msix-vectors",
                "2",
                "--virtio-blk-msix-vectors",
                "3",
                "--virtio-input-msix-vectors",
                "5",
                "--virtio-snd-msix-vectors",
                "6",
            ]
        )
        self.assertTrue(args.enable_virtio_snd)
        self.assertEqual(args.virtio_net_vectors, 2)
        self.assertEqual(args.virtio_blk_vectors, 3)
        self.assertEqual(args.virtio_input_vectors, 5)
        self.assertEqual(args.virtio_snd_vectors, 6)


if __name__ == "__main__":
    unittest.main()
