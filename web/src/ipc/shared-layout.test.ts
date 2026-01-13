import { describe, expect, it } from "vitest";

import { FramebufferFormat, computeSharedFramebufferLayout, dirtyTilesToRects } from "./shared-layout";

describe("ipc/shared-layout dirtyTilesToRects", () => {
  it("falls back to a full-frame rect when the dirty rect cap would be exceeded", () => {
    // tileSize=1 means each pixel is a tile; an alternating dirty pattern produces one rect per
    // dirty tile (no horizontal merging), quickly exceeding the MAX_DIRTY_RECTS cap.
    const width = 512;
    const height = 257;
    const layout = computeSharedFramebufferLayout(
      width,
      height,
      /*strideBytes=*/ width * 4,
      FramebufferFormat.RGBA8,
      /*tileSize=*/ 1,
    );

    // Mark every other tile dirty. Since tilesX is divisible by 32, each scanline starts on a word
    // boundary and we can use a constant alternating-bit pattern.
    const dirtyWords = new Uint32Array(layout.dirtyWordsPerBuffer);
    dirtyWords.fill(0x55555555);

    const rects = dirtyTilesToRects(layout, dirtyWords);
    expect(rects).toEqual([{ x: 0, y: 0, w: width, h: height }]);
  });

  it("merges horizontal dirty runs into a single rect", () => {
    // Mirrors the Rust unit test `dirty_tile_rects_merge_runs_horizontally`.
    const layout = computeSharedFramebufferLayout(
      64,
      64,
      /*strideBytes=*/ 64 * 4,
      FramebufferFormat.RGBA8,
      /*tileSize=*/ 32,
    );
    expect(layout.tilesX).toBe(2);
    expect(layout.tilesY).toBe(2);

    // Mark top row tiles dirty (tiles 0 and 1).
    const dirtyWords = new Uint32Array(layout.dirtyWordsPerBuffer);
    dirtyWords[0] = 0b11;

    const rects = dirtyTilesToRects(layout, dirtyWords);
    expect(rects).toEqual([{ x: 0, y: 0, w: 64, h: 32 }]);
  });

  it("treats 31-bit trailing dirty word as fully set (remaining === 31 regression)", () => {
    // 7 * 9 = 63 tiles => fullWords=1, remaining=31.
    const layout = computeSharedFramebufferLayout(
      7,
      9,
      /*strideBytes=*/ 7 * 4,
      FramebufferFormat.RGBA8,
      /*tileSize=*/ 1,
    );

    const dirtyWords = new Uint32Array([
      0xffffffff, // first 32 tiles
      0x7fffffff, // remaining 31 tiles
    ]);

    expect(dirtyTilesToRects(layout, dirtyWords)).toEqual([{ x: 0, y: 0, w: 7, h: 9 }]);
  });

  it("still fast-paths when tile count is a multiple of 32 (remaining === 0)", () => {
    // 8 * 8 = 64 tiles => remaining=0.
    const layout = computeSharedFramebufferLayout(
      8,
      8,
      /*strideBytes=*/ 8 * 4,
      FramebufferFormat.RGBA8,
      /*tileSize=*/ 1,
    );
    const dirtyWords = new Uint32Array([0xffffffff, 0xffffffff]);

    expect(dirtyTilesToRects(layout, dirtyWords)).toEqual([{ x: 0, y: 0, w: 8, h: 8 }]);
  });
});
