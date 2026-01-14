import {
  SCANOUT_FORMAT_B8G8R8A8,
  SCANOUT_FORMAT_B8G8R8A8_SRGB,
  SCANOUT_FORMAT_B8G8R8X8,
  SCANOUT_FORMAT_B8G8R8X8_SRGB,
  SCANOUT_FORMAT_B5G5R5A1,
  SCANOUT_FORMAT_B5G6R5,
  SCANOUT_FORMAT_R8G8B8A8,
  SCANOUT_FORMAT_R8G8B8A8_SRGB,
  SCANOUT_FORMAT_R8G8B8X8,
  SCANOUT_FORMAT_R8G8B8X8_SRGB,
} from "../ipc/scanout_state";
import { guestPaddrToRamOffset, guestRangeInBounds } from "../arch/guest_ram_translate.ts";
import { aerogpuFormatToString } from "../../../emulator/protocol/aerogpu/aerogpu_pci.ts";
import { convertScanoutToRgba8, type ScanoutSwizzleKind } from "../workers/scanout_swizzle.ts";

export type ScanoutDescriptor = Readonly<{
  /**
   * Guest physical base address (may be >= 4GiB when the VM uses the Q35 high-RAM remap).
   */
  basePaddr: bigint | number;
  width: number;
  height: number;
  pitchBytes: number;
  format: number;
}>;

export type ScanoutReadbackResult = Readonly<{
  width: number;
  height: number;
  // `readScanoutRgba8FromGuestRam` always allocates a fresh, transferable buffer.
  // Model the backing as an `ArrayBuffer` (not `ArrayBufferLike`) so callers can safely
  // `postMessage(..., [rgba8.buffer])` without extra casting.
  rgba8: Uint8Array<ArrayBuffer>;
}>;

function ensureArrayBufferBacked(bytes: Uint8Array): Uint8Array<ArrayBuffer> {
  if (bytes.buffer instanceof ArrayBuffer) return bytes as unknown as Uint8Array<ArrayBuffer>;
  const buf = new ArrayBuffer(bytes.byteLength);
  const out = new Uint8Array(buf);
  out.set(bytes);
  return out;
}

// Upper bound used by screenshot + presentation readback paths to prevent untrusted/corrupt
// scanout descriptors from attempting absurd allocations inside the GPU worker.
//
// NOTE: This is a *safety* limit, not a correctness limit. Callers should treat oversized
// descriptors as "no frame" rather than a fatal error.
export const MAX_SCANOUT_RGBA8_BYTES = 256 * 1024 * 1024;

const MAX_SAFE_U64_BIGINT = BigInt(Number.MAX_SAFE_INTEGER);
const MAX_SCANOUT_READBACK_BYTES = 256 * 1024 * 1024;

const toU32 = (value: number, label: string): number => {
  if (!Number.isFinite(value)) {
    throw new RangeError(`${label} must be a finite number, got ${String(value)}`);
  }
  const int = Math.trunc(value);
  if (int < 0 || int > 0xffff_ffff) {
    throw new RangeError(`${label} must be a u32 in [0, 2^32), got ${String(value)}`);
  }
  return int >>> 0;
};

const toU64Bigint = (value: bigint | number, label: string): bigint => {
  if (typeof value === "bigint") {
    if (value < 0n) {
      throw new RangeError(`${label} must be >= 0, got ${value.toString()}`);
    }
    return value;
  }

  if (!Number.isFinite(value)) {
    throw new RangeError(`${label} must be a finite number, got ${String(value)}`);
  }
  const int = Math.trunc(value);
  if (int < 0 || int > Number.MAX_SAFE_INTEGER) {
    throw new RangeError(`${label} must be an integer in [0, 2^53), got ${String(value)}`);
  }
  return BigInt(int);
};

const u64BigintToSafeNumber = (value: bigint, label: string): number => {
  if (value < 0n) {
    throw new RangeError(`${label} must be >= 0, got ${value.toString()}`);
  }
  if (value > MAX_SAFE_U64_BIGINT) {
    throw new RangeError(`${label} exceeds JS safe integer range: 0x${value.toString(16)}`);
  }
  return Number(value);
};

