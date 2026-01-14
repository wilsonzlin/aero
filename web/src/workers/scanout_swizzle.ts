/**
 * Fast scanout swizzles used when presenting WDDM scanout surfaces.
 *
 * The guest scanout formats are little-endian:
 * - BGRX: [B, G, R, X] in memory => u32 = 0xXXRRGGBB
 * - BGRA: [B, G, R, A] in memory => u32 = 0xAARRGGBB
 *
 * The presenters expect RGBA bytes: [R, G, B, A] in memory => u32 = 0xAABBGGRR
 *
 * These helpers operate on the u32 representation to avoid per-byte shuffles in JS.
 */

export type ScanoutSwizzleKind = "bgrx" | "bgra" | "rgba" | "rgbx";

/**
 * Swizzle a little-endian BGRX pixel (0xXXRRGGBB) to RGBA (0xFFBBGGRR).
 */
export const swizzleBgrxToRgba32 = (v: number): number =>
  (0xff00_0000 | ((v & 0x00ff_0000) >>> 16) | (v & 0x0000_ff00) | ((v & 0x0000_00ff) << 16)) >>> 0;

/**
 * Swizzle a little-endian BGRA pixel (0xAARRGGBB) to RGBA (0xAABBGGRR).
 */
export const swizzleBgraToRgba32 = (v: number): number =>
  (((v & 0xff00_0000) | ((v & 0x00ff_0000) >>> 16) | (v & 0x0000_ff00) | ((v & 0x0000_00ff) << 16)) >>> 0);

export type ConvertScanoutOptions = {
  /** Source pixels as a byte view (may be unaligned). */
  src: Uint8Array;
  /** Source stride in bytes. */
  srcStrideBytes: number;
  /** Destination pixel buffer (RGBA8). */
  dst: Uint8Array;
  /** Destination stride in bytes. */
  dstStrideBytes: number;
  width: number;
  height: number;
  kind: ScanoutSwizzleKind;
};

/**
 * Convert a BGRX/BGRA/RGBA/RGBX scanout into RGBA8.
 *
 * Returns `true` if the u32 fast path was used.
 */
export function convertScanoutToRgba8(opts: ConvertScanoutOptions): boolean {
  const width = opts.width | 0;
  const height = opts.height | 0;
  const srcStrideBytes = opts.srcStrideBytes | 0;
  const dstStrideBytes = opts.dstStrideBytes | 0;

  if (width <= 0 || height <= 0) return false;
  if (srcStrideBytes < width * 4 || dstStrideBytes < width * 4) return false;

  const src = opts.src;
  const dst = opts.dst;

  // Fast path: require both the base offset and the per-row stride to be 4-byte aligned so
  // every row can be addressed as a Uint32Array without misaligned accesses.
  const canU32 =
    (src.byteOffset & 3) === 0 &&
    (dst.byteOffset & 3) === 0 &&
    (srcStrideBytes & 3) === 0 &&
    (dstStrideBytes & 3) === 0;

  if (canU32) {
    const srcWordsPerRow = srcStrideBytes >>> 2;
    const dstWordsPerRow = dstStrideBytes >>> 2;

    const srcU32 = new Uint32Array(src.buffer, src.byteOffset, srcWordsPerRow * height);
    const dstU32 = new Uint32Array(dst.buffer, dst.byteOffset, dstWordsPerRow * height);

    switch (opts.kind) {
      case "rgba":
        for (let y = 0; y < height; y += 1) {
          let srcIdx = y * srcWordsPerRow;
          let dstIdx = y * dstWordsPerRow;
          const rowEnd = dstIdx + width;
          while (dstIdx < rowEnd) {
            // RGBA u32 is already in the destination format.
            dstU32[dstIdx++] = srcU32[srcIdx++]!;
          }
        }
        break;
      case "rgbx":
        for (let y = 0; y < height; y += 1) {
          let srcIdx = y * srcWordsPerRow;
          let dstIdx = y * dstWordsPerRow;
          const rowEnd = dstIdx + width;
          while (dstIdx < rowEnd) {
            // RGBX u32 = 0xXXBBGGRR -> RGBA u32 = 0xFFBBGGRR.
            dstU32[dstIdx++] = (srcU32[srcIdx++]! | 0xff00_0000) >>> 0;
          }
        }
        break;
      case "bgra":
        for (let y = 0; y < height; y += 1) {
          let srcIdx = y * srcWordsPerRow;
          let dstIdx = y * dstWordsPerRow;
          const rowEnd = dstIdx + width;
          while (dstIdx < rowEnd) {
            const v = srcU32[srcIdx++]!;
            // BGRA u32 = 0xAARRGGBB -> RGBA u32 = 0xAABBGGRR
            dstU32[dstIdx++] =
              (v & 0xff00_0000) | ((v & 0x00ff_0000) >>> 16) | (v & 0x0000_ff00) | ((v & 0x0000_00ff) << 16);
          }
        }
        break;
      case "bgrx":
        for (let y = 0; y < height; y += 1) {
          let srcIdx = y * srcWordsPerRow;
          let dstIdx = y * dstWordsPerRow;
          const rowEnd = dstIdx + width;
          while (dstIdx < rowEnd) {
            const v = srcU32[srcIdx++]!;
            // BGRX u32 = 0xXXRRGGBB -> RGBA u32 = 0xFFBBGGRR
            dstU32[dstIdx++] =
              0xff00_0000 | ((v & 0x00ff_0000) >>> 16) | (v & 0x0000_ff00) | ((v & 0x0000_00ff) << 16);
          }
        }
        break;
    }
    return true;
  }

  // Safe fallback: byte-wise shuffle. This works for unaligned bases/strides.
  const swapRb = opts.kind === "bgrx" || opts.kind === "bgra";
  const preserveAlpha = opts.kind === "bgra" || opts.kind === "rgba";
  for (let y = 0; y < height; y += 1) {
    let srcOff = y * srcStrideBytes;
    let dstOff = y * dstStrideBytes;
    for (let x = 0; x < width; x += 1) {
      const c0 = src[srcOff + 0]!;
      const c1 = src[srcOff + 1]!;
      const c2 = src[srcOff + 2]!;
      const r = swapRb ? c2 : c0;
      const g = c1;
      const b = swapRb ? c0 : c2;
      const a = preserveAlpha ? src[srcOff + 3]! : 0xff;
      dst[dstOff + 0] = r;
      dst[dstOff + 1] = g;
      dst[dstOff + 2] = b;
      dst[dstOff + 3] = a;
      srcOff += 4;
      dstOff += 4;
    }
  }
  return false;
}
