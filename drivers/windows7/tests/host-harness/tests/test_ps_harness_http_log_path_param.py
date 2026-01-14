#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PsHarnessHttpLogPathParamTests(unittest.TestCase):
    def test_http_log_path_logging_is_single_line_per_request(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        text = ps_path.read_text(encoding="utf-8", errors="replace")

        # Guardrail: the harness should append exactly one log line per request.
        # We enforce this by checking there is only one AppendAllText($HttpLogPath, ...) site in the script.
        appends = re.findall(r"AppendAllText\(\$HttpLogPath\b", text)
        self.assertEqual(
            len(appends),
            1,
            "expected exactly one AppendAllText($HttpLogPath, ...) call (avoid duplicate log lines per request)",
        )

        # Ensure the log line has the expected field order: method path status_code bytes.
        self.assertRegex(
            text,
            r'\$line\s*=\s*"\$logMethod \$logPath \$statusCode \$bytesSent',
            "expected HTTP log line to contain: $logMethod $logPath $statusCode $bytesSent",
        )


if __name__ == "__main__":
    unittest.main()
