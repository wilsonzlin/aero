#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class PowerShellHarnessMarkerRegexEscapingTests(unittest.TestCase):
    def setUp(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = ps_path.read_text(encoding="utf-8", errors="replace")

    def test_aero_marker_prefixes_do_not_use_double_backslash_pipe_escaping(self) -> None:
        # PowerShell double-quoted strings do not treat backslash as an escape. A regex like "\\|"
        # therefore matches a literal backslash + alternation ("|"), not a literal pipe.
        #
        # We defensively ensure we don't regress to the broken pattern for our marker contracts.
        self.assertNotIn(r"AERO_VIRTIO_SELFTEST\\|", self.text)
        self.assertNotIn(r"AERO_VIRTIO_WIN7_HOST\\|", self.text)


if __name__ == "__main__":
    unittest.main()

