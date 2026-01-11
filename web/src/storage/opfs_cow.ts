import { assertSectorAligned, checkedOffset, type AsyncSectorDisk } from "./disk";
import { OpfsAeroSparseDisk } from "./opfs_sparse";

/**
 * Copy-on-write disk composed of:
 * - `base`: typically a large imported raw image (OPFS file)
 * - `overlay`: Aero sparse disk storing only modified blocks
 *
 * The overlay must have the same virtual capacity as the base disk.
 */
export class OpfsCowDisk implements AsyncSectorDisk {
  readonly sectorSize: number;
  readonly capacityBytes: number;

  constructor(
    private readonly base: AsyncSectorDisk,
    private readonly overlay: OpfsAeroSparseDisk,
  ) {
    if (base.capacityBytes !== overlay.capacityBytes) {
      throw new Error("base/overlay capacity mismatch");
    }
    if (base.sectorSize !== overlay.sectorSize) {
      throw new Error("base/overlay sector size mismatch");
    }
    this.sectorSize = base.sectorSize;
    this.capacityBytes = base.capacityBytes;
  }

  async readSectors(lba: number, buffer: Uint8Array): Promise<void> {
    assertSectorAligned(buffer.byteLength, this.sectorSize);
    const offset = checkedOffset(lba, buffer.byteLength, this.sectorSize);
    if (offset + buffer.byteLength > this.capacityBytes) {
      throw new Error("read past end of disk");
    }

    const blockSize = this.overlay.blockSizeBytes;
    let pos = 0;
    while (pos < buffer.byteLength) {
      const abs = offset + pos;
      const blockIndex = Math.floor(abs / blockSize);
      const within = abs % blockSize;
      const chunkLen = Math.min(blockSize - within, buffer.byteLength - pos);

      const slice = buffer.subarray(pos, pos + chunkLen);
      const chunkLba = Math.floor(abs / this.sectorSize);
      if (this.overlay.isBlockAllocated(blockIndex)) {
        await this.overlay.readSectors(chunkLba, slice);
      } else {
        await this.base.readSectors(chunkLba, slice);
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

    const blockSize = this.overlay.blockSizeBytes;
    let pos = 0;
    while (pos < data.byteLength) {
      const abs = offset + pos;
      const blockIndex = Math.floor(abs / blockSize);
      const within = abs % blockSize;
      const chunkLen = Math.min(blockSize - within, data.byteLength - pos);
      const chunk = data.subarray(pos, pos + chunkLen);

      if (this.overlay.isBlockAllocated(blockIndex)) {
        const chunkLba = Math.floor(abs / this.sectorSize);
        await this.overlay.writeSectors(chunkLba, chunk);
        pos += chunkLen;
        continue;
      }

      // First write to this block.
      if (within === 0 && chunkLen === blockSize) {
        // Whole-block overwrite: no need to consult base.
        await this.overlay.writeBlock(blockIndex, chunk);
        pos += chunkLen;
        continue;
      }

      // Partial write: seed block from base, patch, then write entire block to overlay.
      const tmp = new Uint8Array(blockSize);
      const blockStartByte = blockIndex * blockSize;
      const blockStartLba = Math.floor(blockStartByte / this.sectorSize);
      const validLen = Math.min(blockSize, this.capacityBytes - blockStartByte);
      await this.base.readSectors(blockStartLba, tmp.subarray(0, validLen));
      tmp.set(chunk, within);
      await this.overlay.writeBlock(blockIndex, tmp);
      pos += chunkLen;
    }
  }

  async flush(): Promise<void> {
    await this.overlay.flush();
    await this.base.flush();
  }

  async close(): Promise<void> {
    await this.overlay.close?.();
    await this.base.close?.();
  }
}
