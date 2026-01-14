// Shared memory layout for the CPU worker → GPU worker framebuffer.
//
// Keep this file in sync with:
//   crates/aero-shared/src/shared_framebuffer.rs
//
// The layout is intentionally "boring": a small atomic header followed by two
// framebuffer slots (double buffering) and optional per-slot dirty-tile bitsets.
//
// Color space note:
// - Framebuffer bytes are treated as **linear RGBA8** by the GPU worker runtime.
// - Presenter backends perform any required linear→sRGB encoding during the final blit.

// Stored in an Int32Array header (Atomics requires a signed typed array), so
// the u32 literal is converted via ToInt32 and becomes negative in JS.
export const SHARED_FRAMEBUFFER_MAGIC = 0xA3F0_FB01 | 0;
export const SHARED_FRAMEBUFFER_VERSION = 1;

export const SHARED_FRAMEBUFFER_SLOTS = 2 as const;

export const SHARED_FRAMEBUFFER_HEADER_U32_LEN = 16 as const;
export const SHARED_FRAMEBUFFER_HEADER_BYTE_LEN = SHARED_FRAMEBUFFER_HEADER_U32_LEN * 4;

export const SHARED_FRAMEBUFFER_ALIGNMENT = 64 as const;

export const SharedFramebufferHeaderIndex = {
  MAGIC: 0,
  VERSION: 1,
  WIDTH: 2,
  HEIGHT: 3,
  STRIDE_BYTES: 4,
  FORMAT: 5,
  ACTIVE_INDEX: 6,
  FRAME_SEQ: 7,
  FRAME_DIRTY: 8,
  TILE_SIZE: 9,
  TILES_X: 10,
  TILES_Y: 11,
  DIRTY_WORDS_PER_BUFFER: 12,
  BUF0_FRAME_SEQ: 13,
  BUF1_FRAME_SEQ: 14,
  FLAGS: 15,
} as const;

export type SharedFramebufferHeaderIndex =
  (typeof SharedFramebufferHeaderIndex)[keyof typeof SharedFramebufferHeaderIndex];

export const FramebufferFormat = {
  RGBA8: 0,
} as const;

export type FramebufferFormat = (typeof FramebufferFormat)[keyof typeof FramebufferFormat];

export interface SharedFramebufferLayout {
  width: number;
  height: number;
  strideBytes: number;
  format: FramebufferFormat;

  tileSize: number;
  tilesX: number;
  tilesY: number;
  dirtyWordsPerBuffer: number;

  framebufferOffsets: [number, number];
  dirtyOffsets: [number, number];
  totalBytes: number;
}

