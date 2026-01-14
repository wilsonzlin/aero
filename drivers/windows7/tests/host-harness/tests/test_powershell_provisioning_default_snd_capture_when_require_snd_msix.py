#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellProvisioningDefaultSndCaptureWhenRequireSndMsixTests(unittest.TestCase):
    def test_test_snd_capture_defaults_on_when_require_snd_msix(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "New-AeroWin7TestImage.ps1"
        text = ps_path.read_text(encoding="utf-8", errors="replace")

        # The provisioning script defaults -TestSndCapture on when virtio-snd is being required/tested
        # (important for older guest selftests where capture/duplex is opt-in). Ensure RequireSndMsix
        # participates in that defaulting rule.
        self.assertRegex(
            text,
            re.compile(
                r"\(\$RequireSnd\s+-or\s+\$RequireSndMsix\s+-or\s+\$RequireSndCapture\s+-or\s+\$RequireNonSilence\s+-or\s+\$TestSndBufferLimits\)",
                re.IGNORECASE,
            ),
        )


if __name__ == "__main__":
    unittest.main()

