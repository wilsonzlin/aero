#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class PowerShellHarnessVirtioSndForceNullBackendTokenTests(unittest.TestCase):
    def test_force_null_backend_failure_token_exists(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        text = ps_path.read_text(encoding="utf-8", errors="replace")

        # The host harness should surface the guest marker FAIL|force_null_backend as a deterministic token.
        self.assertIn("VIRTIO_SND_FORCE_NULL_BACKEND", text)
        self.assertIn("force_null_backend", text)

        # The failure message should include the canonical per-device registry path so the
        # user can remediate quickly.
        self.assertIn(
            r"HKLM\SYSTEM\CurrentControlSet\Enum\<DeviceInstancePath>\Device Parameters\Parameters\ForceNullBackend",
            text,
        )


if __name__ == "__main__":
    unittest.main()

