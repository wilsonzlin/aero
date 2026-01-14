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


class VirtioIrqMarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def test_parses_per_device_irq_markers(self) -> None:
        tail = (
            b"boot...\n"
            b"virtio-net-irq|INFO|mode=msix|vectors=4|msix_enabled=1\n"
            b"virtio-blk-irq|WARN|mode=intx|reason=msi_disabled\n"
            b"AERO_VIRTIO_SELFTEST|RESULT|PASS\n"
        )

        out = self.harness._parse_virtio_irq_markers(tail)
        self.assertEqual(out["virtio-net"]["level"], "INFO")
        self.assertEqual(out["virtio-net"]["mode"], "msix")
        self.assertEqual(out["virtio-net"]["vectors"], "4")
        self.assertEqual(out["virtio-net"]["msix_enabled"], "1")

        self.assertEqual(out["virtio-blk"]["level"], "WARN")
        self.assertEqual(out["virtio-blk"]["mode"], "intx")
        self.assertEqual(out["virtio-blk"]["reason"], "msi_disabled")

    def test_parses_blk_miniport_irq_markers(self) -> None:
        tail = (
            b"virtio-blk-miniport-irq|INFO|mode=msi|message_count=2|msix_config_vector=0x0000|"
            b"msix_queue0_vector=0x0001\n"
        )
        out = self.harness._parse_virtio_irq_markers(tail)
        self.assertEqual(out["virtio-blk-miniport"]["level"], "INFO")
        self.assertEqual(out["virtio-blk-miniport"]["mode"], "msi")
        self.assertEqual(out["virtio-blk-miniport"]["messages"], "2")
        self.assertEqual(out["virtio-blk-miniport"]["msix_config_vector"], "0x0000")
        self.assertEqual(out["virtio-blk-miniport"]["msix_queue0_vector"], "0x0001")

    def test_parses_with_leading_whitespace(self) -> None:
        tail = b"  virtio-net-irq|INFO|mode=msix|vectors=4\n"
        out = self.harness._parse_virtio_irq_markers(tail)
        self.assertEqual(out["virtio-net"]["mode"], "msix")
        self.assertEqual(out["virtio-net"]["vectors"], "4")

    def test_parses_crlf_and_cr_newlines(self) -> None:
        tail = (
            b"virtio-net-irq|INFO|mode=msix|vectors=4\r\n"
            b"virtio-blk-irq|WARN|mode=intx|reason=msi_disabled\r"
        )
        out = self.harness._parse_virtio_irq_markers(tail)
        self.assertEqual(out["virtio-net"]["mode"], "msix")
        self.assertEqual(out["virtio-blk"]["mode"], "intx")

    def test_uses_last_marker_per_device(self) -> None:
        tail = (
            b"virtio-net-irq|INFO|mode=msi|vectors=1\n"
            b"virtio-net-irq|INFO|mode=msix|vectors=8\n"
        )
        out = self.harness._parse_virtio_irq_markers(tail)
        self.assertEqual(out["virtio-net"]["mode"], "msix")
        self.assertEqual(out["virtio-net"]["vectors"], "8")

    def test_emits_host_markers(self) -> None:
        tail = (
            b"virtio-net-irq|INFO|mode=msix|vectors=4|msix_enabled=1\n"
            b"virtio-blk-irq|WARN|mode=intx|reason=msi_disabled\n"
        )
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.harness._emit_virtio_irq_host_markers(tail)
        lines = [line for line in buf.getvalue().splitlines() if line.strip()]
        self.assertEqual(
            lines,
            [
                "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ_DIAG|WARN|mode=intx|reason=msi_disabled",
                "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_IRQ_DIAG|INFO|mode=msix|msix_enabled=1|vectors=4",
            ],
        )

    def test_emits_host_markers_for_blk_miniport_prefix(self) -> None:
        tail = (
            b"virtio-blk-miniport-irq|INFO|mode=msi|message_count=2|msix_config_vector=0x0000|"
            b"msix_queue0_vector=0x0001\n"
        )
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.harness._emit_virtio_irq_host_markers(tail)
        self.assertEqual(
            buf.getvalue().strip(),
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_MINIPORT_IRQ_DIAG|INFO|"
            "messages=2|mode=msi|msix_config_vector=0x0000|msix_queue0_vector=0x0001",
        )

    def test_emits_host_markers_from_parsed_dict(self) -> None:
        markers = {
            "virtio-net": {"level": "INFO", "mode": "msix", "vectors": "4"},
            "virtio-blk": {"level": "WARN", "mode": "intx"},
        }
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.harness._emit_virtio_irq_host_markers(b"", markers=markers)
        lines = [line for line in buf.getvalue().splitlines() if line.strip()]
        self.assertEqual(
            lines,
            [
                "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ_DIAG|WARN|mode=intx",
                "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_IRQ_DIAG|INFO|mode=msix|vectors=4",
            ],
        )

    def test_incremental_parser_handles_split_lines(self) -> None:
        markers: dict[str, dict[str, str]] = {}
        carry = b""
        carry = self.harness._update_virtio_irq_markers_from_chunk(
            markers, b"virtio-net-irq|INFO|mode=ms", carry=carry
        )
        self.assertEqual(carry, b"virtio-net-irq|INFO|mode=ms")
        self.assertEqual(markers, {})

        carry = self.harness._update_virtio_irq_markers_from_chunk(
            markers, b"ix|vectors=4\n", carry=carry
        )
        self.assertEqual(carry, b"")
        self.assertEqual(markers["virtio-net"]["mode"], "msix")
        self.assertEqual(markers["virtio-net"]["vectors"], "4")

    def test_incremental_parser_handles_split_lines_for_blk_miniport(self) -> None:
        markers: dict[str, dict[str, str]] = {}
        carry = b""
        carry = self.harness._update_virtio_irq_markers_from_chunk(
            markers,
            b"virtio-blk-miniport-irq|INFO|mode=msi|message_count=2|msix_config_vector=0x0000|msix_queue0_",
            carry=carry,
        )
        self.assertTrue(carry.startswith(b"virtio-blk-miniport-irq|INFO|mode=msi"))
        self.assertEqual(markers, {})

        carry = self.harness._update_virtio_irq_markers_from_chunk(
            markers,
            b"vector=0x0001\n",
            carry=carry,
        )
        self.assertEqual(carry, b"")
        self.assertEqual(markers["virtio-blk-miniport"]["mode"], "msi")
        self.assertEqual(markers["virtio-blk-miniport"]["messages"], "2")
        self.assertEqual(markers["virtio-blk-miniport"]["msix_config_vector"], "0x0000")
        self.assertEqual(markers["virtio-blk-miniport"]["msix_queue0_vector"], "0x0001")

    def test_incremental_parser_handles_crlf(self) -> None:
        markers: dict[str, dict[str, str]] = {}
        carry = b""
        carry = self.harness._update_virtio_irq_markers_from_chunk(
            markers, b"virtio-net-irq|INFO|mode=msix|vectors=4\r\n", carry=carry
        )
        self.assertEqual(carry, b"")
        self.assertEqual(markers["virtio-net"]["mode"], "msix")

    def test_incremental_parser_handles_cr_only(self) -> None:
        markers: dict[str, dict[str, str]] = {}
        carry = b""
        carry = self.harness._update_virtio_irq_markers_from_chunk(
            markers, b"virtio-net-irq|INFO|mode=msix|vectors=4\r", carry=carry
        )
        self.assertEqual(carry, b"")
        self.assertEqual(markers["virtio-net"]["mode"], "msix")

    def test_incremental_parser_allows_leading_whitespace(self) -> None:
        markers: dict[str, dict[str, str]] = {}
        carry = b""
        carry = self.harness._update_virtio_irq_markers_from_chunk(
            markers, b"  virtio-net-irq|INFO|mode=msix|vectors=4\n", carry=carry
        )
        self.assertEqual(carry, b"")
        self.assertEqual(markers["virtio-net"]["mode"], "msix")

    def test_incremental_parser_bounds_carry(self) -> None:
        markers: dict[str, dict[str, str]] = {}
        # 10 bytes of 'x' followed by a 131072-byte run of 'y', no newline terminator.
        chunk = b"x" * 10 + b"y" * 131072
        carry = self.harness._update_virtio_irq_markers_from_chunk(markers, chunk, carry=b"")
        self.assertEqual(len(carry), 131072)
        self.assertEqual(carry, b"y" * 131072)

    def test_emits_msg_field_for_non_kv_tokens(self) -> None:
        tail = b"virtio-net-irq|WARN|msix disabled by policy\n"
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.harness._emit_virtio_irq_host_markers(tail)
        self.assertEqual(
            buf.getvalue().strip(),
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_NET_IRQ_DIAG|WARN|msg=msix disabled by policy",
        )

    def test_no_output_when_no_irq_markers(self) -> None:
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            self.harness._emit_virtio_irq_host_markers(b"AERO_VIRTIO_SELFTEST|RESULT|PASS\n")
        self.assertEqual(buf.getvalue().strip(), "")

    def test_extracts_blk_irq_marker_and_blk_pass_marker(self) -> None:
        # Ensure the virtio-blk PASS marker can still be extracted deterministically even when
        # standalone IRQ diagnostics are present, and normalize `message_count` into `messages`.
        for key in ("messages", "message_count"):
            with self.subTest(key=key):
                tail = (
                    b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS\n"
                    + f"virtio-blk-irq|INFO|mode=msi|{key}=2|msix_config_vector=0x0000|msix_queue0_vector=0x0001\n".encode(
                        "utf-8"
                    )
                )

                blk_marker = self.harness._try_extract_last_marker_line(
                    tail, b"AERO_VIRTIO_SELFTEST|TEST|virtio-blk|"
                )
                self.assertEqual(blk_marker, "AERO_VIRTIO_SELFTEST|TEST|virtio-blk|PASS")

                parsed = self.harness._parse_virtio_irq_markers(tail)
                self.assertEqual(parsed["virtio-blk"]["level"], "INFO")
                self.assertEqual(parsed["virtio-blk"]["mode"], "msi")
                self.assertEqual(parsed["virtio-blk"]["messages"], "2")
                self.assertNotIn("message_count", parsed["virtio-blk"])
                self.assertEqual(parsed["virtio-blk"]["msix_config_vector"], "0x0000")
                self.assertEqual(parsed["virtio-blk"]["msix_queue0_vector"], "0x0001")

                buf = io.StringIO()
                with contextlib.redirect_stdout(buf):
                    self.harness._emit_virtio_irq_host_markers(tail)
                self.assertEqual(
                    buf.getvalue().strip(),
                    "AERO_VIRTIO_WIN7_HOST|VIRTIO_BLK_IRQ_DIAG|INFO|"
                    "messages=2|mode=msi|msix_config_vector=0x0000|msix_queue0_vector=0x0001",
                )


if __name__ == "__main__":
    unittest.main()
