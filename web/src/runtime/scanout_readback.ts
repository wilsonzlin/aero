import { SCANOUT_FORMAT_B8G8R8X8 } from "../ipc/scanout_state";
import { guestPaddrToRamOffset, guestRangeInBounds, type GuestRamLayout } from "./shared_layout";

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
  rgba8: Uint8Array;
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
 * Convert a guest BGRX scanout buffer (WDDM-style `B8G8R8X8`) into a packed RGBA8 buffer.
 *
 * This is a pure helper intended for unit tests and screenshot/present paths.
 * It:
 * - Validates the scanout descriptor
 * - Handles padded row pitch (pitchBytes >= width*4)
 * - Forces alpha=255 (X8 -> opaque)
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

  if (format !== SCANOUT_FORMAT_B8G8R8X8) {
    throw new Error(`Unsupported scanout format ${format} (expected B8G8R8X8=${SCANOUT_FORMAT_B8G8R8X8})`);
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

  const layout: GuestRamLayout = {
    guest_base: 0,
    guest_size: guestRam.byteLength,
    runtime_reserved: 0,
    wasm_pages: 0,
  };

  const basePaddr = toU64Bigint(desc.basePaddr, "basePaddr");
  const pitchBig = BigInt(pitchBytes);

  for (let y = 0; y < height; y += 1) {
    const rowPaddrBig = basePaddr + BigInt(y) * pitchBig;
    const rowPaddr = u64BigintToSafeNumber(rowPaddrBig, "scanout row paddr");

    if (!guestRangeInBounds(layout, rowPaddr, rowBytes)) {
      throw new RangeError(
        `scanout row is out of bounds: basePaddr=0x${basePaddr.toString(16)} y=${y} rowPaddr=0x${rowPaddrBig.toString(16)} rowBytes=0x${rowBytes.toString(16)} guest_size=0x${guestRam.byteLength.toString(16)}`,
      );
    }

    const rowOff = guestPaddrToRamOffset(layout, rowPaddr);
    if (rowOff === null) {
      throw new RangeError(
        `scanout row base_paddr is not backed by RAM: 0x${rowPaddrBig.toString(16)} (guest_size=0x${guestRam.byteLength.toString(16)})`,
      );
    }

    const src = guestRam.subarray(rowOff, rowOff + rowBytes);
    let srcOff = 0;
    let dstOff = y * rowBytes;
    for (let x = 0; x < width; x += 1) {
      const b = src[srcOff + 0]!;
      const g = src[srcOff + 1]!;
      const r = src[srcOff + 2]!;
      // src[srcOff+3] is X8 (ignored); force opaque alpha.
      rgba8[dstOff + 0] = r;
      rgba8[dstOff + 1] = g;
      rgba8[dstOff + 2] = b;
      rgba8[dstOff + 3] = 255;
      srcOff += 4;
      dstOff += 4;
    }
  }

  return { width, height, rgba8 };
}

