/**
 * Pixel conversion helpers for ScanoutState-presented framebuffers.
 *
 * ScanoutState currently uses a VBE-compatible 32bpp pixel layout:
 *   - Memory bytes: B8 G8 R8 X8 (little-endian)
 *   - Presented bytes: R8 G8 B8 A8 (alpha forced to 0xFF)
 */

export function convertB8G8R8X8ToRgba8(
  src: Uint8Array,
  srcPitchBytes: number,
  width: number,
  height: number,
  dst: Uint8Array,
): void {
  const w = width | 0;
  const h = height | 0;
  if (w <= 0 || h <= 0) {
    return;
  }

  const rowBytes = w * 4;
  if (srcPitchBytes < rowBytes) {
    throw new RangeError(`srcPitchBytes (${srcPitchBytes}) < width*4 (${rowBytes})`);
  }

  const requiredSrcBytes = srcPitchBytes * h;
  if (requiredSrcBytes > src.byteLength) {
    throw new RangeError(`src byteLength (${src.byteLength}) < required (${requiredSrcBytes})`);
  }

  const requiredDstBytes = rowBytes * h;
  if (requiredDstBytes > dst.byteLength) {
    throw new RangeError(`dst byteLength (${dst.byteLength}) < required (${requiredDstBytes})`);
  }

  let di = 0;
  for (let y = 0; y < h; y += 1) {
    let si = y * srcPitchBytes;
    for (let x = 0; x < w; x += 1) {
      const b = src[si + 0] ?? 0;
      const g = src[si + 1] ?? 0;
      const r = src[si + 2] ?? 0;
      dst[di + 0] = r;
      dst[di + 1] = g;
      dst[di + 2] = b;
      dst[di + 3] = 0xff;
      si += 4;
      di += 4;
    }
  }
}

