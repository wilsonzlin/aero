import { describe, expect, it } from "vitest";

import {
  computeSharedFramebufferLayout,
  dirtyTilesToRects,
  FramebufferFormat,
  layoutFromHeader,
  SHARED_FRAMEBUFFER_ALIGNMENT,
  SHARED_FRAMEBUFFER_HEADER_BYTE_LEN,
  SHARED_FRAMEBUFFER_HEADER_U32_LEN,
  SharedFramebufferHeaderIndex,
} from "./shared-layout";

describe("ipc/shared-layout SharedFramebufferLayout", () => {
  it("computeSharedFramebufferLayout: 640x480 RGBA8 tileSize=0 matches Rust layout", () => {
    const layout = computeSharedFramebufferLayout(640, 480, 640 * 4, FramebufferFormat.RGBA8, /*tileSize=*/ 0);

    // Mirror `aero_shared::shared_framebuffer::tests::layout_is_stable_and_aligned` and lock down
    // exact offsets/size so JS stays bit-for-bit compatible with the Rust ABI.
    expect(layout.framebufferOffsets).toEqual([64, 1_228_864]);
    expect(layout.totalBytes).toBe(2_457_664);

    for (const offset of layout.framebufferOffsets) {
      expect(offset % SHARED_FRAMEBUFFER_ALIGNMENT).toBe(0);
    }
    expect(layout.totalBytes % SHARED_FRAMEBUFFER_ALIGNMENT).toBe(0);

    // With dirty tracking disabled, the dirty offsets collapse to the end-of-buffer markers.
    expect(layout.dirtyOffsets).toEqual([1_228_864, 2_457_664]);
    expect(layout.tilesX).toBe(0);
    expect(layout.tilesY).toBe(0);
    expect(layout.dirtyWordsPerBuffer).toBe(0);
  });

  it("computeSharedFramebufferLayout: 640x480 RGBA8 tileSize=32 matches Rust layout (with dirty words)", () => {
    const layout = computeSharedFramebufferLayout(640, 480, 640 * 4, FramebufferFormat.RGBA8, /*tileSize=*/ 32);

    expect(layout.tilesX).toBe(20);
    expect(layout.tilesY).toBe(15);
    // 20*15=300 tiles => ceil(300/32)=10 u32 words.
    expect(layout.dirtyWordsPerBuffer).toBe(10);

    expect(layout.framebufferOffsets).toEqual([64, 1_228_928]);
    expect(layout.dirtyOffsets).toEqual([1_228_864, 2_457_728]);
    expect(layout.totalBytes).toBe(2_457_792);

    for (const offset of layout.framebufferOffsets) {
      expect(offset % SHARED_FRAMEBUFFER_ALIGNMENT).toBe(0);
    }
    expect(layout.totalBytes % SHARED_FRAMEBUFFER_ALIGNMENT).toBe(0);
  });

  it("layoutFromHeader returns the same layout as computeSharedFramebufferLayout", () => {
    const expected = computeSharedFramebufferLayout(640, 480, 640 * 4, FramebufferFormat.RGBA8, /*tileSize=*/ 32);

    const sab = new SharedArrayBuffer(SHARED_FRAMEBUFFER_HEADER_BYTE_LEN);
    const header = new Int32Array(sab, 0, SHARED_FRAMEBUFFER_HEADER_U32_LEN);
    Atomics.store(header, SharedFramebufferHeaderIndex.WIDTH, expected.width);
    Atomics.store(header, SharedFramebufferHeaderIndex.HEIGHT, expected.height);
    Atomics.store(header, SharedFramebufferHeaderIndex.STRIDE_BYTES, expected.strideBytes);
    Atomics.store(header, SharedFramebufferHeaderIndex.FORMAT, expected.format);
    Atomics.store(header, SharedFramebufferHeaderIndex.TILE_SIZE, expected.tileSize);

    const actual = layoutFromHeader(header);
    expect(actual).toEqual(expected);
  });
});

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

  it("merges horizontal runs (2x2 tiles, top row dirty → single rect)", () => {
    const layout = computeSharedFramebufferLayout(64, 64, 64 * 4, FramebufferFormat.RGBA8, /*tileSize=*/ 32);
    expect(layout.tilesX).toBe(2);
    expect(layout.tilesY).toBe(2);
    expect(layout.dirtyWordsPerBuffer).toBe(1);

    // Mark top row tiles dirty (tile 0 and tile 1).
    const dirtyWords = new Uint32Array([0b11]);
    const rects = dirtyTilesToRects(layout, dirtyWords);
    expect(rects).toEqual([{ x: 0, y: 0, w: 64, h: 32 }]);
  });

  it("returns a full-frame rect when all tiles are dirty", () => {
    const layout = computeSharedFramebufferLayout(64, 64, 64 * 4, FramebufferFormat.RGBA8, /*tileSize=*/ 32);

    // Mark all 4 tiles dirty.
    const dirtyWords = new Uint32Array([0b1111]);
    const rects = dirtyTilesToRects(layout, dirtyWords);
    expect(rects).toEqual([{ x: 0, y: 0, w: 64, h: 64 }]);
  });

  it("clamps rect width when the tile grid rounds up past framebuffer width", () => {
    // Mirror Rust regression test: tile rounding can cause x+w to overshoot the true width.
    const tileSize = 1 << 30;
    const width = 0xffff_ffff;
    const layout = computeSharedFramebufferLayout(width, 1, width * 4, FramebufferFormat.RGBA8, tileSize);
    expect(layout.tilesX).toBe(4);
    expect(layout.tilesY).toBe(1);

    // Mark only the last tile dirty.
    const rects = dirtyTilesToRects(layout, new Uint32Array([0b1000]));
    expect(rects).toEqual([{ x: 3 * tileSize, y: 0, w: tileSize - 1, h: 1 }]);
  });

  it("clamps rect height when the tile grid rounds up past framebuffer height", () => {
    const tileSize = 1 << 30;
    const height = 0xffff_ffff;
    const layout = computeSharedFramebufferLayout(1, height, 4, FramebufferFormat.RGBA8, tileSize);
    expect(layout.tilesX).toBe(1);
    expect(layout.tilesY).toBe(4);

    // Mark only the last tile dirty (tile_index=3).
    const rects = dirtyTilesToRects(layout, new Uint32Array([0b1000]));
    expect(rects).toEqual([{ x: 0, y: 3 * tileSize, w: 1, h: tileSize - 1 }]);
  });

  it("treats 31-bit trailing dirty word as fully set (remaining === 31 regression)", () => {
    // 7 * 9 = 63 tiles => fullWords=1, remaining=31.
    const layout = computeSharedFramebufferLayout(7, 9, /*strideBytes=*/ 7 * 4, FramebufferFormat.RGBA8, /*tileSize=*/ 1);

    const dirtyWords = new Uint32Array([
      0xffffffff, // first 32 tiles
      0x7fffffff, // remaining 31 tiles
    ]);

    expect(dirtyTilesToRects(layout, dirtyWords)).toEqual([{ x: 0, y: 0, w: 7, h: 9 }]);
  });

  it("still fast-paths when tile count is a multiple of 32 (remaining === 0)", () => {
    // 8 * 8 = 64 tiles => remaining=0.
    const layout = computeSharedFramebufferLayout(8, 8, /*strideBytes=*/ 8 * 4, FramebufferFormat.RGBA8, /*tileSize=*/ 1);
    const dirtyWords = new Uint32Array([0xffffffff, 0xffffffff]);

    expect(dirtyTilesToRects(layout, dirtyWords)).toEqual([{ x: 0, y: 0, w: 8, h: 8 }]);
  });
});

describe("ipc/shared-layout computeSharedFramebufferLayout overflow", () => {
  it("throws on unsafe integer overflow in bufferBytes (tileSize=0)", () => {
    // `strideBytes` and `height` individually fit in u32, but their product does not fit in a JS safe integer.
    expect(() =>
      computeSharedFramebufferLayout(
        /*width=*/ 1,
        /*height=*/ 0xffff_ffff,
        /*strideBytes=*/ 0xffff_ffff,
        FramebufferFormat.RGBA8,
        /*tileSize=*/ 0,
      ),
    ).toThrow(/layout size overflow/);
  });

  it("throws on unsafe integer overflow in cursor/offset additions (tileSize!=0)", () => {
    // Choose a `bufferBytes` that is still a safe integer, but large enough that two slots (plus
    // header/alignment) exceed `Number.MAX_SAFE_INTEGER` and must be rejected.
    const strideBytes = 70_000_000;
    const height = 70_000_000;

    expect(() =>
      computeSharedFramebufferLayout(
        /*width=*/ 1,
        height,
        strideBytes,
        FramebufferFormat.RGBA8,
        /*tileSize=*/ 1,
      ),
    ).toThrow(/layout size overflow/);
  });
});