export function computeSharedFramebufferLayout(
  width: number,
  height: number,
  strideBytes: number,
  format: FramebufferFormat,
  tileSize: number,
): SharedFramebufferLayout {
  if (width <= 0 || height <= 0) {
    throw new Error("width/height must be > 0");
  }
  // In Rust, all layout inputs are `u32` and intermediate sizes are computed with `checked_*`.
  // In JS, `number` arithmetic can silently lose precision once intermediate values exceed
  // `Number.MAX_SAFE_INTEGER` (~9e15). Reject any layout that would require unsafe integers so
  // all offsets remain exact.
  if (
    !Number.isSafeInteger(width) ||
    !Number.isSafeInteger(height) ||
    !Number.isSafeInteger(strideBytes) ||
    !Number.isSafeInteger(tileSize)
  ) {
    layoutSizeOverflow();
  }
  if (tileSize !== 0 && (tileSize & (tileSize - 1)) !== 0) {
    throw new Error("tileSize must be 0 (disabled) or a power-of-two");
  }

  const bytesPerPixel = 4;
  const minStrideBytes = checkedMul(width, bytesPerPixel);
  if (strideBytes < minStrideBytes) {
    throw new Error(`strideBytes (${strideBytes}) < minimum (${minStrideBytes})`);
  }

  const bufferBytes = checkedMul(strideBytes, height);

  const tilesX = tileSize === 0 ? 0 : divCeil(width, tileSize);
  const tilesY = tileSize === 0 ? 0 : divCeil(height, tileSize);
  const tileCount = checkedMul(tilesX, tilesY);
  const dirtyWordsPerBuffer = tileSize === 0 ? 0 : divCeil(tileCount, 32);

  let cursor = alignUp(SHARED_FRAMEBUFFER_HEADER_BYTE_LEN, SHARED_FRAMEBUFFER_ALIGNMENT);

  const slot0Fb = cursor;
  cursor = alignUp(checkedAdd(slot0Fb, bufferBytes), 4);
  const slot0Dirty = cursor;
  cursor = alignUp(
    checkedAdd(slot0Dirty, checkedMul(dirtyWordsPerBuffer, 4)),
    SHARED_FRAMEBUFFER_ALIGNMENT,
  );

  const slot1Fb = cursor;
  cursor = alignUp(checkedAdd(slot1Fb, bufferBytes), 4);
  const slot1Dirty = cursor;
  cursor = alignUp(
    checkedAdd(slot1Dirty, checkedMul(dirtyWordsPerBuffer, 4)),
    SHARED_FRAMEBUFFER_ALIGNMENT,
  );

  return {
    width,
    height,
    strideBytes,
    format,
    tileSize,
    tilesX,
    tilesY,
    dirtyWordsPerBuffer,
    framebufferOffsets: [slot0Fb, slot1Fb],
    dirtyOffsets: [slot0Dirty, slot1Dirty],
    totalBytes: cursor,
  };
}

export function layoutFromHeader(header: Int32Array): SharedFramebufferLayout {
  // The header is defined as `u32[]` in Rust. When read through an `Int32Array`,
  // values with the high bit set appear negative—always reinterpret as unsigned.
  const width = Atomics.load(header, SharedFramebufferHeaderIndex.WIDTH) >>> 0;
  const height = Atomics.load(header, SharedFramebufferHeaderIndex.HEIGHT) >>> 0;
  const strideBytes = Atomics.load(header, SharedFramebufferHeaderIndex.STRIDE_BYTES) >>> 0;
  const format = (Atomics.load(header, SharedFramebufferHeaderIndex.FORMAT) >>> 0) as FramebufferFormat;
  const tileSize = Atomics.load(header, SharedFramebufferHeaderIndex.TILE_SIZE) >>> 0;
  return computeSharedFramebufferLayout(width, height, strideBytes, format, tileSize);
}

export type DirtyRect = { x: number; y: number; w: number; h: number };

// Hard cap on the number of rects we will emit from a dirty-tile bitset before falling back to a
// single full-frame update. Mirrors `aero_shared::shared_framebuffer::dirty_tiles_to_rects`.
const MAX_DIRTY_RECTS = 65_536;

/**
 * Convert a per-tile dirty bitset into pixel-space rects, merging runs on the X axis.
 *
 * Mirrors `aero_shared::shared_framebuffer::dirty_tiles_to_rects`.
 */
