// Shared memory layout for the CPU worker → GPU worker framebuffer.
//
// Keep this file in sync with:
//   crates/aero-shared/src/shared_framebuffer.rs
//
// The layout is intentionally "boring": a small atomic header followed by two
// framebuffer slots (double buffering) and optional per-slot dirty-tile bitsets.

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
  if (tileSize !== 0 && (tileSize & (tileSize - 1)) !== 0) {
    throw new Error("tileSize must be 0 (disabled) or a power-of-two");
  }

  const bytesPerPixel = 4;
  const minStrideBytes = width * bytesPerPixel;
  if (strideBytes < minStrideBytes) {
    throw new Error(`strideBytes (${strideBytes}) < minimum (${minStrideBytes})`);
  }

  const bufferBytes = strideBytes * height;

  const tilesX = tileSize === 0 ? 0 : divCeil(width, tileSize);
  const tilesY = tileSize === 0 ? 0 : divCeil(height, tileSize);
  const tileCount = tilesX * tilesY;
  const dirtyWordsPerBuffer = tileSize === 0 ? 0 : divCeil(tileCount, 32);

  let cursor = alignUp(SHARED_FRAMEBUFFER_HEADER_BYTE_LEN, SHARED_FRAMEBUFFER_ALIGNMENT);

  const slot0Fb = cursor;
  cursor = alignUp(slot0Fb + bufferBytes, 4);
  const slot0Dirty = cursor;
  cursor = alignUp(slot0Dirty + dirtyWordsPerBuffer * 4, SHARED_FRAMEBUFFER_ALIGNMENT);

  const slot1Fb = cursor;
  cursor = alignUp(slot1Fb + bufferBytes, 4);
  const slot1Dirty = cursor;
  cursor = alignUp(slot1Dirty + dirtyWordsPerBuffer * 4, SHARED_FRAMEBUFFER_ALIGNMENT);

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

      rects.push({ x, y, w, h });
    }
  }

  return rects;
}

function alignUp(value: number, align: number): number {
  if (align <= 0 || (align & (align - 1)) !== 0) throw new Error("align must be a positive power of two");
  const rem = value % align;
  return rem === 0 ? value : value + (align - rem);
}

function divCeil(value: number, divisor: number): number {
  if (!Number.isSafeInteger(value) || value < 0 || !Number.isSafeInteger(divisor) || divisor <= 0) {
    throw new Error("divCeil: arguments must be safe non-negative integers and divisor must be > 0");
  }
  const out = Number((BigInt(value) + BigInt(divisor) - 1n) / BigInt(divisor));
  if (!Number.isSafeInteger(out)) {
    throw new Error("divCeil overflow");
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

  const mask = (1 << remaining) - 1;
  return (dirtyWords[fullWords] & mask) === mask;
}

function dirtyBitIsSet(words: Uint32Array, tileIndex: number): boolean {
  const word = Math.floor(tileIndex / 32);
  const bit = tileIndex % 32;
  return (words[word] & (1 << bit)) !== 0;
}
