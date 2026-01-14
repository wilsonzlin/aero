#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class NewAeroWin7TestImageNetLinkFlapFlagTests(unittest.TestCase):
    def setUp(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "New-AeroWin7TestImage.ps1"
        self.text = ps_path.read_text(encoding="utf-8", errors="replace")

    def test_test_net_link_flap_param_exists(self) -> None:
        # Ensure provisioning script generator exposes the switch.
        self.assertIn("[switch]$TestNetLinkFlap", self.text)

    def test_test_net_link_flap_arg_is_baked_into_scheduled_task(self) -> None:
        # Ensure the generator appends --test-net-link-flap when -TestNetLinkFlap is set.
        self.assertIn('$testNetLinkFlapArg = " --test-net-link-flap"', self.text)

        # Ensure the scheduled task commandline includes the arg variable (avoid brittle ordering assumptions).
        # We match against the specific AeroVirtioSelftest task creation line so this doesn't accidentally
        # bind to earlier schtasks-related comments.
        self.assertRegex(
            self.text,
            re.compile(
                r'schtasks /Create /F /TN "AeroVirtioSelftest".*\$testNetLinkFlapArg',
                re.IGNORECASE | re.DOTALL,
            ),
        )

    def test_readme_mentions_test_net_link_flap(self) -> None:
        self.assertIn("`-TestNetLinkFlap` (adds `--test-net-link-flap` to the scheduled task)", self.text)


if __name__ == "__main__":
    unittest.main()