export function dirtyTilesToRects(layout: SharedFramebufferLayout, dirtyWords: Uint32Array): DirtyRect[] {
  if (layout.tileSize === 0 || layout.dirtyWordsPerBuffer === 0) {
    return [{ x: 0, y: 0, w: layout.width, h: layout.height }];
  }

  const tileCount = layout.tilesX * layout.tilesY;
  if (tileCount === 0) return [];

  if (dirtyWordsCoverAllTiles(tileCount, dirtyWords)) {
    return [{ x: 0, y: 0, w: layout.width, h: layout.height }];
  }

  const rects: DirtyRect[] = [];
  const tileSize = layout.tileSize;

  for (let ty = 0; ty < layout.tilesY; ty += 1) {
    const y = ty * tileSize;
    if (y >= layout.height) break;

    let tx = 0;
    while (tx < layout.tilesX) {
      const tileIndex = ty * layout.tilesX + tx;
      if (!dirtyBitIsSet(dirtyWords, tileIndex)) {
        tx += 1;
        continue;
      }

      const startTx = tx;
      tx += 1;
      while (tx < layout.tilesX) {
        const nextIndex = ty * layout.tilesX + tx;
        if (!dirtyBitIsSet(dirtyWords, nextIndex)) break;
        tx += 1;
      }

      const x = startTx * tileSize;
      let w = (tx - startTx) * tileSize;
      if (x + w > layout.width) w = Math.max(0, layout.width - x);

      let h = tileSize;
      if (y + h > layout.height) h = Math.max(0, layout.height - y);

      if (rects.length >= MAX_DIRTY_RECTS) {
        return [{ x: 0, y: 0, w: layout.width, h: layout.height }];
      }

      try {
        rects.push({ x, y, w, h });
      } catch {
        // If allocation fails (e.g. OOM / array growth failure), fall back to a full-frame upload
        // rather than crashing the GPU worker.
        return [{ x: 0, y: 0, w: layout.width, h: layout.height }];
      }
    }
  }

  return rects;
}

const LAYOUT_SIZE_OVERFLOW_MESSAGE = "layout size overflow";

function layoutSizeOverflow(): never {
  throw new Error(LAYOUT_SIZE_OVERFLOW_MESSAGE);
}

function checkedMul(a: number, b: number): number {
  if (!Number.isSafeInteger(a) || !Number.isSafeInteger(b) || a < 0 || b < 0) {
    layoutSizeOverflow();
  }
  const out = a * b;
  if (!Number.isSafeInteger(out)) {
    layoutSizeOverflow();
  }
  return out;
}

function checkedAdd(a: number, b: number): number {
  if (!Number.isSafeInteger(a) || !Number.isSafeInteger(b) || a < 0 || b < 0) {
    layoutSizeOverflow();
  }
  const out = a + b;
  if (!Number.isSafeInteger(out)) {
    layoutSizeOverflow();
  }
  return out;
}

function alignUp(value: number, align: number): number {
  if (align <= 0 || (align & (align - 1)) !== 0) throw new Error("align must be a positive power of two");
  if (!Number.isSafeInteger(value) || value < 0) {
    layoutSizeOverflow();
  }
  const rem = value % align;
  const out = rem === 0 ? value : value + (align - rem);
  if (!Number.isSafeInteger(out)) {
    layoutSizeOverflow();
  }
  return out;
}

function divCeil(value: number, divisor: number): number {
  if (!Number.isSafeInteger(value) || value < 0 || !Number.isSafeInteger(divisor) || divisor <= 0) layoutSizeOverflow();
  const out = Number((BigInt(value) + BigInt(divisor) - 1n) / BigInt(divisor));
  if (!Number.isSafeInteger(out)) {
    layoutSizeOverflow();
  }
  return out;
}

function dirtyWordsCoverAllTiles(tileCount: number, dirtyWords: Uint32Array): boolean {
  if (tileCount <= 0) return true;

  const fullWords = Math.floor(tileCount / 32);
  const remaining = tileCount % 32;

  const requiredWords = fullWords + (remaining === 0 ? 0 : 1);
  if (dirtyWords.length < requiredWords) return false;

  for (let i = 0; i < fullWords; i += 1) {
    if (dirtyWords[i] !== 0xffffffff) return false;
  }

  if (remaining === 0) return true;

  // `1 << 31` overflows signed 32-bit and becomes negative, which then breaks the
  // `(dirtyWords[fullWords] & mask) === mask` comparison when `remaining === 31`.
  // Construct the mask using an unsigned shift instead.
  const mask = 0xffffffff >>> (32 - remaining);
  return (dirtyWords[fullWords] & mask) === mask;
}

function dirtyBitIsSet(words: Uint32Array, tileIndex: number): boolean {
  const word = Math.floor(tileIndex / 32);
  const bit = tileIndex % 32;
  return (words[word] & (1 << bit)) !== 0;
}
