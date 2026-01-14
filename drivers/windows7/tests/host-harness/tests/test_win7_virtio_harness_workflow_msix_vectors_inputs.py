#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class Win7VirtioHarnessWorkflowMsixVectorsInputsTests(unittest.TestCase):
    def setUp(self) -> None:
        # This test is intentionally a simple text scan (no PyYAML dependency).
        # __file__ is:
        #   drivers/windows7/tests/host-harness/tests/test_*.py
        # so repo root is parents[5] (../..../..../..../..).
        repo_root = Path(__file__).resolve().parents[5]
        self.workflow_path = repo_root / ".github" / "workflows" / "win7-virtio-harness.yml"
        self.text = self.workflow_path.read_text(encoding="utf-8", errors="replace")

    def test_workflow_plumbs_virtio_transitional(self) -> None:
        self.assertIn("virtio_transitional:", self.text)
        self.assertIn('virtio_transitional="${{ inputs.virtio_transitional }}"', self.text)
        self.assertIn("args+=(--virtio-transitional)", self.text)

    def test_workflow_plumbs_msix_requirements(self) -> None:
        self.assertIn("require_virtio_net_msix:", self.text)
        self.assertIn('require_virtio_net_msix="${{ inputs.require_virtio_net_msix }}"', self.text)
        self.assertIn("args+=(--require-virtio-net-msix)", self.text)

        self.assertIn("require_virtio_blk_msix:", self.text)
        self.assertIn('require_virtio_blk_msix="${{ inputs.require_virtio_blk_msix }}"', self.text)
        self.assertIn("args+=(--require-virtio-blk-msix)", self.text)

        self.assertIn("require_virtio_snd_msix:", self.text)
        self.assertIn('require_virtio_snd_msix="${{ inputs.require_virtio_snd_msix }}"', self.text)
        self.assertIn("args+=(--require-virtio-snd-msix)", self.text)

        self.assertIn("require_virtio_input_msix:", self.text)
        self.assertIn('require_virtio_input_msix="${{ inputs.require_virtio_input_msix }}"', self.text)
        self.assertIn("args+=(--require-virtio-input-msix)", self.text)

    def test_workflow_plumbs_msix_vectors(self) -> None:
        self.assertIn("virtio_msix_vectors:", self.text)
        self.assertIn('virtio_msix_vectors="${{ inputs.virtio_msix_vectors }}"', self.text)
        self.assertIn('args+=(--virtio-msix-vectors "${virtio_msix_vectors}")', self.text)

        self.assertIn("virtio_net_vectors:", self.text)
        self.assertIn('virtio_net_vectors="${{ inputs.virtio_net_vectors }}"', self.text)
        self.assertIn('args+=(--virtio-net-vectors "${virtio_net_vectors}")', self.text)

        self.assertIn("virtio_blk_vectors:", self.text)
        self.assertIn('virtio_blk_vectors="${{ inputs.virtio_blk_vectors }}"', self.text)
        self.assertIn('args+=(--virtio-blk-vectors "${virtio_blk_vectors}")', self.text)

        self.assertIn("virtio_input_vectors:", self.text)
        self.assertIn('virtio_input_vectors="${{ inputs.virtio_input_vectors }}"', self.text)
        self.assertIn('args+=(--virtio-input-vectors "${virtio_input_vectors}")', self.text)

        self.assertIn("virtio_snd_vectors:", self.text)
        self.assertIn('virtio_snd_vectors="${{ inputs.virtio_snd_vectors }}"', self.text)
        self.assertIn('args+=(--virtio-snd-vectors "${virtio_snd_vectors}")', self.text)


if __name__ == "__main__":
    unittest.main()

