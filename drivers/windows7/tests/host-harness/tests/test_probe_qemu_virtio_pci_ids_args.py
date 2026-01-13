#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path


def _load_probe():
    probe_path = Path(__file__).resolve().parents[1] / "probe_qemu_virtio_pci_ids.py"
    spec = importlib.util.spec_from_file_location("probe_qemu_virtio_pci_ids", probe_path)
    assert spec and spec.loader

    module = importlib.util.module_from_spec(spec)
    # Register the module before execution so dataclasses can resolve __module__ correctly.
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class ProbeQemuVirtioPciIdsArgTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.probe = _load_probe()

    def _build_args(self, *, with_tablet: bool, help_text: str) -> list[str]:
        return self.probe._build_qemu_args(
            qemu_system="qemu-system-x86_64",
            disk_path=Path("disk.img"),
            mode="default",
            with_virtio_snd=False,
            with_virtio_tablet=with_tablet,
            device_help_text=help_text,
        )

    def test_tablet_is_opt_in(self) -> None:
        args = self._build_args(with_tablet=False, help_text="name virtio-tablet-pci\n")
        self.assertFalse(any("virtio-tablet-pci" in a for a in args))

    def test_tablet_not_duplicated_when_enabled(self) -> None:
        args = self._build_args(with_tablet=True, help_text="virtio-tablet-pci\n")
        specs = [a for a in args if a.startswith("virtio-tablet-pci")]
        self.assertEqual(len(specs), 1)

    def test_tablet_skipped_when_qemu_does_not_advertise_device(self) -> None:
        args = self._build_args(with_tablet=True, help_text="virtio-mouse-pci\nvirtio-keyboard-pci\n")
        self.assertFalse(any("virtio-tablet-pci" in a for a in args))


if __name__ == "__main__":
    unittest.main()

