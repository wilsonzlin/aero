// Shared framebuffer protocol between the emulator core and UI.
//
// The shared region is:
// - Header: Int32Array (little-endian) with atomic-friendly fields.
// - Pixel data: RGBA8888 bytes with optional stride padding per row.
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

/**
 * @param {number} width
 * @param {number} height
 * @param {number} [strideBytes]
 */
export function requiredFramebufferBytes(width, height, strideBytes = width * 4) {
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
 * @param {ArrayBuffer | SharedArrayBuffer} buffer
 * @returns {buffer is SharedArrayBuffer}
 */
export function isSharedArrayBuffer(buffer) {
  return typeof SharedArrayBuffer !== "undefined" && buffer instanceof SharedArrayBuffer;
}

/**
 * @param {Int32Array} header
 * @param {number} index
 */
export function loadHeaderI32(header, index) {
  if (isSharedArrayBuffer(header.buffer)) {
    return Atomics.load(header, index);
  }
  return header[index];
}

/**
 * @param {Int32Array} header
 * @param {number} index
 * @param {number} value
 */
export function storeHeaderI32(header, index, value) {
  if (isSharedArrayBuffer(header.buffer)) {
    Atomics.store(header, index, value);
    return;
  }
  header[index] = value;
}

/**
 * Atomically increments a header field.
 *
 * @param {Int32Array} header
 * @param {number} index
 * @param {number} delta
 */
export function addHeaderI32(header, index, delta) {
  if (isSharedArrayBuffer(header.buffer)) {
    return Atomics.add(header, index, delta);
  }
  const prev = header[index];
  header[index] = prev + delta;
  return prev;
}

/**
 * Initializes or re-initializes the shared header.
 *
 * @param {Int32Array} header
 * @param {{width: number, height: number, strideBytes: number, format?: number}} config
 */
export function initFramebufferHeader(header, { width, height, strideBytes, format = FRAMEBUFFER_FORMAT_RGBA8888 }) {
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
 * @typedef {object} SharedFramebufferView
 * @property {ArrayBuffer | SharedArrayBuffer} buffer
 * @property {number} byteOffset
 * @property {Int32Array} header
 * @property {Uint8Array} pixelsU8
 * @property {Uint8ClampedArray} pixelsU8Clamped
 */

/**
 * Wraps a framebuffer region stored in `buffer` at `byteOffset`.
 *
 * @param {ArrayBuffer | SharedArrayBuffer} buffer
 * @param {number} byteOffset
 * @returns {SharedFramebufferView}
 */
export function wrapSharedFramebuffer(buffer, byteOffset = 0) {
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

/**
 * @typedef {object} FramebufferCopyFrame
 * @property {number} width
 * @property {number} height
 * @property {number} strideBytes
 * @property {number} format
 * @property {number} frameCounter
 * @property {Uint8Array} pixelsU8
 */

export const FRAMEBUFFER_COPY_MESSAGE_TYPE = "aero.framebuffer.copy.v1";

/**
 * @typedef {object} FramebufferCopyMessageV1
 * @property {typeof FRAMEBUFFER_COPY_MESSAGE_TYPE} type
 * @property {number} width
 * @property {number} height
 * @property {number} strideBytes
 * @property {number} format
 * @property {number} frameCounter
 * @property {ArrayBuffer} pixels
 */

/**
 * @param {any} msg
 * @returns {msg is FramebufferCopyMessageV1}
 */
export function isFramebufferCopyMessageV1(msg) {
  return (
    msg != null &&
    typeof msg === "object" &&
    msg.type === FRAMEBUFFER_COPY_MESSAGE_TYPE &&
    typeof msg.width === "number" &&
    typeof msg.height === "number" &&
    typeof msg.strideBytes === "number" &&
    typeof msg.format === "number" &&
    typeof msg.frameCounter === "number" &&
    msg.pixels instanceof ArrayBuffer
  );
}

/**
 * @param {FramebufferCopyMessageV1} msg
 * @returns {FramebufferCopyFrame}
 */
export function copyFrameFromMessageV1(msg) {
  return {
    width: msg.width,
    height: msg.height,
    strideBytes: msg.strideBytes,
    format: msg.format,
    frameCounter: msg.frameCounter,
    pixelsU8: new Uint8Array(msg.pixels),
  };
}

