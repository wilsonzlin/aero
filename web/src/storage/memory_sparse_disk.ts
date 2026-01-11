import { assertSectorAligned, checkedOffset, SECTOR_SIZE } from "./disk";
import type { SparseBlockDisk } from "./sparse_block_disk";

/**
 * In-memory sparse disk used for unit tests.
 *
 * Production code should use `OpfsAeroSparseDisk` instead.
 */
export class MemorySparseDisk implements SparseBlockDisk {
  readonly sectorSize = SECTOR_SIZE;
  readonly capacityBytes: number;
  readonly blockSizeBytes: number;

  private readonly blocks = new Map<number, Uint8Array>();
  private readonly blockCount: number;

  private constructor(capacityBytes: number, blockSizeBytes: number) {
    if (!Number.isSafeInteger(capacityBytes) || capacityBytes <= 0) {
      throw new Error(`invalid capacityBytes=${capacityBytes}`);
    }
    if (!Number.isSafeInteger(blockSizeBytes) || blockSizeBytes <= 0) {
      throw new Error(`invalid blockSizeBytes=${blockSizeBytes}`);
    }
    if (blockSizeBytes % this.sectorSize !== 0) {
      throw new Error(`blockSizeBytes must be a multiple of ${this.sectorSize}`);
    }

    this.capacityBytes = capacityBytes;
    this.blockSizeBytes = blockSizeBytes;
    this.blockCount = Math.ceil(capacityBytes / blockSizeBytes);
  }

  static create(opts: { diskSizeBytes: number; blockSizeBytes: number }): MemorySparseDisk {
    return new MemorySparseDisk(opts.diskSizeBytes, opts.blockSizeBytes);
  }

  private assertBlockIndex(blockIndex: number): void {
    if (!Number.isInteger(blockIndex) || blockIndex < 0 || blockIndex >= this.blockCount) {
      throw new Error(`blockIndex out of range: ${blockIndex}`);
    }
  }

  isBlockAllocated(blockIndex: number): boolean {
    this.assertBlockIndex(blockIndex);
    return this.blocks.has(blockIndex);
  }

  getAllocatedBytes(): number {
    return this.blocks.size * this.blockSizeBytes;
  }

  async readBlock(blockIndex: number, dst: Uint8Array): Promise<void> {
    this.assertBlockIndex(blockIndex);
    if (dst.byteLength !== this.blockSizeBytes) {
      throw new Error("readBlock: incorrect block size");
    }
    dst.fill(0);
    const src = this.blocks.get(blockIndex);
    if (src) dst.set(src);
  }

  async writeBlock(blockIndex: number, data: Uint8Array): Promise<void> {
    this.assertBlockIndex(blockIndex);
    if (data.byteLength !== this.blockSizeBytes) {
      throw new Error("writeBlock: incorrect block size");
    }
    this.blocks.set(blockIndex, data.slice());
  }

  async readSectors(lba: number, buffer: Uint8Array): Promise<void> {
    assertSectorAligned(buffer.byteLength, this.sectorSize);
    const offset = checkedOffset(lba, buffer.byteLength, this.sectorSize);
    if (offset + buffer.byteLength > this.capacityBytes) {
      throw new Error("read past end of disk");
    }

    buffer.fill(0);

    let pos = 0;
    while (pos < buffer.byteLength) {
      const abs = offset + pos;
      const blockIndex = Math.floor(abs / this.blockSizeBytes);
      const within = abs % this.blockSizeBytes;
      const chunkLen = Math.min(this.blockSizeBytes - within, buffer.byteLength - pos);

      const block = this.blocks.get(blockIndex);
      if (block) {
        buffer.set(block.subarray(within, within + chunkLen), pos);
      }

      pos += chunkLen;
    }
  }

  async writeSectors(lba: number, data: Uint8Array): Promise<void> {
    assertSectorAligned(data.byteLength, this.sectorSize);
    const offset = checkedOffset(lba, data.byteLength, this.sectorSize);
    if (offset + data.byteLength > this.capacityBytes) {
      throw new Error("write past end of disk");
    }

    let pos = 0;
    while (pos < data.byteLength) {
      const abs = offset + pos;
      const blockIndex = Math.floor(abs / this.blockSizeBytes);
      const within = abs % this.blockSizeBytes;
      const chunkLen = Math.min(this.blockSizeBytes - within, data.byteLength - pos);

      let block = this.blocks.get(blockIndex);
      if (!block) {
        block = new Uint8Array(this.blockSizeBytes);
        this.blocks.set(blockIndex, block);
      }
      block.set(data.subarray(pos, pos + chunkLen), within);
      pos += chunkLen;
    }
  }

  async flush(): Promise<void> {
    // no-op
  }

  async close(): Promise<void> {
    // no-op
  }
}
