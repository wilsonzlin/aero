export const SECTOR_SIZE = 512;

export interface AsyncSectorDisk {
  readonly sectorSize: number;
  readonly capacityBytes: number;

  readSectors(lba: number, buffer: Uint8Array): Promise<void>;
  writeSectors(lba: number, data: Uint8Array): Promise<void>;
  flush(): Promise<void>;
  close?(): Promise<void>;
}

export function assertSectorAligned(byteLength: number): void {
  if (byteLength % SECTOR_SIZE !== 0) {
    throw new Error(`unaligned length ${byteLength} (expected multiple of ${SECTOR_SIZE})`);
  }
}

export function checkedOffset(lba: number, byteLength: number): number {
  // Windows 7 images are ~20–40GB; numbers are safe up to 2^53-1.
  if (!Number.isInteger(lba) || lba < 0) {
    throw new Error(`invalid lba=${lba}`);
  }
  const offset = lba * SECTOR_SIZE;
  if (!Number.isSafeInteger(offset)) {
    throw new Error(`offset overflow (lba=${lba})`);
  }
  const end = offset + byteLength;
  if (!Number.isSafeInteger(end)) {
    throw new Error(`offset overflow (lba=${lba}, len=${byteLength})`);
  }
  return offset;
}