/**
 * Compute the number of bytes required for a tightly packed RGBA8 buffer (`width*height*4`).
 *
 * Returns `null` when:
 * - width/height are invalid
 * - the computed size exceeds `maxBytes`
 *
 * Uses BigInt math to avoid overflow/precision loss for untrusted `u32` inputs.
 */
export function tryComputeScanoutRgba8ByteLength(
  width: number,
  height: number,
  maxBytes: number = MAX_SCANOUT_RGBA8_BYTES,
): number | null {
  if (!Number.isFinite(width) || !Number.isFinite(height) || !Number.isFinite(maxBytes)) return null;
  const w = Math.trunc(width);
  const h = Math.trunc(height);
  if (w <= 0 || h <= 0) return null;
  if (w > 0xffff_ffff || h > 0xffff_ffff) return null;
  if (maxBytes <= 0) return null;

  const required = BigInt(w) * BigInt(h) * 4n;
  if (required > BigInt(maxBytes)) return null;
  return Number(required);
}

/**
 * Convert a guest scanout buffer into a packed RGBA8 buffer.
 *
 * Supported formats:
 * - 32bpp packed: BGRA/BGRX/RGBA/RGBX (incl. sRGB variants)
 * - 16bpp packed: B5G6R5 (opaque) and B5G5R5A1 (1-bit alpha)
 *
 * Notes:
 * - `BGRX` / `RGBX` formats treat the unused `X` byte as fully opaque (`A=0xFF`) when converting to RGBA.
 * - sRGB variants are swizzled identically; this helper does not perform any gamma conversion.
 *   Callers that blend/present in linear space should decode sRGB→linear after swizzle.
 *
 * This is a pure helper intended for unit tests and screenshot/present paths.
 * It:
 * - Validates the scanout descriptor
 * - Handles padded row pitch (pitchBytes >= width*4)
 * - Translates guest physical addresses (including the Q35 high-RAM remap)
 */
