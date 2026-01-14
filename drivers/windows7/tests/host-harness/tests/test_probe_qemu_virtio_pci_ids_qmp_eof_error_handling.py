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


class _DummyProc:
    def __init__(self) -> None:
        self.stdin = io.StringIO()
        # Empty stdout -> QMP EOF.
        self.stdout = io.StringIO("")
        self.stderr = io.StringIO("qemu: failed to start\n")
        self.returncode: int | None = None
        self.killed = False

    def wait(self, timeout: float | None = None) -> int:
        self.returncode = 1
        return 1

    def poll(self) -> int | None:
        return self.returncode

    def kill(self) -> None:
        self.killed = True
        self.returncode = 1


class ProbeQemuVirtioPciIdsQmpEofErrorHandlingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.probe = _load_probe()

    def test_qmp_eof_is_reported_as_error_without_traceback(self) -> None:
        probe = self.probe
        argv = [
            "probe_qemu_virtio_pci_ids.py",
            "--qemu-system",
            "qemu-system-x86_64",
        ]

        stderr = io.StringIO()
        with (
            mock.patch.object(sys, "argv", argv),
            mock.patch.object(probe.subprocess, "Popen", return_value=_DummyProc()) as mock_popen,
            contextlib.redirect_stderr(stderr),
        ):
            rc = probe.main()

        self.assertEqual(rc, 2)
        err = stderr.getvalue()
        self.assertIn("ERROR: QMP EOF while waiting for response", err)
        self.assertIn("--- QEMU stderr ---", err)
        self.assertIn("qemu: failed to start", err)
        mock_popen.assert_called()


if __name__ == "__main__":
    unittest.main()

