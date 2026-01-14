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


class HarnessNoAbbrevTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_parser_does_not_accept_abbreviated_long_options(self) -> None:
        """
        The harness uses parse_known_args() so additional QEMU argv elements can be appended. If
        argparse abbreviation matching is enabled, unknown QEMU args that happen to prefix-match a
        harness flag (or a user typo) can be consumed as a harness option, silently changing
        behavior. Ensure allow_abbrev is disabled.
        """
        parser = self.harness._build_arg_parser()
        args, extra = parser.parse_known_args(
            [
                "--qemu-system",
                "qemu-system-x86_64",
                "--disk-image",
                "disk.img",
                # Would be accepted as an abbreviation for --dry-run if allow_abbrev=True.
                "--dry",
            ]
        )
        self.assertFalse(args.dry_run)
        self.assertEqual(extra, ["--dry"])

