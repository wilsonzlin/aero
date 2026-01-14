// Shared framebuffer protocol between the emulator core and UI.
//
// The shared region is:
// - Header: Int32Array (little-endian) with atomic-friendly fields.
// - Pixel data: RGBA8888 bytes with optional stride padding per row.
//
// Color space note:
// - The framebuffer bytes are treated as **linear** RGBA8 by the GPU runtime and the
//   canonical presenters.
// - Canvas2D `putImageData` expects sRGB-encoded bytes, so Canvas2D presenters encode
//   linear→sRGB for display (and do so on a copy when the source bytes are used for
//   deterministic hashing).
//
// The header is intentionally small and stable; extend by bumping VERSION and
// appending fields (never reorder existing ones).

export const FRAMEBUFFER_MAGIC = 0x4f524541; // "AERO" in little-endian u32
export const FRAMEBUFFER_VERSION = 1;

export const FRAMEBUFFER_FORMAT_RGBA8888 = 1;

export const HEADER_INDEX_MAGIC = 0;
export const HEADER_INDEX_VERSION = 1;
export const HEADER_INDEX_WIDTH = 2;
export const HEADER_INDEX_HEIGHT = 3;
export const HEADER_INDEX_STRIDE_BYTES = 4;
export const HEADER_INDEX_FORMAT = 5;
export const HEADER_INDEX_FRAME_COUNTER = 6;
export const HEADER_INDEX_CONFIG_COUNTER = 7;

export const HEADER_I32_COUNT = 8;
export const HEADER_BYTE_LENGTH = HEADER_I32_COUNT * 4;

export type SharedFramebufferView = Readonly<{
  buffer: ArrayBuffer | SharedArrayBuffer;
  byteOffset: number;
  header: Int32Array;
  pixelsU8: Uint8Array;
  pixelsU8Clamped: Uint8ClampedArray;
}>;

export type FramebufferHeaderConfig = Readonly<{
  width: number;
  height: number;
  strideBytes: number;
  format?: number;
}>;

export function requiredFramebufferBytes(
  width: number,
  height: number,
  strideBytes: number = width * 4,
): number {
  if (!Number.isInteger(width) || width <= 0) {
    throw new Error(`Invalid width: ${width}`);
  }
  if (!Number.isInteger(height) || height <= 0) {
    throw new Error(`Invalid height: ${height}`);
  }
  if (!Number.isInteger(strideBytes) || strideBytes < width * 4) {
    throw new Error(`Invalid strideBytes: ${strideBytes} (min ${width * 4})`);
  }
  return HEADER_BYTE_LENGTH + strideBytes * height;
}

/**
 * Returns true if `buffer` is a SharedArrayBuffer (and the runtime supports it).
 */
export function isSharedArrayBuffer(
  buffer: ArrayBuffer | SharedArrayBuffer,
): buffer is SharedArrayBuffer {
  return typeof SharedArrayBuffer !== "undefined" && buffer instanceof SharedArrayBuffer;
}

export function loadHeaderI32(header: Int32Array, index: number): number {
  if (isSharedArrayBuffer(header.buffer)) {
    return Atomics.load(header, index);
  }
  return header[index];
}

export function storeHeaderI32(header: Int32Array, index: number, value: number): void {
  if (isSharedArrayBuffer(header.buffer)) {
    Atomics.store(header, index, value);
    return;
  }
  header[index] = value;
}

/**
 * Atomically increments a header field.
 */
export function addHeaderI32(header: Int32Array, index: number, delta: number): number {
  if (isSharedArrayBuffer(header.buffer)) {
    return Atomics.add(header, index, delta);
  }
  const prev = header[index];
  header[index] = prev + delta;
  return prev;
}

/**
 * Initializes or re-initializes the shared header.
 */
export function initFramebufferHeader(
  header: Int32Array,
  { width, height, strideBytes, format = FRAMEBUFFER_FORMAT_RGBA8888 }: FramebufferHeaderConfig,
): void {
  storeHeaderI32(header, HEADER_INDEX_MAGIC, FRAMEBUFFER_MAGIC);
  storeHeaderI32(header, HEADER_INDEX_VERSION, FRAMEBUFFER_VERSION);
  storeHeaderI32(header, HEADER_INDEX_WIDTH, width);
  storeHeaderI32(header, HEADER_INDEX_HEIGHT, height);
  storeHeaderI32(header, HEADER_INDEX_STRIDE_BYTES, strideBytes);
  storeHeaderI32(header, HEADER_INDEX_FORMAT, format);
  storeHeaderI32(header, HEADER_INDEX_FRAME_COUNTER, 0);
  storeHeaderI32(header, HEADER_INDEX_CONFIG_COUNTER, 1);
}

/**
 * Wraps a framebuffer region stored in `buffer` at `byteOffset`.
 */
export function wrapSharedFramebuffer(
  buffer: ArrayBuffer | SharedArrayBuffer,
  byteOffset: number = 0,
): SharedFramebufferView {
  if (!Number.isInteger(byteOffset) || byteOffset < 0) {
    throw new Error(`Invalid byteOffset: ${byteOffset}`);
  }
  if (byteOffset + HEADER_BYTE_LENGTH > buffer.byteLength) {
    throw new Error(`Buffer too small for framebuffer header at offset ${byteOffset}`);
  }

  const header = new Int32Array(buffer, byteOffset, HEADER_I32_COUNT);
  const pixelOffset = byteOffset + HEADER_BYTE_LENGTH;
  const pixelsLen = buffer.byteLength - pixelOffset;

  return {
    buffer,
    byteOffset,
    header,
    pixelsU8: new Uint8Array(buffer, pixelOffset, pixelsLen),
    pixelsU8Clamped: new Uint8ClampedArray(buffer, pixelOffset, pixelsLen),
  };
}

export const FRAMEBUFFER_COPY_MESSAGE_TYPE = "aero.framebuffer.copy.v1";

export type FramebufferCopyFrame = Readonly<{
  width: number;
  height: number;
  strideBytes: number;
  format: number;
  frameCounter: number;
  pixelsU8: Uint8Array;
}>;

export type FramebufferCopyMessageV1 = Readonly<{
  type: typeof FRAMEBUFFER_COPY_MESSAGE_TYPE;
  width: number;
  height: number;
  strideBytes: number;
  format: number;
  frameCounter: number;
  pixels: ArrayBuffer;
}>;

/**
 * Runtime type guard for messages produced by the worker copy path.
 */
export function isFramebufferCopyMessageV1(msg: unknown): msg is FramebufferCopyMessageV1 {
  if (!msg || typeof msg !== "object") return false;
  const obj = msg as Record<string, unknown>;
  return (
    obj.type === FRAMEBUFFER_COPY_MESSAGE_TYPE &&
    typeof obj.width === "number" &&
    typeof obj.height === "number" &&
    typeof obj.strideBytes === "number" &&
    typeof obj.format === "number" &&
    typeof obj.frameCounter === "number" &&
    obj.pixels instanceof ArrayBuffer
  );
}

export function copyFrameFromMessageV1(msg: FramebufferCopyMessageV1): FramebufferCopyFrame {
  return {
    width: msg.width,
    height: msg.height,
    strideBytes: msg.strideBytes,
    format: msg.format,
    frameCounter: msg.frameCounter,
    pixelsU8: new Uint8Array(msg.pixels),
  };
}
