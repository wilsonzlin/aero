#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import contextlib
import importlib.util
import io
import sys
import unittest
from pathlib import Path
from unittest import mock


def _load_probe():
    script_path = Path(__file__).resolve().parents[1] / "probe_qemu_virtio_pci_ids.py"
    spec = importlib.util.spec_from_file_location("probe_qemu_virtio_pci_ids", script_path)
    assert spec and spec.loader

    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class ProbeQemuVirtioPciIdsMissingQemuSystemTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.probe = _load_probe()

    def test_missing_qemu_system_prints_friendly_error(self) -> None:
        probe = self.probe
        argv = [
            "probe_qemu_virtio_pci_ids.py",
            "--qemu-system",
            "qemu-system-x86_64",
        ]
        stderr = io.StringIO()
        with (
            mock.patch.object(sys, "argv", argv),
            contextlib.redirect_stderr(stderr),
            mock.patch.object(probe.subprocess, "Popen", side_effect=FileNotFoundError()),
        ):
            rc = probe.main()

        self.assertEqual(rc, 2)
        self.assertIn("ERROR: qemu-system binary not found:", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()

