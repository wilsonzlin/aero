#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import contextlib
import importlib.util
import io
import sys
import tempfile
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


class ProbeQemuVirtioPciIdsQemuSystemDirectoryValidationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.probe = _load_probe()

    def test_directory_qemu_system_fails_fast(self) -> None:
        probe = self.probe
        with tempfile.TemporaryDirectory() as td:
            qemu_dir = Path(td) / "qemu-dir"
            qemu_dir.mkdir()

            argv = [
                "probe_qemu_virtio_pci_ids.py",
                "--qemu-system",
                str(qemu_dir),
            ]

            stderr = io.StringIO()
            with (
                mock.patch.object(sys, "argv", argv),
                contextlib.redirect_stderr(stderr),
                mock.patch.object(probe.subprocess, "Popen") as mock_popen,
            ):
                rc = probe.main()

        self.assertEqual(rc, 2)
        self.assertIn("qemu system binary path is a directory", stderr.getvalue())
        mock_popen.assert_not_called()


if __name__ == "__main__":
    unittest.main()
