#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class GuestSelftestLastErrorLoggingTests(unittest.TestCase):
    def setUp(self) -> None:
        cpp_path = (
            Path(__file__).resolve().parents[2] / "guest-selftest" / "src" / "main.cpp"
        )
        self.lines = cpp_path.read_text(encoding="utf-8", errors="replace").splitlines()

    def test_logf_does_not_mix_widetoutf8_with_getlasterror_in_args(self) -> None:
        # Calling GetLastError() inside a log.Logf(...) call that also calls WideToUtf8(...)
        # is unsafe: evaluation order of function arguments is unspecified and WideToUtf8()
        # calls Win32 APIs that may clobber LastError. Capture the error into a local before
        # formatting it into a log line.
        offending: list[tuple[int, str]] = []
        for i, line in enumerate(self.lines):
            if "log.Logf" not in line or "WideToUtf8" not in line:
                continue
            call = line
            j = i
            while ");" not in call and j + 1 < len(self.lines) and j - i < 40:
                j += 1
                call += "\n" + self.lines[j]
            if "GetLastError" in call:
                offending.append((i + 1, call))

        if offending:
            first_line, call = offending[0]
            self.fail(
                f"Found log.Logf call mixing WideToUtf8 and GetLastError at main.cpp:{first_line}:\n{call}"
            )


if __name__ == "__main__":
    unittest.main()