export function readScanoutRgba8FromGuestRam(
  guestRam: Uint8Array,
  desc: ScanoutDescriptor,
  dst?: Uint8Array<ArrayBuffer> | null,
): ScanoutReadbackResult {
  if (!(guestRam instanceof Uint8Array)) {
    throw new TypeError("guestRam must be a Uint8Array");
  }

  const width = toU32(desc.width, "width");
  const height = toU32(desc.height, "height");
  const pitchBytes = toU32(desc.pitchBytes, "pitchBytes");
  const format = toU32(desc.format, "format");

  let kind: ScanoutSwizzleKind | null = null;
  let srcBytesPerPixel: number;
  let isB5G6R5 = false;
  let isB5G5R5A1 = false;
  switch (format) {
    case SCANOUT_FORMAT_B8G8R8X8:
    case SCANOUT_FORMAT_B8G8R8X8_SRGB:
      kind = "bgrx";
      srcBytesPerPixel = 4;
      break;
    case SCANOUT_FORMAT_B8G8R8A8:
    case SCANOUT_FORMAT_B8G8R8A8_SRGB:
      kind = "bgra";
      srcBytesPerPixel = 4;
      break;
    case SCANOUT_FORMAT_R8G8B8A8:
    case SCANOUT_FORMAT_R8G8B8A8_SRGB:
      kind = "rgba";
      srcBytesPerPixel = 4;
      break;
    case SCANOUT_FORMAT_R8G8B8X8:
    case SCANOUT_FORMAT_R8G8B8X8_SRGB:
      kind = "rgbx";
      srcBytesPerPixel = 4;
      break;
    case SCANOUT_FORMAT_B5G6R5:
      srcBytesPerPixel = 2;
      isB5G6R5 = true;
      break;
    case SCANOUT_FORMAT_B5G5R5A1:
      srcBytesPerPixel = 2;
      isB5G5R5A1 = true;
      break;
    default:
      throw new Error(`Unsupported scanout format ${aerogpuFormatToString(format)}`);
  }

  if (width === 0 || height === 0) {
    return { width, height, rgba8: new Uint8Array(new ArrayBuffer(0)) };
  }

  const srcRowBytes = width * srcBytesPerPixel;
  if (!Number.isSafeInteger(srcRowBytes)) {
    throw new RangeError(`scanout row size exceeds JS safe integer range: width=${width}`);
  }

  if (pitchBytes < srcRowBytes) {
    throw new RangeError(
      `scanout pitchBytes is too small: pitchBytes=${pitchBytes} < width*${srcBytesPerPixel}=${srcRowBytes}`,
    );
  }

  const rowBytes = width * 4;
  const totalBytes = rowBytes * height;
  if (!Number.isSafeInteger(totalBytes)) {
    throw new RangeError(`scanout output size exceeds JS safe integer range: ${width}x${height}`);
  }
  // Avoid attempting absurd allocations if the descriptor is corrupt/malicious.
  if (totalBytes > MAX_SCANOUT_RGBA8_BYTES) {
    throw new RangeError(`scanout output size exceeds cap (${MAX_SCANOUT_RGBA8_BYTES} bytes): ${width}x${height}`);
  }
  const rgba8Candidate = dst && dst.byteLength >= totalBytes ? dst.subarray(0, totalBytes) : new Uint8Array(totalBytes);
  const rgba8 = ensureArrayBufferBacked(rgba8Candidate);

  const basePaddr = toU64Bigint(desc.basePaddr, "basePaddr");
  const pitchBig = BigInt(pitchBytes);

  // Fast path: the whole scanout surface is backed by contiguous guest RAM (does not cross PCI holes).
  //
  // In this case we can translate `basePaddr` once and swizzle the full buffer without
  // per-row address translation overhead.
  //
  // Note: the scanout surface occupies `srcRowBytes` bytes on the last row, not the full `pitchBytes`.
  // This matches typical linear framebuffer semantics and avoids requiring unused pitch padding bytes
  // after the final row.
  //
  // For 32bpp formats, `srcRowBytes === rowBytes`. For 16bpp formats, the scanout only needs
  // `srcRowBytes` bytes on the last row (not the expanded RGBA row size).
  const requiredSrcBytesBig = pitchBig * (BigInt(height) - 1n) + BigInt(srcRowBytes);
  if (requiredSrcBytesBig > MAX_SAFE_U64_BIGINT) {
    throw new RangeError(`scanout buffer size exceeds JS safe integer range: pitchBytes=${pitchBytes} height=${height}`);
  }
  const requiredSrcBytes = Number(requiredSrcBytesBig);
  const basePaddrNum = u64BigintToSafeNumber(basePaddr, "basePaddr");

  if (guestRangeInBounds(guestRam.byteLength, basePaddrNum, requiredSrcBytes)) {
    const ramOffset = guestPaddrToRamOffset(guestRam.byteLength, basePaddrNum);
    if (ramOffset === null) {
      throw new RangeError(
        `scanout base_paddr is not backed by RAM: 0x${basePaddr.toString(16)} (guest_size=0x${guestRam.byteLength.toString(16)})`,
      );
    }
    const end = ramOffset + requiredSrcBytes;
    if (end < ramOffset || end > guestRam.byteLength) {
      throw new RangeError(
        `scanout buffer is out of bounds: basePaddr=0x${basePaddr.toString(16)} bytes=0x${requiredSrcBytes.toString(16)} guest_size=0x${guestRam.byteLength.toString(16)}`,
      );
    }

    const src = guestRam.subarray(ramOffset, end);
    if (kind) {
      convertScanoutToRgba8({
        src,
        srcStrideBytes: pitchBytes,
        dst: rgba8,
        dstStrideBytes: rowBytes,
        width,
        height,
        kind,
      });
    } else if (isB5G6R5 || isB5G5R5A1) {
      for (let y = 0; y < height; y += 1) {
        let srcOff = y * pitchBytes;
        let dstOff = y * rowBytes;
        for (let x = 0; x < width; x += 1) {
          const lo = src[srcOff++]!;
          const hi = src[srcOff++]!;
          const pix = (lo | (hi << 8)) >>> 0;
          if (isB5G6R5) {
            const b = pix & 0x1f;
            const g = (pix >>> 5) & 0x3f;
            const r = (pix >>> 11) & 0x1f;
            const r8 = ((r << 3) | (r >>> 2)) & 0xff;
            const g8 = ((g << 2) | (g >>> 4)) & 0xff;
            const b8 = ((b << 3) | (b >>> 2)) & 0xff;
            rgba8[dstOff + 0] = r8;
            rgba8[dstOff + 1] = g8;
            rgba8[dstOff + 2] = b8;
            rgba8[dstOff + 3] = 0xff;
          } else {
            const b = pix & 0x1f;
            const g = (pix >>> 5) & 0x1f;
            const r = (pix >>> 10) & 0x1f;
            const a = (pix >>> 15) & 0x1;
            const r8 = ((r << 3) | (r >>> 2)) & 0xff;
            const g8 = ((g << 3) | (g >>> 2)) & 0xff;
            const b8 = ((b << 3) | (b >>> 2)) & 0xff;
            rgba8[dstOff + 0] = r8;
            rgba8[dstOff + 1] = g8;
            rgba8[dstOff + 2] = b8;
            rgba8[dstOff + 3] = a ? 0xff : 0;
          }
          dstOff += 4;
        }
      }
    }
    return { width, height, rgba8 };
  }

  for (let y = 0; y < height; y += 1) {
    const rowPaddrBig = basePaddr + BigInt(y) * pitchBig;
    const rowPaddr = u64BigintToSafeNumber(rowPaddrBig, "scanout row paddr");

    if (!guestRangeInBounds(guestRam.byteLength, rowPaddr, srcRowBytes)) {
      throw new RangeError(
        `scanout row is out of bounds: basePaddr=0x${basePaddr.toString(16)} y=${y} rowPaddr=0x${rowPaddrBig.toString(16)} rowBytes=0x${srcRowBytes.toString(16)} guest_size=0x${guestRam.byteLength.toString(16)}`,
      );
    }

    const rowOff = guestPaddrToRamOffset(guestRam.byteLength, rowPaddr);
    if (rowOff === null) {
      throw new RangeError(
        `scanout row base_paddr is not backed by RAM: 0x${rowPaddrBig.toString(16)} (guest_size=0x${guestRam.byteLength.toString(16)})`,
      );
    }

    const srcRow = guestRam.subarray(rowOff, rowOff + srcRowBytes);
    const dstRow = rgba8.subarray(y * rowBytes, y * rowBytes + rowBytes);
    if (kind) {
      convertScanoutToRgba8({
        src: srcRow,
        srcStrideBytes: srcRowBytes,
        dst: dstRow,
        dstStrideBytes: rowBytes,
        width,
        height: 1,
        kind,
      });
    } else if (isB5G6R5 || isB5G5R5A1) {
      let srcOff = 0;
      let dstOff = 0;
      for (let x = 0; x < width; x += 1) {
        const lo = srcRow[srcOff++]!;
        const hi = srcRow[srcOff++]!;
        const pix = (lo | (hi << 8)) >>> 0;
        if (isB5G6R5) {
          const b = pix & 0x1f;
          const g = (pix >>> 5) & 0x3f;
          const r = (pix >>> 11) & 0x1f;
          const r8 = ((r << 3) | (r >>> 2)) & 0xff;
          const g8 = ((g << 2) | (g >>> 4)) & 0xff;
          const b8 = ((b << 3) | (b >>> 2)) & 0xff;
          dstRow[dstOff + 0] = r8;
          dstRow[dstOff + 1] = g8;
          dstRow[dstOff + 2] = b8;
          dstRow[dstOff + 3] = 0xff;
        } else {
          const b = pix & 0x1f;
          const g = (pix >>> 5) & 0x1f;
          const r = (pix >>> 10) & 0x1f;
          const a = (pix >>> 15) & 0x1;
          const r8 = ((r << 3) | (r >>> 2)) & 0xff;
          const g8 = ((g << 3) | (g >>> 2)) & 0xff;
          const b8 = ((b << 3) | (b >>> 2)) & 0xff;
          dstRow[dstOff + 0] = r8;
          dstRow[dstOff + 1] = g8;
          dstRow[dstOff + 2] = b8;
          dstRow[dstOff + 3] = a ? 0xff : 0;
        }
        dstOff += 4;
      }
    }
  }

  return { width, height, rgba8 };
}
