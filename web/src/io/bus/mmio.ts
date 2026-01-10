import { defaultReadValue } from "../ipc/io_protocol.ts";

export interface MmioHandler {
  mmioRead(offset: bigint, size: number): number;
  mmioWrite(offset: bigint, size: number, value: number): void;
}

export type MmioHandle = number;

interface MmioRegion {
  handle: MmioHandle;
  base: bigint;
  size: bigint;
  handler: MmioHandler;
}

export class MmioBus {
  #regions: MmioRegion[] = [];
  #nextHandle: MmioHandle = 1;

  mapRange(base: bigint, size: bigint, handler: MmioHandler): MmioHandle {
    if (size <= 0n) throw new RangeError(`MMIO size must be > 0, got ${size}`);
    const end = base + size;
    if (end <= base) throw new RangeError(`MMIO range overflow: base=${base} size=${size}`);

    // Ensure no overlap; keep sorted by base.
    for (const region of this.#regions) {
      const rEnd = region.base + region.size;
      const overlaps = base < rEnd && end > region.base;
      if (overlaps) {
        throw new Error(`MMIO range [${base}, ${end}) overlaps existing [${region.base}, ${rEnd})`);
      }
    }

    const handle = this.#nextHandle++;
    this.#regions.push({ handle, base, size, handler });
    this.#regions.sort((a, b) => (a.base < b.base ? -1 : a.base > b.base ? 1 : 0));
    return handle;
  }

  unmap(handle: MmioHandle): void {
    this.#regions = this.#regions.filter((r) => r.handle !== handle);
  }

  read(addr: bigint, size: number): number {
    const region = this.#findRegion(addr);
    if (!region) return defaultReadValue(size);
    const offset = addr - region.base;
    return region.handler.mmioRead(offset, size) >>> 0;
  }

  write(addr: bigint, size: number, value: number): void {
    const region = this.#findRegion(addr);
    if (!region) return;
    const offset = addr - region.base;
    region.handler.mmioWrite(offset, size, value >>> 0);
  }

  #findRegion(addr: bigint): MmioRegion | null {
    // Linear scan is fine for now; number of regions is small. Can be upgraded
    // to binary search when devices grow.
    for (const region of this.#regions) {
      if (addr >= region.base && addr < region.base + region.size) return region;
    }
    return null;
  }
}

