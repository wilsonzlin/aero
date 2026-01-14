import { describe, expect, it } from "vitest";

import {
  LINEAR_TO_SRGB_U8,
  SRGB_TO_LINEAR_U8,
  encodeLinearRgba8ToSrgbInPlace,
  linearizeSrgbRgba8InPlace,
} from "./srgb";

describe("sRGB lookup tables", () => {
  it("maps endpoints and is monotonic", () => {
    expect(SRGB_TO_LINEAR_U8[0]).toBe(0);
    expect(SRGB_TO_LINEAR_U8[255]).toBe(255);
    expect(LINEAR_TO_SRGB_U8[0]).toBe(0);
    expect(LINEAR_TO_SRGB_U8[255]).toBe(255);

    for (let i = 0; i < 255; i += 1) {
      expect(SRGB_TO_LINEAR_U8[i]).toBeLessThanOrEqual(SRGB_TO_LINEAR_U8[i + 1]);
      expect(LINEAR_TO_SRGB_U8[i]).toBeLessThanOrEqual(LINEAR_TO_SRGB_U8[i + 1]);
    }
  });

  it("round-trips linear -> sRGB -> linear within 1", () => {
    for (let i = 0; i < 256; i += 1) {
      const s = LINEAR_TO_SRGB_U8[i]!;
      const rt = SRGB_TO_LINEAR_U8[s]!;
      expect(Math.abs(rt - i)).toBeLessThanOrEqual(1);
    }
  });
});

describe("sRGB encode/decode helpers", () => {
  it("preserves alpha and can round-trip a mid-gray sample", () => {
    const rgba = new Uint8Array([
      // Pixel 0: mid-gray, alpha=7
      128, 128, 128, 7,
      // Pixel 1: low-ish values, alpha=0x80
      16, 32, 64, 0x80,
    ]);

    encodeLinearRgba8ToSrgbInPlace(rgba);
    expect(rgba[3]).toBe(7);
    expect(rgba[7]).toBe(0x80);
    // Known value: linear 128 encodes to sRGB 188 (with our LUT + rounding).
    expect(Array.from(rgba.subarray(0, 4))).toEqual([188, 188, 188, 7]);

    linearizeSrgbRgba8InPlace(rgba);
    expect(rgba[3]).toBe(7);
    expect(rgba[7]).toBe(0x80);
    expect(Array.from(rgba.subarray(0, 4))).toEqual([128, 128, 128, 7]);
  });
});

