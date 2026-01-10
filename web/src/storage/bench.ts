import type { AsyncSectorDisk } from "./disk";

export type BenchResult = {
  bytes: number;
  seconds: number;
  mibPerSec: number;
};

export async function benchSequentialWrite(
  disk: AsyncSectorDisk,
  opts: { totalBytes: number; chunkBytes?: number },
): Promise<BenchResult> {
  const sectorSize = disk.sectorSize;
  const chunkBytes = opts.chunkBytes ?? 1024 * 1024;
  const chunkAligned = Math.max(sectorSize, Math.floor(chunkBytes / sectorSize) * sectorSize);
  const totalAligned = Math.floor(opts.totalBytes / sectorSize) * sectorSize;

  const buf = new Uint8Array(chunkAligned);
  for (let i = 0; i < buf.length; i++) buf[i] = i & 0xff;

  const start = performance.now();
  let written = 0;
  let lba = 0;
  while (written < totalAligned) {
    const remaining = totalAligned - written;
    const thisChunk = Math.min(remaining, buf.byteLength);
    const view = buf.subarray(0, thisChunk);
    await disk.writeSectors(lba, view);
    written += thisChunk;
    lba += thisChunk / sectorSize;
  }
  await disk.flush();
  const seconds = (performance.now() - start) / 1000;
  return { bytes: written, seconds, mibPerSec: written / (1024 * 1024) / seconds };
}

export async function benchSequentialRead(
  disk: AsyncSectorDisk,
  opts: { totalBytes: number; chunkBytes?: number },
): Promise<BenchResult> {
  const sectorSize = disk.sectorSize;
  const chunkBytes = opts.chunkBytes ?? 1024 * 1024;
  const chunkAligned = Math.max(sectorSize, Math.floor(chunkBytes / sectorSize) * sectorSize);
  const totalAligned = Math.floor(opts.totalBytes / sectorSize) * sectorSize;

  const buf = new Uint8Array(chunkAligned);

  const start = performance.now();
  let read = 0;
  let lba = 0;
  while (read < totalAligned) {
    const remaining = totalAligned - read;
    const thisChunk = Math.min(remaining, buf.byteLength);
    const view = buf.subarray(0, thisChunk);
    await disk.readSectors(lba, view);
    read += thisChunk;
    lba += thisChunk / sectorSize;
  }
  const seconds = (performance.now() - start) / 1000;
  return { bytes: read, seconds, mibPerSec: read / (1024 * 1024) / seconds };
}
