#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import contextlib
import importlib.util
import io
import sys
import unittest
from pathlib import Path
from unittest import mock


def _load_harness():
    harness_path = Path(__file__).resolve().parents[1] / "invoke_aero_virtio_win7_tests.py"
    spec = importlib.util.spec_from_file_location("invoke_aero_virtio_win7_tests", harness_path)
    assert spec and spec.loader

    module = importlib.util.module_from_spec(spec)
    # Register the module before execution so dataclasses can resolve __module__ correctly.
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class QmpPciPreflightTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_contract_v1_pass(self) -> None:
        h = self.harness
        query = [
            {
                "bus": 0,
                "devices": [
                    {
                        # Exercise the common q35/PCIe root-port structure where devices are behind
                        # a bridge (pci_bridge.bus.devices).
                        "bus": 0,
                        "slot": 1,
                        "function": 0,
                        "vendor_id": 0x8086,
                        "device_id": 0x1234,
                        "pci_bridge": {
                            "bus": {
                                "number": 1,
                                "devices": [
                                    {"vendor_id": 0x1AF4, "device_id": 0x1041, "revision": 0x01},
                                    {"vendor_id": 0x1AF4, "device_id": 0x1042, "revision": 0x01},
                                    {"vendor_id": 0x1AF4, "device_id": 0x1052, "revision": 0x01},
                                    {"vendor_id": 0x1AF4, "device_id": 0x1052, "revision": 0x01},
                                ],
                            }
                        },
                    }
                ],
            }
        ]

        out = io.StringIO()
        with (
            mock.patch.object(h, "_qmp_query_pci", return_value=query),
            contextlib.redirect_stdout(out),
        ):
            h._qmp_pci_preflight(
                h._QmpEndpoint(tcp_host="127.0.0.1", tcp_port=1),
                virtio_transitional=False,
                with_virtio_snd=False,
                with_virtio_tablet=False,
            )

        self.assertIn(
            "AERO_VIRTIO_WIN7_HOST|QEMU_PCI_PREFLIGHT|PASS|mode=contract-v1|vendor=1af4|devices=",
            out.getvalue(),
        )

    def test_transitional_pass_is_permissive(self) -> None:
        h = self.harness
        query = [
            {
                "devices": [
                    # Transitional IDs and non-REV_01 should still PASS in transitional mode.
                    {"vendor_id": 0x1AF4, "device_id": 0x1000, "revision": 0x00},
                ]
            }
        ]

        out = io.StringIO()
        with (
            mock.patch.object(h, "_qmp_query_pci", return_value=query),
            contextlib.redirect_stdout(out),
        ):
            h._qmp_pci_preflight(
                h._QmpEndpoint(tcp_host="127.0.0.1", tcp_port=1),
                virtio_transitional=True,
                with_virtio_snd=False,
                with_virtio_tablet=False,
            )

        self.assertIn(
            "AERO_VIRTIO_WIN7_HOST|QEMU_PCI_PREFLIGHT|PASS|mode=transitional|vendor=1af4|devices=",
            out.getvalue(),
        )

    def test_contract_v1_fails_on_bad_revision(self) -> None:
        h = self.harness
        query = [
            {
                "devices": [
                    {"vendor_id": 0x1AF4, "device_id": 0x1041, "revision": 0x00},
                    {"vendor_id": 0x1AF4, "device_id": 0x1042, "revision": 0x01},
                    {"vendor_id": 0x1AF4, "device_id": 0x1052, "revision": 0x01},
                    {"vendor_id": 0x1AF4, "device_id": 0x1052, "revision": 0x01},
                ]
            }
        ]

        with mock.patch.object(h, "_qmp_query_pci", return_value=query):
            with self.assertRaises(RuntimeError) as ctx:
                h._qmp_pci_preflight(
                    h._QmpEndpoint(tcp_host="127.0.0.1", tcp_port=1),
                    virtio_transitional=False,
                    with_virtio_snd=False,
                    with_virtio_tablet=False,
                )

        msg = str(ctx.exception)
        self.assertIn("QEMU PCI preflight failed", msg)
        self.assertIn("Unexpected revision IDs", msg)
        self.assertIn("1AF4:1041@00", msg)


if __name__ == "__main__":
    unittest.main()
