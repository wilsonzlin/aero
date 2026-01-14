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


class VirtioSndMarkerTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def _emit(self, fn_name: str, tail: bytes) -> str:
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            getattr(self.harness, fn_name)(tail)
        return buf.getvalue().strip()

    def test_emits_playback_marker(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd|PASS|backend=wav|frames=48000\n"
        out = self._emit("_emit_virtio_snd_playback_host_marker", tail)
        self.assertEqual(out, "AERO_VIRTIO_WIN7_HOST|VIRTIO_SND|PASS|backend=wav|frames=48000")

    def test_emits_capture_marker(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-capture|FAIL|method=mic|frames=123|"
            b"non_silence=0|silence_only=1|reason=all_silence|extra=ignored\n"
        )
        out = self._emit("_emit_virtio_snd_capture_host_marker", tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_CAPTURE|FAIL|method=mic|frames=123|"
            "non_silence=0|silence_only=1|reason=all_silence",
        )

    def test_emits_duplex_marker(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-duplex|SKIP|frames=0|non_silence=0|"
            b"reason=endpoint_missing|hr=0x80070057\n"
        )
        out = self._emit("_emit_virtio_snd_duplex_host_marker", tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_DUPLEX|SKIP|frames=0|non_silence=0|"
            "reason=endpoint_missing|hr=0x80070057",
        )

    def test_emits_buffer_limits_marker(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-buffer-limits|PASS|mode=dequeue|"
            b"expected_failure=0|buffer_bytes=262144|init_hr=0x0|hr=0x0|reason=ok\n"
        )
        out = self._emit("_emit_virtio_snd_buffer_limits_host_marker", tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_BUFFER_LIMITS|PASS|mode=dequeue|expected_failure=0|"
            "buffer_bytes=262144|init_hr=0x0|hr=0x0|reason=ok",
        )

    def test_emits_format_marker(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-format|INFO|render=tag=0x0001 type=pcm rate=48000 ch=2 bits=16 valid=16 align=4|"
            b"capture=tag=0xfffe type=pcm rate=44100 ch=1 bits=32 valid=24 align=4\n"
        )
        out = self._emit("_emit_virtio_snd_format_host_marker", tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_FORMAT|INFO|render=tag=0x0001 type=pcm rate=48000 ch=2 bits=16 valid=16 align=4|"
            "capture=tag=0xfffe type=pcm rate=44100 ch=1 bits=32 valid=24 align=4",
        )

    def test_emits_eventq_marker_info(self) -> None:
        tail = (
            b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-eventq|INFO|completions=10|parsed=9|short=1|unknown=0|"
            b"jack_connected=0|jack_disconnected=0|pcm_period=8|xrun=0|ctl_notify=0\n"
        )
        out = self._emit("_emit_virtio_snd_eventq_host_marker", tail)
        self.assertEqual(
            out,
            "AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_EVENTQ|INFO|completions=10|parsed=9|short=1|unknown=0|"
            "jack_connected=0|jack_disconnected=0|pcm_period=8|xrun=0|ctl_notify=0",
        )

    def test_emits_eventq_marker_skip_reason(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-snd-eventq|SKIP|device_missing\n"
        out = self._emit("_emit_virtio_snd_eventq_host_marker", tail)
        self.assertEqual(out, "AERO_VIRTIO_WIN7_HOST|VIRTIO_SND_EVENTQ|SKIP|reason=device_missing")

    def test_no_output_when_missing(self) -> None:
        tail = b"AERO_VIRTIO_SELFTEST|TEST|virtio-net|PASS|large_ok=1|large_bytes=1\n"
        for fn in (
            "_emit_virtio_snd_playback_host_marker",
            "_emit_virtio_snd_capture_host_marker",
            "_emit_virtio_snd_format_host_marker",
            "_emit_virtio_snd_duplex_host_marker",
            "_emit_virtio_snd_buffer_limits_host_marker",
            "_emit_virtio_snd_eventq_host_marker",
        ):
            with self.subTest(fn=fn):
                out = self._emit(fn, tail)
                self.assertEqual(out, "")


if __name__ == "__main__":
    unittest.main()
