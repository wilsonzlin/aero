import { describe, expect, it } from "vitest";

import { convertScanoutToRgba8, swizzleBgraToRgba32, swizzleBgrxToRgba32 } from "./scanout_swizzle";

describe("workers/scanout_swizzle", () => {
  it("swizzles BGRX -> RGBA (forces alpha)", () => {
    // src bytes: [B, G, R, X]
    // u32 little-endian: 0xXXRRGGBB
    const v = 0x4433_2211;
    // dst bytes: [R, G, B, 0xFF]
    // u32 little-endian: 0xFFBBGGRR
    expect(swizzleBgrxToRgba32(v)).toBe(0xff11_2233);
  });

  it("swizzles BGRA -> RGBA (preserves alpha)", () => {
    const v = 0x4433_2211;
    expect(swizzleBgraToRgba32(v)).toBe(0x4411_2233);
  });

  it("converts using the u32 fast path when aligned", () => {
    const width = 2;
    const height = 2;
    const srcStrideBytes = width * 4;
    const dstStrideBytes = width * 4;

    // Two pixels per row:
    // row0: BGRX: [1,2,3,4] [5,6,7,8]
    // row1: BGRX: [9,10,11,12] [13,14,15,16]
    const src = new Uint8Array([
      1, 2, 3, 4, 5, 6, 7, 8,
      9, 10, 11, 12, 13, 14, 15, 16,
    ]);
    const dst = new Uint8Array(width * height * 4);

    const usedFast = convertScanoutToRgba8({
      src,
      srcStrideBytes,
      dst,
      dstStrideBytes,
      width,
      height,
      kind: "bgrx",
    });
    expect(usedFast).toBe(true);

    expect(Array.from(dst)).toEqual([
      3, 2, 1, 255, 7, 6, 5, 255,
      11, 10, 9, 255, 15, 14, 13, 255,
    ]);
  });

  it("converts BGRA using the u32 fast path when aligned (preserves alpha)", () => {
    const width = 2;
    const height = 1;
    const srcStrideBytes = width * 4;
    const dstStrideBytes = width * 4;

    const src = new Uint8Array([
      // pixel0: B,G,R,A
      0x11, 0x22, 0x33, 0x44,
      // pixel1: B,G,R,A
      0x55, 0x66, 0x77, 0x88,
    ]);
    const dst = new Uint8Array(width * height * 4);

    const usedFast = convertScanoutToRgba8({
      src,
      srcStrideBytes,
      dst,
      dstStrideBytes,
      width,
      height,
      kind: "bgra",
    });
    expect(usedFast).toBe(true);
    expect(Array.from(dst)).toEqual([0x33, 0x22, 0x11, 0x44, 0x77, 0x66, 0x55, 0x88]);
  });

  it("converts RGBA using the u32 fast path when aligned (identity)", () => {
    const width = 2;
    const height = 1;
    const srcStrideBytes = width * 4;
    const dstStrideBytes = width * 4;

    const src = new Uint8Array([
      // pixel0: R,G,B,A
      0x11, 0x22, 0x33, 0x44,
      // pixel1: R,G,B,A
      0x55, 0x66, 0x77, 0x88,
    ]);
    const dst = new Uint8Array(width * height * 4);

    const usedFast = convertScanoutToRgba8({
      src,
      srcStrideBytes,
      dst,
      dstStrideBytes,
      width,
      height,
      kind: "rgba",
    });
    expect(usedFast).toBe(true);
    expect(Array.from(dst)).toEqual([0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88]);
  });

  it("converts RGBX using the u32 fast path when aligned (forces alpha)", () => {
    const width = 2;
    const height = 1;
    const srcStrideBytes = width * 4;
    const dstStrideBytes = width * 4;

    const src = new Uint8Array([
      // pixel0: R,G,B,X
      0x11, 0x22, 0x33, 0x44,
      // pixel1: R,G,B,X
      0x55, 0x66, 0x77, 0x88,
    ]);
    const dst = new Uint8Array(width * height * 4);

    const usedFast = convertScanoutToRgba8({
      src,
      srcStrideBytes,
      dst,
      dstStrideBytes,
      width,
      height,
      kind: "rgbx",
    });
    expect(usedFast).toBe(true);
    expect(Array.from(dst)).toEqual([0x11, 0x22, 0x33, 0xff, 0x55, 0x66, 0x77, 0xff]);
  });

  it("converts correctly via byte fallback when unaligned", () => {
    const width = 1;
    const height = 1;
    const srcStrideBytes = 4;
    const dstStrideBytes = 4;

    const buf = new ArrayBuffer(12);
    const src = new Uint8Array(buf, 1, 4);
    src.set([0x11, 0x22, 0x33, 0x44]); // B,G,R,A
    const dst = new Uint8Array(buf, 1 + 4, 4);

    const usedFast = convertScanoutToRgba8({
      src,
      srcStrideBytes,
      dst,
      dstStrideBytes,
      width,
      height,
      kind: "bgra",
    });
    expect(usedFast).toBe(false);
    expect(Array.from(dst)).toEqual([0x33, 0x22, 0x11, 0x44]);
  });

  it("converts RGBX correctly via byte fallback when unaligned (forces alpha)", () => {
    const width = 1;
    const height = 1;
    const srcStrideBytes = 4;
    const dstStrideBytes = 4;

    const buf = new ArrayBuffer(12);
    const src = new Uint8Array(buf, 1, 4);
    src.set([0x11, 0x22, 0x33, 0x44]); // R,G,B,X
    const dst = new Uint8Array(buf, 1 + 4, 4);

    const usedFast = convertScanoutToRgba8({
      src,
      srcStrideBytes,
      dst,
      dstStrideBytes,
      width,
      height,
      kind: "rgbx",
    });
    expect(usedFast).toBe(false);
    expect(Array.from(dst)).toEqual([0x11, 0x22, 0x33, 0xff]);
  });
});
