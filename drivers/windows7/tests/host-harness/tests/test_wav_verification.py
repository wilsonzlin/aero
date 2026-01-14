#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0

from __future__ import annotations

import contextlib
import importlib.util
import io
import math
import struct
import sys
import tempfile
import unittest
import wave
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


def _write_pcm_wav(
    path: Path,
    *,
    sample_rate: int,
    channels: int,
    sampwidth_bytes: int,
    frames: int,
    kind: str,
    freq_hz: float = 440.0,
    amp: float = 0.20,
) -> None:
    assert kind in ("silence", "tone")
    assert sampwidth_bytes in (1, 2, 3, 4)

    def sample_value(i: int) -> float:
        if kind == "silence":
            return 0.0
        return math.sin(2.0 * math.pi * freq_hz * (i / sample_rate)) * amp

    buf = bytearray()
    for i in range(frames):
        s = sample_value(i)
        if sampwidth_bytes == 1:
            # WAV 8-bit PCM is unsigned [0,255], silence is 0x80.
            v = int(round(s * 127.0))
            u = max(0, min(255, v + 128))
            frame = bytes([u]) * channels
        elif sampwidth_bytes == 2:
            v = int(round(s * 32767.0))
            frame = v.to_bytes(2, "little", signed=True) * channels
        elif sampwidth_bytes == 3:
            v = int(round(s * 8388607.0))
            frame = v.to_bytes(3, "little", signed=True) * channels
        else:
            v = int(round(s * 2147483647.0))
            frame = v.to_bytes(4, "little", signed=True) * channels

        buf += frame

    with wave.open(str(path), "wb") as w:
        w.setnchannels(channels)
        w.setsampwidth(sampwidth_bytes)
        w.setframerate(sample_rate)
        w.writeframes(bytes(buf))


def _write_ieee_float32_wav(
    path: Path,
    *,
    sample_rate: int,
    channels: int,
    frames: int,
    kind: str,
    freq_hz: float = 440.0,
    amp: float = 0.20,
    data_chunk_size_override: int | None = None,
) -> None:
    assert kind in ("silence", "tone")

    def sample_value(i: int) -> float:
        if kind == "silence":
            return 0.0
        return math.sin(2.0 * math.pi * freq_hz * (i / sample_rate)) * amp

    data = bytearray()
    for i in range(frames):
        s = sample_value(i)
        for _ in range(channels):
            data += struct.pack("<f", float(s))

    data_size = len(data)
    data_chunk_size = data_chunk_size_override if data_chunk_size_override is not None else data_size

    fmt = struct.pack(
        "<HHIIHH",
        3,  # WAVE_FORMAT_IEEE_FLOAT
        channels,
        sample_rate,
        sample_rate * channels * 4,
        channels * 4,
        32,
    )
    fmt_chunk = b"fmt " + struct.pack("<I", len(fmt)) + fmt
    data_chunk = b"data" + struct.pack("<I", data_chunk_size) + data

    riff_size = 4 + len(fmt_chunk) + len(data_chunk)
    header = b"RIFF" + struct.pack("<I", riff_size) + b"WAVE"
    path.write_bytes(header + fmt_chunk + data_chunk)


