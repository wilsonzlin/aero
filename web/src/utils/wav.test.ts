import { describe, expect, it } from "vitest";

import { encodeWavPcm16 } from "./wav";

function readAscii(bytes: Uint8Array, off: number, len: number): string {
  return new TextDecoder().decode(bytes.subarray(off, off + len));
}

describe("encodeWavPcm16()", () => {
  it("writes a valid RIFF/WAVE header + PCM16 payload", () => {
    const sr = 48000;
    const cc = 2;
    // Two stereo frames: [L0, R0, L1, R1]
    const samples = new Float32Array([0, 1, -1, 0.5]);
    const wav = encodeWavPcm16(samples, sr, cc);

    expect(readAscii(wav, 0, 4)).toBe("RIFF");
    expect(readAscii(wav, 8, 4)).toBe("WAVE");
    expect(readAscii(wav, 12, 4)).toBe("fmt ");
    expect(readAscii(wav, 36, 4)).toBe("data");

    const view = new DataView(wav.buffer);
    expect(view.getUint16(20, true)).toBe(1); // PCM
    expect(view.getUint16(22, true)).toBe(cc);
    expect(view.getUint32(24, true)).toBe(sr);
    expect(view.getUint16(34, true)).toBe(16);

    const dataBytes = view.getUint32(40, true);
    expect(dataBytes).toBe(samples.length * 2);
    expect(wav.byteLength).toBe(44 + dataBytes);

    // Validate PCM conversions.
    // Frame 0: L=0, R=1
    expect(view.getInt16(44, true)).toBe(0);
    expect(view.getInt16(46, true)).toBe(32767);
    // Frame 1: L=-1, R=0.5
    expect(view.getInt16(48, true)).toBe(-32768);
    expect(view.getInt16(50, true)).toBe(16384);
  });

  it("rejects sample buffers that are not aligned to channelCount", () => {
    expect(() => encodeWavPcm16(new Float32Array([0, 1, 2]), 48000, 2)).toThrow(/not divisible/);
  });
});

