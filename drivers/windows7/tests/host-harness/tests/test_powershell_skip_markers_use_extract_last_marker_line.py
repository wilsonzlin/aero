#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import re
import unittest
from pathlib import Path


class PowerShellSkipMarkersUseExtractLastMarkerLineTests(unittest.TestCase):
    def setUp(self) -> None:
        self.ps_path = Path(__file__).resolve().parents[1] / "Invoke-AeroVirtioWin7Tests.ps1"
        self.text = self.ps_path.read_text(encoding="utf-8", errors="replace")

    def _extract_case_body(self, case_name: str, next_sentinel: str) -> str:
        m = re.search(
            rf'"{re.escape(case_name)}"\s*\{{(?P<body>[\s\S]*?)\r?\n\s*\}}\r?\n\s*{next_sentinel}',
            self.text,
        )
        self.assertIsNotNone(m, f"failed to locate PowerShell case {case_name}")
        assert m is not None
        return m.group("body")

    def test_virtio_input_led_skipped_uses_try_extract_last_aero_marker_line(self) -> None:
        body = self._extract_case_body("VIRTIO_INPUT_LED_SKIPPED", r'"VIRTIO_INPUT_LED_FAILED"\s*\{')
        self.assertIn("Try-ExtractLastAeroMarkerLine", body)
        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-led|SKIP|"', body)

    def test_virtio_input_leds_skipped_uses_try_extract_last_aero_marker_line(self) -> None:
        body = self._extract_case_body("VIRTIO_INPUT_LEDS_SKIPPED", r'"VIRTIO_INPUT_LEDS_FAILED"\s*\{')
        self.assertIn("Try-ExtractLastAeroMarkerLine", body)
        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-leds|SKIP|"', body)
        self.assertIn("\\|SKIP\\|([^|\\r\\n=]+)(?:\\||$)", body)

    def test_virtio_blk_resize_skipped_uses_try_extract_last_aero_marker_line(self) -> None:
        body = self._extract_case_body("VIRTIO_BLK_RESIZE_SKIPPED", r'"VIRTIO_BLK_RESIZE_FAILED"\s*\{')
        self.assertIn("Try-ExtractLastAeroMarkerLine", body)
        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-blk-resize|SKIP|"', body)
        self.assertIn("\\|SKIP\\|([^|\\r\\n=]+)(?:\\||$)", body)

    def test_virtio_input_binding_skipped_uses_try_extract_last_aero_marker_line(self) -> None:
        body = self._extract_case_body("VIRTIO_INPUT_BINDING_SKIPPED", r'"VIRTIO_INPUT_BINDING_FAILED"\s*\{')
        self.assertIn("Try-ExtractLastAeroMarkerLine", body)
        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-binding|SKIP"', body)
        self.assertIn("\\|SKIP\\|([^|\\r\\n=]+)(?:\\||$)", body)

    def test_virtio_net_link_flap_skipped_uses_try_extract_last_aero_marker_line(self) -> None:
        body = self._extract_case_body("VIRTIO_NET_LINK_FLAP_SKIPPED", r'"VIRTIO_NET_UDP_FAILED"\s*\{')
        self.assertIn("Try-ExtractLastAeroMarkerLine", body)
        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-net-link-flap|SKIP|"', body)

    def test_virtio_input_events_skipped_uses_try_extract_last_aero_marker_line(self) -> None:
        body = self._extract_case_body(
            "VIRTIO_INPUT_EVENTS_SKIPPED",
            r'"VIRTIO_INPUT_EVENTS_EXTENDED_SKIPPED"\s*\{',
        )
        self.assertIn("Try-ExtractLastAeroMarkerLine", body)
        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events|SKIP|"', body)
        self.assertIn("\\|SKIP\\|([^|\\r\\n=]+)(?:\\||$)", body)

    def test_virtio_input_media_keys_skipped_uses_try_extract_last_aero_marker_line(self) -> None:
        body = self._extract_case_body("VIRTIO_INPUT_MEDIA_KEYS_SKIPPED", r'"VIRTIO_INPUT_MEDIA_KEYS_FAILED"\s*\{')
        self.assertIn("Try-ExtractLastAeroMarkerLine", body)
        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-media-keys|SKIP|"', body)
        self.assertIn("\\|SKIP\\|([^|\\r\\n=]+)(?:\\||$)", body)

    def test_virtio_net_udp_skipped_uses_try_extract_last_aero_marker_line(self) -> None:
        body = self._extract_case_body("VIRTIO_NET_UDP_SKIPPED", r'"QMP_NET_LINK_FLAP_FAILED"\s*\{')
        self.assertIn("Try-ExtractLastAeroMarkerLine", body)
        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-net-udp|SKIP|"', body)
        self.assertIn("\\|SKIP\\|([^|\\r\\n=]+)(?:\\||$)", body)

    def test_virtio_snd_skipped_uses_try_extract_last_aero_marker_line(self) -> None:
        body = self._extract_case_body("VIRTIO_SND_SKIPPED", r'"VIRTIO_SND_CAPTURE_SKIPPED"\s*\{')
        self.assertIn("Try-ExtractLastAeroMarkerLine", body)
        self.assertIn("Try-ExtractVirtioSndSkipReason", body)
        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-snd|SKIP"', body)
        for pat in (
            "irq_mode=([^|\\r\\n]+)",
            "irq_message_count=([^|\\r\\n]+)",
        ):
            self.assertIn(pat, body)

    def test_try_extract_virtio_snd_skip_reason_scans_serial_log_path(self) -> None:
        idx = self.text.find("function Try-ExtractVirtioSndSkipReason")
        self.assertNotEqual(idx, -1, "missing Try-ExtractVirtioSndSkipReason helper")
        window = self.text[idx : idx + 2500]
        self.assertIn("SerialLogPath", window)
        self.assertIn("StreamReader", window)
        self.assertIn("virtio-snd: skipped \\(enable with --test-snd\\)", window)

    def test_virtio_snd_capture_skipped_uses_try_extract_last_aero_marker_line(self) -> None:
        body = self._extract_case_body("VIRTIO_SND_CAPTURE_SKIPPED", r'"VIRTIO_SND_DUPLEX_SKIPPED"\s*\{')
        self.assertIn("Try-ExtractLastAeroMarkerLine", body)
        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|SKIP|"', body)

    def test_virtio_snd_duplex_skipped_uses_try_extract_last_aero_marker_line(self) -> None:
        body = self._extract_case_body("VIRTIO_SND_DUPLEX_SKIPPED", r'"VIRTIO_SND_BUFFER_LIMITS_SKIPPED"\s*\{')
        self.assertIn("Try-ExtractLastAeroMarkerLine", body)
        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|"', body)

    def test_virtio_snd_buffer_limits_skipped_uses_try_extract_last_aero_marker_line(self) -> None:
        body = self._extract_case_body("VIRTIO_SND_BUFFER_LIMITS_SKIPPED", r"default\s*\{")
        self.assertIn("Try-ExtractLastAeroMarkerLine", body)
        self.assertIn('-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|SKIP|"', body)

    def test_virtio_input_events_extended_skipped_uses_try_extract_last_aero_marker_line(self) -> None:
        body = self._extract_case_body(
            "VIRTIO_INPUT_EVENTS_EXTENDED_SKIPPED",
            r'"VIRTIO_INPUT_EVENTS_FAILED"\s*\{',
        )
        self.assertIn("Try-ExtractLastAeroMarkerLine", body)
        self.assertIn(
            '-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-modifiers|SKIP|"',
            body,
        )
        self.assertIn(
            '-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-buttons|SKIP|"',
            body,
        )
        self.assertIn(
            '-Prefix "AERO_VIRTIO_SELFTEST|TEST|virtio-input-events-wheel|SKIP|"',
            body,
        )


if __name__ == "__main__":
    unittest.main()
