#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path


def _load_harness():
    harness_path = Path(__file__).resolve().parents[1] / "invoke_aero_virtio_win7_tests.py"
    spec = importlib.util.spec_from_file_location("invoke_aero_virtio_win7_tests", harness_path)
    assert spec and spec.loader

    module = importlib.util.module_from_spec(spec)
    # Register the module before execution so dataclasses can resolve __module__ correctly.
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class QmpMsixParsingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_query_pci_msix_enabled_disabled(self) -> None:
        h = self.harness
        query = [
            {
                "bus": 0,
                "devices": [
                    {
                        "bus": 0,
                        "slot": 3,
                        "function": 0,
                        "vendor_id": 0x1AF4,
                        "device_id": 0x1041,
                        "capabilities": [{"id": "msix", "msix": {"enabled": True}}],
                    },
                    {
                        "bus": "0x0",
                        "slot": "0x4",
                        "function": 0,
                        "vendor_id": "0x1af4",
                        "device_id": "0x1042",
                        "capabilities": [{"id": "msix", "msix": {"enabled": False}}],
                    },
                ],
            }
        ]
        infos = h._parse_qmp_query_pci_msix_info(query)

        net = next(i for i in infos if i.vendor_id == 0x1AF4 and i.device_id == 0x1041)
        self.assertEqual(net.msix_enabled, True)
        self.assertEqual(net.bdf(), "00:03.0")
        self.assertEqual(net.source, "query-pci")

        blk = next(i for i in infos if i.vendor_id == 0x1AF4 and i.device_id == 0x1042)
        self.assertEqual(blk.msix_enabled, False)
        self.assertEqual(blk.bdf(), "00:04.0")

    def test_query_pci_msix_missing_enabled_field(self) -> None:
        h = self.harness
        query = [
            {
                "bus": 0,
                "devices": [
                    {
                        "bus": 0,
                        "slot": 5,
                        "function": 0,
                        "vendor_id": 0x1AF4,
                        "device_id": 0x1041,
                        "capabilities": [{"id": "msix", "msix": {"table_size": 4}}],
                    }
                ],
            }
        ]
        infos = h._parse_qmp_query_pci_msix_info(query)
        net = next(i for i in infos if i.vendor_id == 0x1AF4 and i.device_id == 0x1041)
        self.assertIsNone(net.msix_enabled)

    def test_hmp_info_pci_msix_enabled_disabled(self) -> None:
        h = self.harness
        info_pci = "\n".join(
            [
                "Bus 0, device 3, function 0:",
                "  Ethernet controller: Device 1af4:1041 (rev 00)",
                "    MSI-X: Enabled+ Count=4 Masked-",
                "Bus 0, device 4, function 0:",
                "  Device 1af4:1042",
                "    MSI-X: disabled",
                "",
            ]
        )
        infos = h._parse_hmp_info_pci_msix_info(info_pci)

        net = next(i for i in infos if i.vendor_id == 0x1AF4 and i.device_id == 0x1041)
        self.assertEqual(net.msix_enabled, True)
        self.assertEqual(net.bdf(), "00:03.0")
        self.assertEqual(net.source, "info pci")

        blk = next(i for i in infos if i.vendor_id == 0x1AF4 and i.device_id == 0x1042)
        self.assertEqual(blk.msix_enabled, False)
        self.assertEqual(blk.bdf(), "00:04.0")

    def test_require_virtio_snd_msix_disabled_token(self) -> None:
        h = self.harness

        # Stub the QMP collector so we can unit test the failure token without a real QMP socket.
        dev_id = sorted(h._VIRTIO_SND_PCI_DEVICE_IDS)[0]
        query_infos = [
            h._PciMsixInfo(
                vendor_id=h._VIRTIO_PCI_VENDOR_ID,
                device_id=dev_id,
                bus=0,
                slot=5,
                function=0,
                msix_enabled=False,
                source="query-pci",
            )
        ]

        def stub_collect(_endpoint):
            return query_infos, [], True, False

        orig = h._qmp_collect_pci_msix_info
        h._qmp_collect_pci_msix_info = stub_collect
        try:
            msg = h._require_virtio_msix_check_failure_message(
                h._QmpEndpoint(tcp_host="127.0.0.1", tcp_port=1),
                require_virtio_net_msix=False,
                require_virtio_blk_msix=False,
                require_virtio_snd_msix=True,
            )
        finally:
            h._qmp_collect_pci_msix_info = orig

        assert msg is not None
        self.assertTrue(
            msg.startswith("FAIL: VIRTIO_SND_MSIX_NOT_ENABLED:"),
            msg,
        )

    def test_require_virtio_blk_msix_disabled_token(self) -> None:
        h = self.harness

        # Stub the QMP collector so we can unit test the failure token without a real QMP socket.
        dev_id = sorted(h._VIRTIO_BLK_PCI_DEVICE_IDS)[0]
        query_infos = [
            h._PciMsixInfo(
                vendor_id=h._VIRTIO_PCI_VENDOR_ID,
                device_id=dev_id,
                bus=0,
                slot=5,
                function=0,
                msix_enabled=False,
                source="query-pci",
            )
        ]

        def stub_collect(_endpoint):
            return query_infos, [], True, False

        orig = h._qmp_collect_pci_msix_info
        h._qmp_collect_pci_msix_info = stub_collect
        try:
            msg = h._require_virtio_msix_check_failure_message(
                h._QmpEndpoint(tcp_host="127.0.0.1", tcp_port=1),
                require_virtio_net_msix=False,
                require_virtio_blk_msix=True,
                require_virtio_snd_msix=False,
            )
        finally:
            h._qmp_collect_pci_msix_info = orig

        assert msg is not None
        self.assertTrue(
            msg.startswith("FAIL: VIRTIO_BLK_MSIX_NOT_ENABLED:"),
            msg,
        )

    def test_require_virtio_net_msix_disabled_token(self) -> None:
        h = self.harness

        # Stub the QMP collector so we can unit test the failure token without a real QMP socket.
        dev_id = sorted(h._VIRTIO_NET_PCI_DEVICE_IDS)[0]
        query_infos = [
            h._PciMsixInfo(
                vendor_id=h._VIRTIO_PCI_VENDOR_ID,
                device_id=dev_id,
                bus=0,
                slot=5,
                function=0,
                msix_enabled=False,
                source="query-pci",
            )
        ]

        def stub_collect(_endpoint):
            return query_infos, [], True, False

        orig = h._qmp_collect_pci_msix_info
        h._qmp_collect_pci_msix_info = stub_collect
        try:
            msg = h._require_virtio_msix_check_failure_message(
                h._QmpEndpoint(tcp_host="127.0.0.1", tcp_port=1),
                require_virtio_net_msix=True,
                require_virtio_blk_msix=False,
                require_virtio_snd_msix=False,
            )
        finally:
            h._qmp_collect_pci_msix_info = orig

        assert msg is not None
        self.assertTrue(
            msg.startswith("FAIL: VIRTIO_NET_MSIX_NOT_ENABLED:"),
            msg,
        )


if __name__ == "__main__":
    unittest.main()
