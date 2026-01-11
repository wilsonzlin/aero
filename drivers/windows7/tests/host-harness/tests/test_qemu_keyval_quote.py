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


class QemuKeyvalQuoteTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_escapes_backslashes_and_quotes(self) -> None:
        f = self.harness._qemu_quote_keyval_value

        # Typical Windows path: must preserve backslashes.
        self.assertEqual(f(r"C:\a\b"), '"C:\\\\a\\\\b"')

        # Quotes inside the path should be escaped for QEMU keyval parsing.
        self.assertEqual(
            f('C:\\path with "quotes"\\file.wav'),
            '"C:\\\\path with \\"quotes\\"\\\\file.wav"',
        )

    def test_leaves_commas_unescaped_inside_quotes(self) -> None:
        f = self.harness._qemu_quote_keyval_value
        out = f(r"C:\with,comma\file.wav")
        self.assertEqual(out, '"C:\\\\with,comma\\\\file.wav"')


if __name__ == "__main__":
    unittest.main()
