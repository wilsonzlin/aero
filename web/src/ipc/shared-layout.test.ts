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
});

