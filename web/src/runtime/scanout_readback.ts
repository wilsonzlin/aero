import {
  SCANOUT_FORMAT_B8G8R8A8,
  SCANOUT_FORMAT_B8G8R8A8_SRGB,
  SCANOUT_FORMAT_B8G8R8X8,
  SCANOUT_FORMAT_B8G8R8X8_SRGB,
} from "../ipc/scanout_state";
import { guestPaddrToRamOffset, guestRangeInBounds } from "../arch/guest_ram_translate.ts";
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
  // The readback buffer is always owned by the caller and transferred across workers,
  // so it must be backed by a non-shared ArrayBuffer (not SharedArrayBuffer).
  rgba8: Uint8Array<ArrayBuffer>;
}>;

const MAX_SAFE_U64_BIGINT = BigInt(Number.MAX_SAFE_INTEGER);

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
 * Convert a guest scanout buffer into a packed RGBA8 buffer.
 *
 * Supported scanout formats:
 * - `B8G8R8X8` -> RGBA8 (alpha forced to 255)
 * - `B8G8R8A8` -> RGBA8 (alpha preserved)
 *
 * This is a pure helper intended for unit tests and screenshot/present paths.
 * It:
 * - Validates the scanout descriptor
 * - Handles padded row pitch (pitchBytes >= width*4)
 * - Translates guest physical addresses (including the Q35 high-RAM remap)
 */
export function readScanoutRgba8FromGuestRam(guestRam: Uint8Array, desc: ScanoutDescriptor): ScanoutReadbackResult {
  if (!(guestRam instanceof Uint8Array)) {
    throw new TypeError("guestRam must be a Uint8Array");
  }

  const width = toU32(desc.width, "width");
  const height = toU32(desc.height, "height");
  const pitchBytes = toU32(desc.pitchBytes, "pitchBytes");
  const format = toU32(desc.format, "format");

  let kind: ScanoutSwizzleKind;
  if (format === SCANOUT_FORMAT_B8G8R8X8 || format === SCANOUT_FORMAT_B8G8R8X8_SRGB) {
    kind = "bgrx";
  } else if (format === SCANOUT_FORMAT_B8G8R8A8 || format === SCANOUT_FORMAT_B8G8R8A8_SRGB) {
    kind = "bgra";
  } else {
    throw new Error(
      `Unsupported scanout format ${format} (expected ` +
        `B8G8R8X8=${SCANOUT_FORMAT_B8G8R8X8}, ` +
        `B8G8R8A8=${SCANOUT_FORMAT_B8G8R8A8}, ` +
        `B8G8R8X8_SRGB=${SCANOUT_FORMAT_B8G8R8X8_SRGB}, ` +
        `or B8G8R8A8_SRGB=${SCANOUT_FORMAT_B8G8R8A8_SRGB})`,
    );
  }

  if (width === 0 || height === 0) {
    return { width, height, rgba8: new Uint8Array(0) };
  }

  const rowBytes = width * 4;
  if (!Number.isSafeInteger(rowBytes)) {
    throw new RangeError(`scanout row size exceeds JS safe integer range: width=${width}`);
  }

  if (pitchBytes < rowBytes) {
    throw new RangeError(`scanout pitchBytes is too small: pitchBytes=${pitchBytes} < width*4=${rowBytes}`);
  }
  if (pitchBytes % 4 !== 0) {
    throw new RangeError(`scanout pitchBytes must be a multiple of 4 (got ${pitchBytes})`);
  }

  const totalBytes = rowBytes * height;
  if (!Number.isSafeInteger(totalBytes)) {
    throw new RangeError(`scanout output size exceeds JS safe integer range: ${width}x${height}`);
  }
  const rgba8 = new Uint8Array(totalBytes);

  const basePaddr = toU64Bigint(desc.basePaddr, "basePaddr");
  const pitchBig = BigInt(pitchBytes);

  // Fast path: the whole scanout surface is backed by contiguous guest RAM (does not cross PCI holes).
  //
  // In this case we can translate `basePaddr` once and swizzle the full buffer without
  // per-row address translation overhead.
  const requiredSrcBytesBig = pitchBig * BigInt(height);
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
    convertScanoutToRgba8({
      src,
      srcStrideBytes: pitchBytes,
      dst: rgba8,
      dstStrideBytes: rowBytes,
      width,
      height,
      kind,
    });
    return { width, height, rgba8 };
  }

  for (let y = 0; y < height; y += 1) {
    const rowPaddrBig = basePaddr + BigInt(y) * pitchBig;
    const rowPaddr = u64BigintToSafeNumber(rowPaddrBig, "scanout row paddr");

    if (!guestRangeInBounds(guestRam.byteLength, rowPaddr, rowBytes)) {
      throw new RangeError(
        `scanout row is out of bounds: basePaddr=0x${basePaddr.toString(16)} y=${y} rowPaddr=0x${rowPaddrBig.toString(16)} rowBytes=0x${rowBytes.toString(16)} guest_size=0x${guestRam.byteLength.toString(16)}`,
      );
    }

    const rowOff = guestPaddrToRamOffset(guestRam.byteLength, rowPaddr);
    if (rowOff === null) {
      throw new RangeError(
        `scanout row base_paddr is not backed by RAM: 0x${rowPaddrBig.toString(16)} (guest_size=0x${guestRam.byteLength.toString(16)})`,
      );
    }

    const srcRow = guestRam.subarray(rowOff, rowOff + rowBytes);
    const dstRow = rgba8.subarray(y * rowBytes, y * rowBytes + rowBytes);
    convertScanoutToRgba8({
      src: srcRow,
      srcStrideBytes: rowBytes,
      dst: dstRow,
      dstStrideBytes: rowBytes,
      width,
      height: 1,
      kind,
    });
  }

  return { width, height, rgba8 };
}
