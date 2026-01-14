#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from collections import Counter, defaultdict
from pathlib import Path


class PowerShellHarnessResultTokenUniquenessTests(unittest.TestCase):
    def test_result_switch_cases_are_unique(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        lines = ps_path.read_text(encoding="utf-8", errors="replace").splitlines()

        # Only scan the main result switch (`switch ($result.Result) { ... }`) to avoid
        # counting unrelated switches earlier in the script (e.g. audio backend selection).
        switch_re = re.compile(r"\bswitch\s*\(\s*\$result\.Result\s*\)\s*\{")
        start = None
        for i, line in enumerate(lines):
            if switch_re.search(line):
                start = i
                break
        self.assertIsNotNone(start, "did not find switch ($result.Result) in PowerShell harness")
        assert start is not None

        # Case labels are of the form: "TOKEN" { ... }.
        case_re = re.compile(r'^\s*"([A-Z0-9_]+)"\s*\{\s*$')
        counts: Counter[str] = Counter()
        occur: dict[str, list[int]] = defaultdict(list)

        for lineno, line in enumerate(lines[start:], start=start + 1):
            m = case_re.match(line)
            if not m:
                continue
            tok = m.group(1)
            counts[tok] += 1
            occur[tok].append(lineno)

        dups = {tok: locs for tok, locs in occur.items() if len(locs) > 1}
        if dups:
            msg = "duplicate result tokens in Invoke-AeroVirtioWin7Tests.ps1:\n"
            for tok, locs in sorted(dups.items()):
                msg += f"  {tok}: lines {locs}\n"
            self.fail(msg)


if __name__ == "__main__":
    unittest.main()

