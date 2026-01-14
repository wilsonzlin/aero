#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import unittest
from pathlib import Path


class Win7VirtioHarnessWorkflowJobSummaryNetBlkMarkersTests(unittest.TestCase):
    def setUp(self) -> None:
        # This test is intentionally a simple text scan (no PyYAML dependency).
        # __file__ is:
        #   drivers/windows7/tests/host-harness/tests/test_*.py
        # so repo root is parents[5] (../..../..../..../..).
        repo_root = Path(__file__).resolve().parents[5]
        self.workflow_path = repo_root / ".github" / "workflows" / "win7-virtio-harness.yml"
        self.text = self.workflow_path.read_text(encoding="utf-8", errors="replace")

    def test_job_summary_surfaces_host_net_blk_markers(self) -> None:
        # Host marker extraction
        for marker in (
            "VIRTIO_BLK_IO|",
            "VIRTIO_BLK_MSIX|",
            "VIRTIO_BLK_RESET_RECOVERY|",
            "VIRTIO_BLK_MINIPORT_FLAGS|",
            "VIRTIO_BLK_MINIPORT_RESET_RECOVERY|",
            "VIRTIO_NET_LARGE|",
            "VIRTIO_NET_UDP|",
            "VIRTIO_NET_UDP_DNS|",
            "VIRTIO_NET_OFFLOAD_CSUM|",
            "VIRTIO_NET_DIAG|",
            "VIRTIO_NET_MSIX|",
        ):
            with self.subTest(marker=marker):
                self.assertIn(marker, self.text)

        # Job summary bullet labels
        self.assertIn("Host virtio-blk I/O marker", self.text)
        self.assertIn("Host virtio-blk MSI-X marker", self.text)
        self.assertIn("Host virtio-blk-reset-recovery marker", self.text)
        self.assertIn("Host virtio-blk miniport flags marker", self.text)
        self.assertIn("Host virtio-blk miniport reset recovery marker", self.text)
        self.assertIn("Host virtio-net-large marker", self.text)
        self.assertIn("Host virtio-net-udp marker", self.text)
        self.assertIn("Host virtio-net-udp-dns marker", self.text)
        self.assertIn("Host virtio-net-offload-csum marker", self.text)
        self.assertIn("Host virtio-net-diag marker", self.text)
        self.assertIn("Host virtio-net MSI-X marker", self.text)


if __name__ == "__main__":
    unittest.main()
