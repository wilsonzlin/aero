#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class ProbeQemuVirtioPciIdsNoAbbrevTests(unittest.TestCase):
    def test_probe_disables_argparse_abbreviations(self) -> None:
        """
        The probe script is often invoked from CI/shell scripts. Ensure argparse long-option
        abbreviation matching is disabled so unknown args/typos cannot be silently consumed as
        abbreviated options.
        """
        probe_path = Path(__file__).resolve().parents[1] / "probe_qemu_virtio_pci_ids.py"
        text = probe_path.read_text(encoding="utf-8", errors="replace")
        self.assertIn("argparse.ArgumentParser(allow_abbrev=False)", text)


if __name__ == "__main__":
    unittest.main()

