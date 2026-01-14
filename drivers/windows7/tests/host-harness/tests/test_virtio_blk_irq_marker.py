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


class VirtioBlkIrqMarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def _emit(self, tail: bytes) -> str:
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.harness._emit_virtio_blk_irq_host_marker(tail)
        return buf.getvalue().strip()

    def test_emits_pass_with_msix_fields(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|irq_mode=msix|msix_config_vector=0|"
            b"msix_queue_vector=1\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ|PASS|irq_mode=msix|msix_config_vector=0|msix_queue_vector=1",
        )

    def test_emits_pass_with_hex_vector_values(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|irq_mode=intx|msix_config_vector=0xffff|"
            b"msix_queue_vector=0xFFFF\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ|PASS|irq_mode=intx|msix_config_vector=0xffff|msix_queue_vector=0xFFFF",
        )

    def test_falls_back_to_standalone_irq_diag_lines(self) -> None:
        # Older guest selftests may not include `irq_mode`/MSI-X fields on the per-test marker,
        # but can still emit a standalone `virtio-blk-irq|...` diagnostic line.
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS\n"
            b"virtio-blk-irq|INFO|mode=msix|msix_config_vector=0x0000|msix_queue0_vector=0x0001\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ|PASS|irq_mode=msix|msix_config_vector=0x0000|msix_queue_vector=0x0001",
        )

    def test_emits_fail_token(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|FAIL|irq_mode=intx\n"
        out = self._emit(tail)
        self.assertEqual(out, "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ|FAIL|irq_mode=intx")

    def test_emits_info_when_only_irq_diag_marker_present(self) -> None:
        tail = b"virtio-blk-irq|INFO|mode=intx\n"
        out = self._emit(tail)
        self.assertEqual(out, "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ|INFO|irq_mode=intx")

    def test_emits_info_when_only_miniport_irq_diag_marker_present(self) -> None:
        # Newer guest selftests renamed miniport IOCTL-derived diagnostics from `virtio-blk-irq|...`
        # to `virtio-blk-miniport-irq|...` so `virtio-blk-irq|...` can be reserved for
        # cfgmgr32/Windows-assigned IRQ resource enumeration.
        tail = (
            b"virtio-blk-miniport-irq|INFO|mode=msix|message_count=2|msix_config_vector=0x0000|"
            b"msix_queue0_vector=0x0001\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ|INFO|irq_mode=msix|irq_message_count=2|"
            "msix_config_vector=0x0000|msix_queue_vector=0x0001",
        )

    def test_prefers_miniport_irq_diag_over_blk_irq_diag(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS\n"
            b"virtio-blk-irq|INFO|mode=intx|message_count=1|msix_config_vector=0xffff|msix_queue0_vector=0xffff\n"
            b"virtio-blk-miniport-irq|INFO|mode=msix|message_count=2|msix_config_vector=0x0000|msix_queue0_vector=0x0001\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ|PASS|irq_mode=msix|irq_message_count=2|"
            "msix_config_vector=0x0000|msix_queue_vector=0x0001",
        )

    def test_miniport_irq_diag_fills_missing_fields_but_does_not_override_per_test(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|irq_mode=msix|msix_config_vector=5|msix_queue_vector=6\n"
            b"virtio-blk-miniport-irq|INFO|mode=intx|message_count=2|msix_config_vector=0x0000|msix_queue0_vector=0x0001\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ|PASS|irq_mode=msix|irq_message_count=2|"
            "msix_config_vector=5|msix_queue_vector=6",
        )

    def test_uses_last_marker(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|irq_mode=intx\n"
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|irq_mode=msix|msix_config_vector=5|msix_queue_vector=6\n"
        )
        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ|PASS|irq_mode=msix|msix_config_vector=5|msix_queue_vector=6",
        )

    def test_no_output_when_no_irq_fields(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS\n"
        out = self._emit(tail)
        self.assertEqual(out, "")

        # Even if other key/value fields are present, do not emit the IRQ marker unless
        # interrupt-mode related fields are included.
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|sector_size=512\n"
        out = self._emit(tail)
        self.assertEqual(out, "")

    def test_emits_irq_marker_and_keeps_blk_pass_marker_parseable(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS\n"
            b"virtio-blk-irq|INFO|mode=msi|messages=2\n"
        )

        out = self._emit(tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ|PASS|irq_mode=msi|irq_message_count=2",
        )

        # Ensure the existing virtio-blk PASS marker can still be extracted deterministically.
        blk_marker = self.harness._try_extract_last_marker_line(tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|")
        self.assertEqual(blk_marker, "AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS")

    def test_uses_incremental_marker_overrides(self) -> None:
        # Simulate the harness tail buffer truncating earlier output: pass the per-test marker and
        # standalone IRQ diagnostics via the optional override parameters.
        tail = b""
        blk_test_line = (
            "AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS|irq_mode=msix|msix_config_vector=0x0005|"
            "msix_queue_vector=0x0006"
        )
        irq_diag_markers = {"virtio-blk": {"level": "INFO", "mode": "msi", "message_count": "2"}}

        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.harness._emit_virtio_blk_irq_host_marker(
                tail, blk_test_line=blk_test_line, irq_diag_markers=irq_diag_markers
            )
        out = buf.getvalue().strip()
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ|PASS|irq_mode=msix|irq_message_count=2|"
            "msix_config_vector=0x0005|msix_queue_vector=0x0006",
        )

    def test_uses_incremental_marker_overrides_with_miniport_diag(self) -> None:
        tail = b""
        blk_test_line = "AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS"
        irq_diag_markers = {
            "virtio-blk-miniport": {
                "level": "INFO",
                "mode": "msix",
                "message_count": "2",
                "msix_config_vector": "0x0000",
                "msix_queue0_vector": "0x0001",
            }
        }

        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.harness._emit_virtio_blk_irq_host_marker(
                tail, blk_test_line=blk_test_line, irq_diag_markers=irq_diag_markers
            )
        out = buf.getvalue().strip()
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ|PASS|irq_mode=msix|irq_message_count=2|"
            "msix_config_vector=0x0000|msix_queue_vector=0x0001",
        )


if __name__ == "__main__":
    unittest.main()
