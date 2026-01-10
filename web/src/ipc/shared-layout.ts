// Shared memory layout for the CPU worker → GPU worker framebuffer.
//
// Keep this file in sync with:
//   crates/aero-shared/src/shared_framebuffer.rs
//
// The layout is intentionally "boring": a small atomic header followed by two
// framebuffer slots (double buffering) and optional per-slot dirty-tile bitsets.

export const SHARED_FRAMEBUFFER_MAGIC = 0xA3F0_FB01;
export const SHARED_FRAMEBUFFER_VERSION = 1;

export const SHARED_FRAMEBUFFER_SLOTS = 2 as const;

export const SHARED_FRAMEBUFFER_HEADER_U32_LEN = 16 as const;
export const SHARED_FRAMEBUFFER_HEADER_BYTE_LEN = SHARED_FRAMEBUFFER_HEADER_U32_LEN * 4;

export const SHARED_FRAMEBUFFER_ALIGNMENT = 64 as const;

export enum SharedFramebufferHeaderIndex {
  MAGIC = 0,
  VERSION = 1,
  WIDTH = 2,
  HEIGHT = 3,
  STRIDE_BYTES = 4,
  FORMAT = 5,
  ACTIVE_INDEX = 6,
  FRAME_SEQ = 7,
  FRAME_DIRTY = 8,
  TILE_SIZE = 9,
  TILES_X = 10,
  TILES_Y = 11,
  DIRTY_WORDS_PER_BUFFER = 12,
  BUF0_FRAME_SEQ = 13,
  BUF1_FRAME_SEQ = 14,
  FLAGS = 15,
}

export enum FramebufferFormat {
  RGBA8 = 0,
}

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
  const width = Atomics.load(header, SharedFramebufferHeaderIndex.WIDTH);
  const height = Atomics.load(header, SharedFramebufferHeaderIndex.HEIGHT);
  const strideBytes = Atomics.load(header, SharedFramebufferHeaderIndex.STRIDE_BYTES);
  const format = Atomics.load(header, SharedFramebufferHeaderIndex.FORMAT) as FramebufferFormat;
  const tileSize = Atomics.load(header, SharedFramebufferHeaderIndex.TILE_SIZE);
  return computeSharedFramebufferLayout(width, height, strideBytes, format, tileSize);
}

function alignUp(value: number, align: number): number {
  return (value + align - 1) & ~(align - 1);
}

function divCeil(value: number, divisor: number): number {
  return Math.floor((value + divisor - 1) / divisor);
}

