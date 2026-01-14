#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellBlkIrqMarkerMsixInferenceTests(unittest.TestCase):
    def setUp(self) -> None:
        ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = ps_path.read_text(encoding="utf-8", errors="replace")

    def _extract_function_text(self, name: str) -> str:
        m = re.search(rf"(?m)^\s*function\s+{re.escape(name)}\b", self.text)
        self.assertIsNotNone(m, f"missing PowerShell function {name}")
        assert m is not None
        start = m.start()

        m2 = re.search(r"(?m)^\s*function\s+", self.text[m.end() :])
        end = len(self.text) if m2 is None else (m.end() + m2.start())
        return self.text[start:end]

    def test_mode_msi_is_upgraded_to_msix_when_vectors_present(self) -> None:
        fn = self._extract_function_text("Try-EmitAeroVirtioBlkIrqMarker")

        # Ensure the harness infers MSI-X even when some guest diagnostics report mode=msi
        # but provide MSI-X vector indices. This keeps VIRTIO_BLK_IRQ consistent with the
        # per-test marker semantics (`irq_mode=msix`).
        self.assertRegex(
            fn,
            re.compile(
                r"\$mode\.Trim\(\)\.ToLowerInvariant\(\)\s*-eq\s*[\'\"]msi[\'\"][\s\S]*?"
                r"foreach\s*\(\s*\$vec\s+in\s+@\(\s*\$msixConfigVector\s*,\s*\$msixQueueVector\s*\)\s*\)\s*\{[\s\S]*?"
                r"\$mode\s*=\s*[\'\"]msix[\'\"]",
                re.IGNORECASE,
            ),
        )


if __name__ == "__main__":
    unittest.main()
