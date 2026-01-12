export type Rect = { x: number; y: number; w: number; h: number };

export type PackedRect = {
  /** The rect clamped to `srcWidth/srcHeight`. */
  x: number;
  y: number;
  w: number;
  h: number;
  /** WebGPU-aligned `bytesPerRow` (multiple of 256). */
  bytesPerRow: number;
  /** Total bytes written into the returned staging buffer. */
  byteLength: number;
};

function alignUp(value: number, alignment: number): number {
  if (alignment <= 0) throw new Error(`alignment must be > 0; got ${alignment}`);
  return Math.ceil(value / alignment) * alignment;
}

/**
 * Clamp a possibly-out-of-bounds rect to `[0, srcWidth) x [0, srcHeight)`.
 *
 * Returns `null` when the rect does not intersect the bounds.
 */
export function clampRect(rect: Rect, srcWidth: number, srcHeight: number): Rect | null {
  const x0 = rect.x | 0;
  const y0 = rect.y | 0;
  const w0 = rect.w | 0;
  const h0 = rect.h | 0;
  if (w0 <= 0 || h0 <= 0) return null;

  const left = Math.max(0, x0);
  const top = Math.max(0, y0);
  const right = Math.min(srcWidth, x0 + w0);
  const bottom = Math.min(srcHeight, y0 + h0);

  const w = Math.max(0, right - left);
  const h = Math.max(0, bottom - top);
  if (w === 0 || h === 0) return null;
  return { x: left, y: top, w, h };
}

/**
 * Pack an RGBA8 sub-rect into a WebGPU-compatible staging buffer.
 *
 * WebGPU requires `bytesPerRow` to be a multiple of 256; most dirty rect widths
 * are not, so we repack each rect into a scratch buffer with padded rows.
 */
export function packRgba8RectToAlignedBuffer(
  src: Uint8Array,
  srcStrideBytes: number,
  srcWidth: number,
  srcHeight: number,
  rect: Rect,
  staging: Uint8Array | null,
  out: PackedRect,
): Uint8Array | null {
  const x0 = rect.x | 0;
  const y0 = rect.y | 0;
  const w0 = rect.w | 0;
  const h0 = rect.h | 0;
  if (w0 <= 0 || h0 <= 0) return null;

  const left = Math.max(0, x0);
  const top = Math.max(0, y0);
  const right = Math.min(srcWidth, x0 + w0);
  const bottom = Math.min(srcHeight, y0 + h0);

  const w = Math.max(0, right - left);
  const h = Math.max(0, bottom - top);
  if (w === 0 || h === 0) return null;

  out.x = left;
  out.y = top;
  out.w = w;
  out.h = h;

  const rowBytes = w * 4;
  const bytesPerRow = alignUp(rowBytes, 256);
  const total = bytesPerRow * h;

  let buffer = staging;
  if (!buffer || buffer.byteLength < total) {
    buffer = new Uint8Array(total);
  }

  for (let row = 0; row < h; row += 1) {
    const srcOff = (top + row) * srcStrideBytes + left * 4;
    const dstOff = row * bytesPerRow;
    buffer.set(src.subarray(srcOff, srcOff + rowBytes), dstOff);
    buffer.fill(0, dstOff + rowBytes, dstOff + bytesPerRow);
  }

  out.bytesPerRow = bytesPerRow;
  out.byteLength = total;
  return buffer;
}
