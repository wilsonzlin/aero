import { assertSectorAligned, checkedOffset, type AsyncSectorDisk } from "./disk";
import { IdbChunkDisk } from "./idb_chunk_disk";
import { idbTxDone, openDiskManagerDb } from "./metadata";

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function hasOwn(obj: object, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(obj, key);
}

async function loadAllocatedChunks(diskId: string): Promise<Set<number>> {
  const db = await openDiskManagerDb();
  try {
    const tx = db.transaction(["chunks"], "readonly");
    const store = tx.objectStore("chunks");
    const index = store.index("by_id");
    const range = IDBKeyRange.only(diskId);
    const allocated = new Set<number>();

    await new Promise<void>((resolve, reject) => {
      const req = index.openCursor(range);
      req.onerror = () => reject(req.error ?? new Error("IndexedDB cursor failed"));
      req.onsuccess = () => {
        const cursor = req.result;
        if (!cursor) {
          resolve();
          return;
        }
        const value = cursor.value as unknown;
        if (isRecord(value)) {
          // Defensive: IndexedDB contents are untrusted/can be corrupt. Never observe inherited
          // fields (prototype pollution) and only treat chunks as "allocated" when the record is
          // well-typed (matching `IdbChunkDisk`'s acceptance rules).
          const id = hasOwn(value, "id") ? value.id : undefined;
          const idx = hasOwn(value, "index") ? value.index : undefined;
          const dataRaw = hasOwn(value, "data") ? value.data : undefined;
          const dataAny = dataRaw as any;
          const okData = dataAny instanceof ArrayBuffer || dataAny instanceof Uint8Array;
          if (id === diskId && typeof idx === "number" && Number.isInteger(idx) && idx >= 0 && okData) {
            allocated.add(idx);
          }
        }
        cursor.continue();
      };
    });

    await idbTxDone(tx);
    return allocated;
  } finally {
    db.close();
  }
}

/**
 * Copy-on-write disk that stores modified sectors in IndexedDB (via `IdbChunkDisk`).
 *
 * This is the fallback path for browsers that lack OPFS SyncAccessHandle support.
 */
export class IdbCowDisk implements AsyncSectorDisk {
  readonly sectorSize: number;
  readonly capacityBytes: number;

  private readonly blockSizeBytes: number;

  private constructor(
    private readonly base: AsyncSectorDisk,
    private readonly overlay: IdbChunkDisk,
    private readonly allocatedBlocks: Set<number>,
  ) {
    if (base.capacityBytes !== overlay.capacityBytes) {
      throw new Error("base/overlay capacity mismatch");
    }
    if (base.sectorSize !== overlay.sectorSize) {
      throw new Error("base/overlay sector size mismatch");
    }
    this.sectorSize = base.sectorSize;
    this.capacityBytes = base.capacityBytes;
    this.blockSizeBytes = overlay.chunkSizeBytes;
  }

  static async open(base: AsyncSectorDisk, overlayDiskId: string, capacityBytes: number): Promise<IdbCowDisk> {
    const overlay = await IdbChunkDisk.open(overlayDiskId, capacityBytes);
    const allocated = await loadAllocatedChunks(overlayDiskId);
    return new IdbCowDisk(base, overlay, allocated);
  }

  async readSectors(lba: number, buffer: Uint8Array): Promise<void> {
    assertSectorAligned(buffer.byteLength, this.sectorSize);
    const offset = checkedOffset(lba, buffer.byteLength, this.sectorSize);
    if (offset + buffer.byteLength > this.capacityBytes) {
      throw new Error("read past end of disk");
    }

    const blockSize = this.blockSizeBytes;
    let pos = 0;
    while (pos < buffer.byteLength) {
      const abs = offset + pos;
      const blockIndex = Math.floor(abs / blockSize);
      const within = abs % blockSize;
      const chunkLen = Math.min(blockSize - within, buffer.byteLength - pos);

      const slice = buffer.subarray(pos, pos + chunkLen);
      const chunkLba = Math.floor(abs / this.sectorSize);
      if (this.allocatedBlocks.has(blockIndex)) {
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

    const blockSize = this.blockSizeBytes;
    let pos = 0;
    while (pos < data.byteLength) {
      const abs = offset + pos;
      const blockIndex = Math.floor(abs / blockSize);
      const within = abs % blockSize;
      const chunkLen = Math.min(blockSize - within, data.byteLength - pos);
      const chunk = data.subarray(pos, pos + chunkLen);

      if (this.allocatedBlocks.has(blockIndex)) {
        const chunkLba = Math.floor(abs / this.sectorSize);
        await this.overlay.writeSectors(chunkLba, chunk);
        pos += chunkLen;
        continue;
      }

      const blockStartByte = blockIndex * blockSize;
      const blockStartLba = Math.floor(blockStartByte / this.sectorSize);
      const validLen = Math.min(blockSize, this.capacityBytes - blockStartByte);

      // First write to this block.
      if (within === 0 && chunkLen === validLen) {
        // Whole-block overwrite: no need to consult base.
        await this.overlay.writeSectors(blockStartLba, chunk);
        this.allocatedBlocks.add(blockIndex);
        pos += chunkLen;
        continue;
      }

      // Partial write: seed the block from base, patch, then write the full block into the overlay.
      const tmp = new Uint8Array(validLen);
      await this.base.readSectors(blockStartLba, tmp);
      tmp.set(chunk, within);
      await this.overlay.writeSectors(blockStartLba, tmp);
      this.allocatedBlocks.add(blockIndex);
      pos += chunkLen;
    }
  }

  async flush(): Promise<void> {
    await this.overlay.flush();
    await this.base.flush();
  }

  async clearCache(): Promise<void> {
    const baseAny = this.base as unknown as { clearCache?: () => Promise<void> };
    if (typeof baseAny.clearCache !== "function") {
      throw new Error("base disk does not support cache clearing");
    }
    await baseAny.clearCache();
  }

  async close(): Promise<void> {
    let firstErr: unknown;
    try {
      await this.overlay.close?.();
    } catch (err) {
      firstErr = err;
    }
    try {
      await this.base.close?.();
    } catch (err) {
      if (!firstErr) firstErr = err;
    }
    if (firstErr) throw firstErr;
  }
}
