#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import contextlib
import importlib.util
import io
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


class VirtioNetDiagMarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def _emit(self, tail: bytes) -> str:
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.harness._emit_virtio_net_diag_host_marker(tail)
        return buf.getvalue().strip()

    def test_emits_info_marker(self) -> None:
        tail = (
            b"virtio-net-diag|INFO|host_features=0x1|guest_features=0x2|irq_mode=msix|"
            b"irq_message_count=3|msix_config_vector=0x0000|msix_rx_vector=0x0001|"
            b"msix_tx_vector=0x0002|rx_queue_size=256|tx_queue_size=256\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_DIAG|INFO|host_features=0x1|guest_features=0x2|"
            "irq_mode=msix|irq_message_count=3|msix_config_vector=0x0000|msix_rx_vector=0x0001|"
            "msix_tx_vector=0x0002|rx_queue_size=256|tx_queue_size=256",
        )

    def test_emits_warn_not_supported(self) -> None:
        tail = b"virtio-net-diag|WARN|reason=not_supported\n"
        out = self._emit(tail)
        self.assertEqual(out, "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_DIAG|WARN|reason=not_supported")

    def test_uses_last_marker(self) -> None:
        tail = (
            b"virtio-net-diag|WARN|reason=not_supported\n"
            b"virtio-net-diag|INFO|host_features=0x3\n"
        )
        out = self._emit(tail)
        self.assertEqual(out, "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_DIAG|INFO|host_features=0x3")

    def test_emits_queue_error_flags(self) -> None:
        tail = (
            b"virtio-net-diag|INFO|host_features=0x1|guest_features=0x2|irq_mode=msix|irq_message_count=3|"
            b"rx_avail_idx=1|rx_used_idx=2|tx_avail_idx=3|tx_used_idx=4|"
            b"rx_vq_error_flags=0x00000000|tx_vq_error_flags=0x00000001\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_DIAG|INFO|host_features=0x1|guest_features=0x2|"
            "irq_mode=msix|irq_message_count=3|rx_avail_idx=1|rx_used_idx=2|tx_avail_idx=3|tx_used_idx=4|"
            "rx_vq_error_flags=0x00000000|tx_vq_error_flags=0x00000001",
        )

    def test_emits_udp_checksum_fields(self) -> None:
        tail = (
            b"virtio-net-diag|INFO|host_features=0x1|guest_features=0x2|irq_mode=msix|irq_message_count=3|"
            b"tx_csum_v4=1|tx_csum_v6=1|tx_udp_csum_v4=0|tx_udp_csum_v6=1|"
            b"tx_tso_v4=1|tx_tso_v6=0|tx_tso_max_size=65536\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_DIAG|INFO|host_features=0x1|guest_features=0x2|"
            "irq_mode=msix|irq_message_count=3|"
            "tx_csum_v4=1|tx_csum_v6=1|tx_udp_csum_v4=0|tx_udp_csum_v6=1|"
            "tx_tso_v4=1|tx_tso_v6=0|tx_tso_max_size=65536",
        )

    def test_emits_tx_checksum_counters(self) -> None:
        tail = (
            b"virtio-net-diag|INFO|host_features=0x1|guest_features=0x2|irq_mode=msix|irq_message_count=3|"
            b"tx_csum_v4=1|tx_csum_v6=1|tx_udp_csum_v4=0|tx_udp_csum_v6=1|"
            b"tx_tcp_csum_offload_pkts=10|tx_tcp_csum_fallback_pkts=2|"
            b"tx_udp_csum_offload_pkts=7|tx_udp_csum_fallback_pkts=1|"
            b"tx_tso_v4=1|tx_tso_v6=0|tx_tso_max_size=65536\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_DIAG|INFO|host_features=0x1|guest_features=0x2|"
            "irq_mode=msix|irq_message_count=3|"
            "tx_csum_v4=1|tx_csum_v6=1|tx_udp_csum_v4=0|tx_udp_csum_v6=1|"
            "tx_tcp_csum_offload_pkts=10|tx_tcp_csum_fallback_pkts=2|"
            "tx_udp_csum_offload_pkts=7|tx_udp_csum_fallback_pkts=1|"
            "tx_tso_v4=1|tx_tso_v6=0|tx_tso_max_size=65536",
        )

    def test_emits_ctrl_vq_fields(self) -> None:
        tail = (
            b"virtio-net-diag|INFO|irq_mode=msix|irq_message_count=3|ctrl_vq=1|ctrl_rx=1|ctrl_vlan=0|"
            b"ctrl_mac_addr=1|ctrl_queue_index=2|ctrl_queue_size=64|ctrl_error_flags=0x00000000|"
            b"ctrl_cmd_sent=10|ctrl_cmd_ok=9|ctrl_cmd_err=1|ctrl_cmd_timeout=0\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_DIAG|INFO|irq_mode=msix|irq_message_count=3|"
            "ctrl_vq=1|ctrl_rx=1|ctrl_vlan=0|ctrl_mac_addr=1|ctrl_queue_index=2|ctrl_queue_size=64|"
            "ctrl_error_flags=0x00000000|ctrl_cmd_sent=10|ctrl_cmd_ok=9|ctrl_cmd_err=1|ctrl_cmd_timeout=0",
        )


if __name__ == "__main__":
    unittest.main()
