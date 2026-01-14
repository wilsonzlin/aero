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


class QmpQueryPciParsingTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_parses_int_fields(self) -> None:
        h = self.harness
        query = [
            {
                "bus": 0,
                "devices": [
                    {
                        "vendor_id": 0x1AF4,
                        "device_id": 0x1041,
                        "subsystem_vendor_id": 0x1AF4,
                        "subsystem_id": 0x0001,
                        "revision": 0x01,
                    },
                ],
            }
        ]
        devs = h._iter_qmp_query_pci_devices(query)
        self.assertEqual(len(devs), 1)
        d = devs[0]
        self.assertEqual(d.vendor_id, 0x1AF4)
        self.assertEqual(d.device_id, 0x1041)
        self.assertEqual(d.subsystem_vendor_id, 0x1AF4)
        self.assertEqual(d.subsystem_id, 0x0001)
        self.assertEqual(d.revision, 0x01)

    def test_parses_string_fields(self) -> None:
        h = self.harness
        query = [
            {
                "devices": [
                    {
                        "vendor_id": "0x1af4",
                        "device_id": "0x1042",
                        "revision": "0x01",
                    },
                ]
            }
        ]
        devs = h._iter_qmp_query_pci_devices(query)
        self.assertEqual(len(devs), 1)
        self.assertEqual(devs[0].vendor_id, 0x1AF4)
        self.assertEqual(devs[0].device_id, 0x1042)
        self.assertEqual(devs[0].revision, 0x01)

    def test_parses_nested_id_object(self) -> None:
        h = self.harness
        query = [
            {
                "devices": [
                    {
                        # Some QEMU builds nest IDs under an `id` object.
                        "id": {"vendor_id": "0x1af4", "device_id": "0x1041", "revision": "0x01"},
                    },
                ]
            }
        ]
        devs = h._iter_qmp_query_pci_devices(query)
        self.assertEqual(len(devs), 1)
        self.assertEqual(devs[0].vendor_id, 0x1AF4)
        self.assertEqual(devs[0].device_id, 0x1041)
        self.assertEqual(devs[0].revision, 0x01)

    def test_recurses_pci_bridge_bus(self) -> None:
        h = self.harness
        query = [
            {
                "bus": 0,
                "devices": [
                    {
                        "bus": 0,
                        "slot": 1,
                        "function": 0,
                        "vendor_id": 0x8086,
                        "device_id": 0x1234,
                        "pci_bridge": {
                            "bus": {
                                "number": 1,
                                "devices": [
                                    {
                                        "bus": 1,
                                        "slot": 2,
                                        "function": 0,
                                        "id": {"vendor_id": 0x1AF4, "device_id": 0x1041, "revision": 0x01},
                                    }
                                ],
                            }
                        },
                    }
                ],
            }
        ]
        devs = h._iter_qmp_query_pci_devices(query)
        self.assertTrue(any(d.vendor_id == 0x1AF4 and d.device_id == 0x1041 for d in devs))

    def test_ignores_missing_vendor_or_device_id(self) -> None:
        h = self.harness
        query = [
            {"devices": [{"vendor_id": 0x1AF4}]},
            {"devices": [{"device_id": 0x1041}]},
            {"devices": [{"vendor_id": None, "device_id": 0x1041}]},
        ]
        devs = h._iter_qmp_query_pci_devices(query)
        self.assertEqual(devs, [])

    def test_handles_non_list_input(self) -> None:
        h = self.harness
        devs = h._iter_qmp_query_pci_devices({"not": "a list"})
        self.assertEqual(devs, [])


if __name__ == "__main__":
    unittest.main()
