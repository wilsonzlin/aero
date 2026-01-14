#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import contextlib
import importlib.util
import io
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


class HarnessVectorsFlagValidationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_virtio_snd_vectors_requires_with_virtio_snd(self) -> None:
        h = self.harness

        old_argv = sys.argv
        try:
            sys.argv = [
                "invoke_aero_virtio_win7_tests.py",
                "--qemu-system",
                "qemu-system-x86_64",
                "--disk-image",
                "disk.img",
                "--virtio-snd-vectors",
                "2",
            ]
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr), self.assertRaises(SystemExit) as cm:
                h.main()
            # argparse uses exit code 2 for CLI usage errors.
            self.assertEqual(cm.exception.code, 2)
            self.assertIn("--virtio-snd-vectors requires --with-virtio-snd", stderr.getvalue())
        finally:
            sys.argv = old_argv


if __name__ == "__main__":
    unittest.main()

