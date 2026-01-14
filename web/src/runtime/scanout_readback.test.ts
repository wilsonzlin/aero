import { describe, expect, it } from "vitest";

import {
  SCANOUT_FORMAT_B5G5R5A1,
  SCANOUT_FORMAT_B5G6R5,
  SCANOUT_FORMAT_B8G8R8A8,
  SCANOUT_FORMAT_B8G8R8X8,
  SCANOUT_FORMAT_R8G8B8X8_SRGB,
} from "../ipc/scanout_state";
import { readScanoutRgba8FromGuestRam, tryComputeScanoutRgba8ByteLength } from "./scanout_readback";

describe("runtime/scanout_readback", () => {
  it("converts tight pitch (pitch == width*4) BGRX->RGBA and forces alpha=255", () => {
    const width = 2;
    const height = 2;
    const pitchBytes = width * 4;

    const guest = new Uint8Array([
      // row0
      // pixel0: B=1,G=2,R=3,X=0
      1, 2, 3, 0,
      // pixel1: B=4,G=5,R=6,X=0
      4, 5, 6, 0,
      // row1
      // pixel2: B=7,G=8,R=9,X=0
      7, 8, 9, 0,
      // pixel3: B=10,G=11,R=12,X=0
      10, 11, 12, 0,
    ]);

    const out = readScanoutRgba8FromGuestRam(guest, {
      basePaddr: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });

    expect(out.width).toBe(width);
    expect(out.height).toBe(height);
    expect(Array.from(out.rgba8)).toEqual([
      // row0
      3, 2, 1, 255, 6, 5, 4, 255,
      // row1
      9, 8, 7, 255, 12, 11, 10, 255,
    ]);
  });

  it("converts padded pitch (pitch > width*4), skipping padding bytes", () => {
    const width = 2;
    const height = 2;
    const rowBytes = width * 4;
    const pitchBytes = rowBytes + 4; // 4 bytes padding at end of each row

    const guest = new Uint8Array(pitchBytes * height);
    guest.fill(0xee);

    // Row0 pixels.
    guest.set([1, 2, 3, 0, 4, 5, 6, 0], 0);
    // Row1 pixels (after pitchBytes).
    guest.set([7, 8, 9, 0, 10, 11, 12, 0], pitchBytes);

    const out = readScanoutRgba8FromGuestRam(guest, {
      basePaddr: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });

    expect(out.rgba8.byteLength).toBe(rowBytes * height);
    expect(Array.from(out.rgba8)).toEqual([
      // row0
      3, 2, 1, 255, 6, 5, 4, 255,
      // row1
      9, 8, 7, 255, 12, 11, 10, 255,
    ]);
  });

  it("converts byte-granular pitch (pitchBytes not aligned to bytes-per-pixel)", () => {
    const width = 2;
    const height = 2;
    const rowBytes = width * 4;
    const pitchBytes = rowBytes + 1; // 1 byte padding at end of each row (not divisible by 4)

    const guest = new Uint8Array(pitchBytes * height);
    guest.fill(0xee);

    // Row0 pixels.
    guest.set([1, 2, 3, 0, 4, 5, 6, 0], 0);
    // Row1 pixels (after pitchBytes).
    guest.set([7, 8, 9, 0, 10, 11, 12, 0], pitchBytes);

    const out = readScanoutRgba8FromGuestRam(guest, {
      basePaddr: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });

    expect(out.rgba8.byteLength).toBe(rowBytes * height);
    expect(Array.from(out.rgba8)).toEqual([
      // row0
      3, 2, 1, 255, 6, 5, 4, 255,
      // row1
      9, 8, 7, 255, 12, 11, 10, 255,
    ]);
  });

  it("converts padded pitch without requiring last-row padding bytes", () => {
    const width = 2;
    const height = 2;
    const rowBytes = width * 4;
    const pitchBytes = rowBytes + 4; // 4 bytes padding at end of each row

    // Only allocate the bytes actually required for the scanout surface:
    // (height-1)*pitchBytes + rowBytes. The unused pitch padding after the last row is omitted.
    const requiredSrcBytes = pitchBytes * (height - 1) + rowBytes;
    const guest = new Uint8Array(requiredSrcBytes);
    guest.fill(0xee);

    // Row0 pixels.
    guest.set([1, 2, 3, 0, 4, 5, 6, 0], 0);
    // Row1 pixels (at offset pitchBytes).
    guest.set([7, 8, 9, 0, 10, 11, 12, 0], pitchBytes);

    const out = readScanoutRgba8FromGuestRam(guest, {
      basePaddr: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_B8G8R8X8,
    });

    expect(out.rgba8.byteLength).toBe(rowBytes * height);
    expect(Array.from(out.rgba8)).toEqual([
      // row0
      3, 2, 1, 255, 6, 5, 4, 255,
      // row1
      9, 8, 7, 255, 12, 11, 10, 255,
    ]);
  });

  it("converts BGRA->RGBA and preserves alpha", () => {
    const width = 2;
    const height = 1;
    const pitchBytes = width * 4;

    const guest = new Uint8Array([
      // pixel0: B=1,G=2,R=3,A=4
      1, 2, 3, 4,
      // pixel1: B=5,G=6,R=7,A=8
      5, 6, 7, 8,
    ]);

    const out = readScanoutRgba8FromGuestRam(guest, {
      basePaddr: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_B8G8R8A8,
    });

    expect(Array.from(out.rgba8)).toEqual([3, 2, 1, 4, 7, 6, 5, 8]);
  });

  it("converts B5G6R5 (RGB565) -> RGBA and forces alpha=255", () => {
    const width = 2;
    const height = 1;
    const pitchBytes = width * 2;

    const guest = new Uint8Array([
      // pixel0: RGB565 red = 0xF800 (LE: 00 F8)
      0x00, 0xf8,
      // pixel1: RGB565 green = 0x07E0 (LE: E0 07)
      0xe0, 0x07,
    ]);

    const out = readScanoutRgba8FromGuestRam(guest, {
      basePaddr: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_B5G6R5,
    });

    expect(Array.from(out.rgba8)).toEqual([
      // pixel0
      255, 0, 0, 255,
      // pixel1
      0, 255, 0, 255,
    ]);
  });

  it("converts B5G5R5A1 -> RGBA and expands alpha bit", () => {
    const width = 2;
    const height = 1;
    const pitchBytes = width * 2;

    const guest = new Uint8Array([
      // pixel0: B5G5R5A1 red, alpha=1 => 0xFC00 (LE: 00 FC)
      0x00, 0xfc,
      // pixel1: B5G5R5A1 red, alpha=0 => 0x7C00 (LE: 00 7C)
      0x00, 0x7c,
    ]);

    const out = readScanoutRgba8FromGuestRam(guest, {
      basePaddr: 0,
      width,
      height,
      pitchBytes,
      format: SCANOUT_FORMAT_B5G5R5A1,
    });

    expect(Array.from(out.rgba8)).toEqual([
      // pixel0
      255, 0, 0, 255,
      // pixel1
      255, 0, 0, 0,
    ]);
  });

  it("throws for invalid pitchBytes (< width*4)", () => {
    const guest = new Uint8Array(64);
    expect(() =>
      readScanoutRgba8FromGuestRam(guest, {
        basePaddr: 0,
        width: 3,
        height: 1,
        pitchBytes: 8, // < 3*4
        format: SCANOUT_FORMAT_B8G8R8X8,
      }),
    ).toThrow(/pitchBytes/i);
  });

  it("throws when basePaddr is out of bounds", () => {
    // 16 bytes of guest RAM, but rowBytes=8, basePaddr=12 would require bytes [12..20).
    const guest = new Uint8Array(16);
    expect(() =>
      readScanoutRgba8FromGuestRam(guest, {
        basePaddr: 12,
        width: 2,
        height: 1,
        pitchBytes: 8,
        format: SCANOUT_FORMAT_B8G8R8X8,
      }),
    ).toThrow(/out of bounds/i);
  });

  it("returns null for absurd dimensions (caps RGBA8 output size)", () => {
    // width chosen so pitchBytes can still be a plausible u32 (width*4 ~= 0xffff_fffc).
    const width = 0x3fff_ffff;
    const height = 2;
    expect(tryComputeScanoutRgba8ByteLength(width, height)).toBeNull();
  });

  it("converts sRGB RGBX->RGBA and forces alpha=255", () => {
    const guest = new Uint8Array([
      // pixel0: R=1,G=2,B=3,X=0
      1, 2, 3, 0,
    ]);
    const out = readScanoutRgba8FromGuestRam(guest, {
      basePaddr: 0,
      width: 1,
      height: 1,
      pitchBytes: 4,
      format: SCANOUT_FORMAT_R8G8B8X8_SRGB,
    });
    expect(Array.from(out.rgba8)).toEqual([1, 2, 3, 255]);
  });
});
