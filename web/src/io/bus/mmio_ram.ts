import { defaultReadValue } from "../ipc/io_protocol.ts";
import type { MmioHandler } from "./mmio.ts";

export class MmioRamHandler implements MmioHandler {
  readonly #u8: Uint8Array;
  readonly #view: DataView;

  constructor(bytes: Uint8Array) {
    this.#u8 = bytes;
    this.#view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  }

  mmioRead(offset: bigint, size: number): number {
    if (offset < 0n) return defaultReadValue(size);
    if (offset > BigInt(Number.MAX_SAFE_INTEGER)) return defaultReadValue(size);
    const off = Number(offset);
    if (!Number.isSafeInteger(off)) return defaultReadValue(size);
    if (off < 0) return defaultReadValue(size);

    const end = off + size;
    if (end > this.#u8.byteLength) return defaultReadValue(size);

    if (size === 1) return this.#u8[off]! & 0xff;
    if (size === 2) return this.#view.getUint16(off, true) & 0xffff;
    if (size === 4) return this.#view.getUint32(off, true) >>> 0;

    // Unsupported read sizes behave like unmapped MMIO.
    return defaultReadValue(size);
  }

  mmioWrite(offset: bigint, size: number, value: number): void {
    if (offset < 0n) return;
    if (offset > BigInt(Number.MAX_SAFE_INTEGER)) return;
    const off = Number(offset);
    if (!Number.isSafeInteger(off)) return;
    if (off < 0) return;

    const end = off + size;
    if (end > this.#u8.byteLength) return;

    if (size === 1) {
      this.#u8[off] = value & 0xff;
      return;
    }
    if (size === 2) {
      this.#view.setUint16(off, value & 0xffff, true);
      return;
    }
    if (size === 4) {
      this.#view.setUint32(off, value >>> 0, true);
      return;
    }

    // Ignore unsupported write sizes.
  }
}

