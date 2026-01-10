import { assertSectorAligned, checkedOffset, SECTOR_SIZE, type AsyncSectorDisk } from "./disk";
import { IDB_CHUNK_SIZE } from "./import_export.ts";
import { idbReq, idbTxDone, openDiskManagerDb } from "./metadata.ts";

type ChunkRecord = { id: string; index: number; data: ArrayBuffer };

type CacheEntry = { data: Uint8Array; dirty: boolean };

/**
 * IndexedDB-backed sparse disk view over the DiskManager `chunks` store.
 *
 * This is intended for runtime sector I/O when OPFS is unavailable.
 *
 * Data layout:
 * - DB: `aero-disk-manager`
 * - Store: `chunks` (keyPath: ["id", "index"])
 * - Missing chunk records are treated as zero-filled.
 */
export class IdbChunkDisk implements AsyncSectorDisk {
  readonly sectorSize = SECTOR_SIZE;
  readonly capacityBytes: number;
  readonly chunkSizeBytes: number;

  private readonly cache = new Map<number, CacheEntry>();

  private constructor(
    private readonly db: IDBDatabase,
    private readonly diskId: string,
    capacityBytes: number,
    private readonly maxCachedChunks: number,
  ) {
    this.capacityBytes = capacityBytes;
    this.chunkSizeBytes = IDB_CHUNK_SIZE;
  }

  static async open(
    diskId: string,
    capacityBytes: number,
    opts: { maxCachedChunks?: number } = {},
  ): Promise<IdbChunkDisk> {
    if (!diskId) throw new Error("diskId must not be empty");
    if (!Number.isSafeInteger(capacityBytes) || capacityBytes <= 0) {
      throw new Error("capacityBytes must be a positive safe integer");
    }
    const db = await openDiskManagerDb();
    return new IdbChunkDisk(db, diskId, capacityBytes, opts.maxCachedChunks ?? 64);
  }

  private touchCacheKey(key: number, entry: CacheEntry): void {
    // Map maintains insertion order; delete+set moves key to the end (MRU).
    this.cache.delete(key);
    this.cache.set(key, entry);
  }

  private async getChunkRecord(index: number): Promise<ChunkRecord | undefined> {
    const tx = this.db.transaction(["chunks"], "readonly");
    const store = tx.objectStore("chunks");
    const req = store.get([this.diskId, index]);
    const rec = (await idbReq(req)) as ChunkRecord | undefined;
    await idbTxDone(tx);
    return rec;
  }

  private expectedChunkLen(index: number): number {
    const start = index * this.chunkSizeBytes;
    if (start >= this.capacityBytes) return 0;
    return Math.min(this.chunkSizeBytes, this.capacityBytes - start);
  }

  private async putChunks(entries: Array<[number, Uint8Array]>): Promise<void> {
    const tx = this.db.transaction(["chunks"], "readwrite");
    const store = tx.objectStore("chunks");
    for (const [index, data] of entries) {
      const outLen = this.expectedChunkLen(index);
      const buf = data.slice(0, outLen).buffer;
      store.put({ id: this.diskId, index, data: buf } satisfies ChunkRecord);
    }
    await idbTxDone(tx);
  }

  private async evictIfNeeded(): Promise<void> {
    if (this.cache.size <= this.maxCachedChunks) return;

    /** @type {Array<[number, Uint8Array]>} */
    const dirtyToWrite: Array<[number, Uint8Array]> = [];
    while (this.cache.size > this.maxCachedChunks) {
      const lruKey = this.cache.keys().next().value as number | undefined;
      if (lruKey === undefined) break;
      const entry = this.cache.get(lruKey)!;
      this.cache.delete(lruKey);
      if (entry.dirty) dirtyToWrite.push([lruKey, entry.data]);
    }

    if (dirtyToWrite.length > 0) {
      await this.putChunks(dirtyToWrite);
    }
  }

  private async loadChunk(index: number): Promise<CacheEntry> {
    const hit = this.cache.get(index);
    if (hit) {
      this.touchCacheKey(index, hit);
      return hit;
    }

    const entry: CacheEntry = { data: new Uint8Array(this.chunkSizeBytes), dirty: false };

    const expectedLen = this.expectedChunkLen(index);
    if (expectedLen === 0) {
      // Outside virtual disk capacity.
      this.cache.set(index, entry);
      this.touchCacheKey(index, entry);
      await this.evictIfNeeded();
      return entry;
    }

    const rec = await this.getChunkRecord(index);
    if (rec?.data) {
      const existing = new Uint8Array(rec.data, 0, Math.min(expectedLen, rec.data.byteLength));
      entry.data.set(existing);
    }

    this.cache.set(index, entry);
    this.touchCacheKey(index, entry);
    await this.evictIfNeeded();
    return entry;
  }

  async readSectors(lba: number, buffer: Uint8Array): Promise<void> {
    assertSectorAligned(buffer.byteLength, this.sectorSize);
    const offset = checkedOffset(lba, buffer.byteLength, this.sectorSize);
    if (offset + buffer.byteLength > this.capacityBytes) {
      throw new Error("read past end of disk");
    }

    let pos = 0;
    while (pos < buffer.byteLength) {
      const abs = offset + pos;
      const chunkIndex = Math.floor(abs / this.chunkSizeBytes);
      const within = abs % this.chunkSizeBytes;
      const chunkLen = Math.min(this.chunkSizeBytes - within, buffer.byteLength - pos);

      const entry = await this.loadChunk(chunkIndex);
      buffer.set(entry.data.subarray(within, within + chunkLen), pos);
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
      const chunkIndex = Math.floor(abs / this.chunkSizeBytes);
      const within = abs % this.chunkSizeBytes;
      const chunkLen = Math.min(this.chunkSizeBytes - within, data.byteLength - pos);

      const entry = await this.loadChunk(chunkIndex);
      entry.data.set(data.subarray(pos, pos + chunkLen), within);
      entry.dirty = true;
      this.touchCacheKey(chunkIndex, entry);
      await this.evictIfNeeded();

      pos += chunkLen;
    }
  }

  async flush(): Promise<void> {
    const dirty: Array<[number, Uint8Array]> = [];
    for (const [k, v] of this.cache) {
      if (v.dirty) dirty.push([k, v.data]);
    }
    if (dirty.length === 0) return;
    await this.putChunks(dirty);
    for (const [k] of dirty) {
      const entry = this.cache.get(k);
      if (entry) entry.dirty = false;
    }
  }

  async close(): Promise<void> {
    await this.flush();
    this.db.close();
  }
}