def _write_extensible_wav(
    path: Path,
    *,
    sample_rate: int,
    channels: int,
    frames: int,
    bits_per_sample: int,
    subformat_guid_le: bytes,
    kind: str,
    freq_hz: float = 440.0,
    amp: float = 0.20,
    data_chunk_size_override: int | None = None,
) -> None:
    """
    Write a minimal WAVE_FORMAT_EXTENSIBLE file (fmt chunk size=40).

    `subformat_guid_le` must be the 16-byte GUID in little-endian struct layout as used by
    WAVEFORMATEXTENSIBLE (and as expected by the host harness verifier).
    """
    assert kind in ("silence", "tone")
    assert bits_per_sample in (16, 32, 64)
    assert len(subformat_guid_le) == 16

    def sample_value(i: int) -> float:
        if kind == "silence":
            return 0.0
        return math.sin(2.0 * math.pi * freq_hz * (i / sample_rate)) * amp

    if bits_per_sample == 16:
        sample_bytes = 2
        max_val = 32767.0
    elif bits_per_sample == 32:
        sample_bytes = 4
        max_val = 1.0  # float32 uses [-1.0, 1.0]
    else:
        sample_bytes = 8
        max_val = 1.0  # float64 uses [-1.0, 1.0]

    block_align = channels * sample_bytes
    avg_bytes_per_sec = sample_rate * block_align

    data = bytearray()
    for i in range(frames):
        s = sample_value(i)
        for _ in range(channels):
            if bits_per_sample == 16:
                v = int(round(s * max_val))
                data += v.to_bytes(2, "little", signed=True)
            elif bits_per_sample == 32:
                data += struct.pack("<f", float(s))
            else:
                data += struct.pack("<d", float(s))

    data_size = len(data)
    data_chunk_size = data_chunk_size_override if data_chunk_size_override is not None else data_size

    fmt_head = struct.pack(
        "<HHIIHH",
        0xFFFE,  # WAVE_FORMAT_EXTENSIBLE
        channels,
        sample_rate,
        avg_bytes_per_sec,
        block_align,
        bits_per_sample,
    )
    # cbSize (22) + wValidBitsPerSample + dwChannelMask + SubFormat
    ext = struct.pack("<HHI", 22, bits_per_sample, 0) + subformat_guid_le
    assert len(ext) == 24

    fmt_chunk = b"fmt " + struct.pack("<I", len(fmt_head) + len(ext)) + fmt_head + ext
    data_chunk = b"data" + struct.pack("<I", data_chunk_size) + data

    riff_size = 4 + len(fmt_chunk) + len(data_chunk)
    header = b"RIFF" + struct.pack("<I", riff_size) + b"WAVE"
    path.write_bytes(header + fmt_chunk + data_chunk)


class WavVerificationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.harness = _load_harness()

    def _verify(self, wav_path: Path) -> tuple[bool, str]:
        buf = io.StringIO()
        with contextlib.redirect_stdout(buf):
            ok = self.harness._verify_virtio_snd_wav_non_silent(
                wav_path, peak_threshold=200, rms_threshold=50
            )
        return bool(ok), buf.getvalue()

    def test_pcm_16bit_silence_and_tone(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            td_path = Path(td)
            silence = td_path / "silence16.wav"
            tone = td_path / "tone16.wav"
            _write_pcm_wav(
                silence, sample_rate=8000, channels=2, sampwidth_bytes=2, frames=8000, kind="silence"
            )
            _write_pcm_wav(tone, sample_rate=8000, channels=2, sampwidth_bytes=2, frames=8000, kind="tone")

            ok, out = self._verify(silence)
            self.assertFalse(ok)
            self.assertIn("reason=silent_pcm", out)

            ok, out = self._verify(tone)
            self.assertTrue(ok)
            self.assertIn("|PASS|", out)

    def test_pcm_8bit_unsigned_silence_and_tone(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            td_path = Path(td)
            silence = td_path / "silence8.wav"
            tone = td_path / "tone8.wav"
            _write_pcm_wav(silence, sample_rate=8000, channels=1, sampwidth_bytes=1, frames=8000, kind="silence")
            _write_pcm_wav(tone, sample_rate=8000, channels=1, sampwidth_bytes=1, frames=8000, kind="tone")

            ok, out = self._verify(silence)
            self.assertFalse(ok)
            self.assertIn("reason=silent_pcm", out)

            ok, out = self._verify(tone)
            self.assertTrue(ok)
            self.assertIn("|PASS|", out)

    def test_pcm_24bit_and_32bit(self) -> None:
        with tempfile.TemporaryDirectory() as td:
            td_path = Path(td)
            for sampwidth in (3, 4):
                silence = td_path / f"silence{8 * sampwidth}.wav"
                tone = td_path / f"tone{8 * sampwidth}.wav"
                _write_pcm_wav(
                    silence,
                    sample_rate=8000,
                    channels=2,
                    sampwidth_bytes=sampwidth,
                    frames=8000,
                    kind="silence",
                )
                _write_pcm_wav(
                    tone,
                    sample_rate=8000,
                    channels=2,
                    sampwidth_bytes=sampwidth,
                    frames=8000,
                    kind="tone",
                )

                ok, out = self._verify(silence)
                self.assertFalse(ok)
                self.assertIn("reason=silent_pcm", out)

                ok, out = self._verify(tone)
                self.assertTrue(ok)
                self.assertIn("|PASS|", out)

    def test_ieee_float32_and_data_chunk_size_zero(self) -> None:
        # The harness supports float32 wav captures, and also tries to recover from the QEMU wav backend
        # leaving the data chunk size as a placeholder 0 when QEMU is killed hard.
        with tempfile.TemporaryDirectory() as td:
            td_path = Path(td)

            silence = td_path / "silence_float.wav"
            tone_size0 = td_path / "tone_float_size0.wav"
            _write_ieee_float32_wav(silence, sample_rate=8000, channels=1, frames=8000, kind="silence")
            _write_ieee_float32_wav(
                tone_size0,
                sample_rate=8000,
                channels=1,
                frames=8000,
                kind="tone",
                data_chunk_size_override=0,
            )

            ok, out = self._verify(silence)
            self.assertFalse(ok)
            self.assertIn("reason=silent_pcm", out)

            ok, out = self._verify(tone_size0)
            self.assertTrue(ok)
            self.assertIn("|PASS|", out)

    def test_extensible_pcm_and_float(self) -> None:
        k_subformat_pcm = bytes.fromhex("0100000000001000800000aa00389b71")
        k_subformat_float = bytes.fromhex("0300000000001000800000aa00389b71")

        with tempfile.TemporaryDirectory() as td:
            td_path = Path(td)

            # PCM16 in extensible container.
            silence_pcm = td_path / "silence_ext_pcm16.wav"
            tone_pcm = td_path / "tone_ext_pcm16.wav"
            _write_extensible_wav(
                silence_pcm,
                sample_rate=8000,
                channels=2,
                frames=8000,
                bits_per_sample=16,
                subformat_guid_le=k_subformat_pcm,
                kind="silence",
            )
            _write_extensible_wav(
                tone_pcm,
                sample_rate=8000,
                channels=2,
                frames=8000,
                bits_per_sample=16,
                subformat_guid_le=k_subformat_pcm,
                kind="tone",
            )

            ok, out = self._verify(silence_pcm)
            self.assertFalse(ok)
            self.assertIn("reason=silent_pcm", out)

            ok, out = self._verify(tone_pcm)
            self.assertTrue(ok)
            self.assertIn("|PASS|", out)

            # Float32 in extensible container; also exercise data chunk size=0 recovery.
            silence_f = td_path / "silence_ext_f32.wav"
            tone_f_size0 = td_path / "tone_ext_f32_size0.wav"
            _write_extensible_wav(
                silence_f,
                sample_rate=8000,
                channels=1,
                frames=8000,
                bits_per_sample=32,
                subformat_guid_le=k_subformat_float,
                kind="silence",
            )
            _write_extensible_wav(
                tone_f_size0,
                sample_rate=8000,
                channels=1,
                frames=8000,
                bits_per_sample=32,
                subformat_guid_le=k_subformat_float,
                kind="tone",
                data_chunk_size_override=0,
            )

            ok, out = self._verify(silence_f)
            self.assertFalse(ok)
            self.assertIn("reason=silent_pcm", out)

            ok, out = self._verify(tone_f_size0)
            self.assertTrue(ok)
            self.assertIn("|PASS|", out)

            # Float64 in extensible container.
            silence_f64 = td_path / "silence_ext_f64.wav"
            tone_f64 = td_path / "tone_ext_f64.wav"
            _write_extensible_wav(
                silence_f64,
                sample_rate=8000,
                channels=1,
                frames=8000,
                bits_per_sample=64,
                subformat_guid_le=k_subformat_float,
                kind="silence",
            )
            _write_extensible_wav(
                tone_f64,
                sample_rate=8000,
                channels=1,
                frames=8000,
                bits_per_sample=64,
                subformat_guid_le=k_subformat_float,
                kind="tone",
            )

            ok, out = self._verify(silence_f64)
            self.assertFalse(ok)
            self.assertIn("reason=silent_pcm", out)

            ok, out = self._verify(tone_f64)
            self.assertTrue(ok)
            self.assertIn("|PASS|", out)


if __name__ == "__main__":
    unittest.main()
