import { assertSectorAligned, checkedOffset, SECTOR_SIZE, type AsyncSectorDisk } from "./disk";

type Header = {
  version: 1;
  diskSizeBytes: number;
  blockSizeBytes: number;
};

function openDb(
  name: string,
  version: number,
  onUpgrade: (db: IDBDatabase) => void,
): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open(name, version);
    req.onerror = () => reject(req.error);
    req.onupgradeneeded = () => onUpgrade(req.result);
    req.onsuccess = () => resolve(req.result);
  });
}

function txDone(tx: IDBTransaction): Promise<void> {
  return new Promise((resolve, reject) => {
    tx.oncomplete = () => resolve();
    tx.onabort = () => reject(tx.error ?? new Error("transaction aborted"));
    tx.onerror = () => reject(tx.error ?? new Error("transaction error"));
  });
}

type CacheEntry = { data: Uint8Array; dirty: boolean };

export class IndexedDbBlockDisk implements AsyncSectorDisk {
  readonly sectorSize = SECTOR_SIZE;
  readonly capacityBytes: number;
  readonly blockSizeBytes: number;

  private readonly cache = new Map<number, CacheEntry>();

  private constructor(
    private readonly db: IDBDatabase,
    header: Header,
    private readonly maxCachedBlocks: number,
  ) {
    this.capacityBytes = header.diskSizeBytes;
    this.blockSizeBytes = header.blockSizeBytes;
  }

  static async open(
    name: string,
    opts: {
      create?: boolean;
      diskSizeBytes?: number;
      blockSizeBytes?: number;
      maxCachedBlocks?: number;
    } = {},
  ): Promise<IndexedDbBlockDisk> {
    const db = await openDb(name, 1, (db) => {
      if (!db.objectStoreNames.contains("meta")) db.createObjectStore("meta");
      if (!db.objectStoreNames.contains("blocks")) db.createObjectStore("blocks");
    });

    // Load header.
    const tx = db.transaction(["meta"], "readonly");
    const meta = tx.objectStore("meta");
    const headerReq = meta.get("header");
    const header: Header | undefined = await new Promise((resolve, reject) => {
      headerReq.onsuccess = () => resolve(headerReq.result as Header | undefined);
      headerReq.onerror = () => reject(headerReq.error);
    });
    await txDone(tx);

    if (!header) {
      if (!opts.create) {
        db.close();
        throw new Error("IndexedDB disk not found (missing header)");
      }
      if (typeof opts.diskSizeBytes !== "number" || opts.diskSizeBytes <= 0) {
        db.close();
        throw new Error("diskSizeBytes must be provided when creating");
      }
      if (typeof opts.blockSizeBytes !== "number" || opts.blockSizeBytes <= 0) {
        db.close();
        throw new Error("blockSizeBytes must be provided when creating");
      }
      if (opts.blockSizeBytes % SECTOR_SIZE !== 0) {
        db.close();
        throw new Error("blockSizeBytes must be a multiple of 512");
      }

      const newHeader: Header = {
        version: 1,
        diskSizeBytes: opts.diskSizeBytes,
        blockSizeBytes: opts.blockSizeBytes,
      };
      const txw = db.transaction(["meta"], "readwrite");
      txw.objectStore("meta").put(newHeader, "header");
      await txDone(txw);

      return new IndexedDbBlockDisk(db, newHeader, opts.maxCachedBlocks ?? 256);
    }

    // Validate against requested options.
    if (
      typeof opts.diskSizeBytes === "number" &&
      opts.diskSizeBytes !== header.diskSizeBytes
    ) {
      db.close();
      throw new Error("diskSizeBytes mismatch");
    }
    if (
      typeof opts.blockSizeBytes === "number" &&
      opts.blockSizeBytes !== header.blockSizeBytes
    ) {
      db.close();
      throw new Error("blockSizeBytes mismatch");
    }

    return new IndexedDbBlockDisk(db, header, opts.maxCachedBlocks ?? 256);
  }

  private touchCacheKey(key: number, entry: CacheEntry): void {
    this.cache.delete(key);
    this.cache.set(key, entry);
  }

  private async writeBlockToDb(blockIndex: number, entry: CacheEntry): Promise<void> {
    const tx = this.db.transaction(["blocks"], "readwrite");
    tx.objectStore("blocks").put(entry.data, blockIndex);
    await txDone(tx);
  }

  private async evictIfNeeded(): Promise<void> {
    while (this.cache.size > this.maxCachedBlocks) {
      const lruKey = this.cache.keys().next().value as number | undefined;
      if (lruKey === undefined) return;
      const entry = this.cache.get(lruKey)!;
      this.cache.delete(lruKey);
      if (entry.dirty) {
        await this.writeBlockToDb(lruKey, entry);
      }
    }
  }

  private async loadBlock(blockIndex: number): Promise<CacheEntry> {
    const hit = this.cache.get(blockIndex);
    if (hit) {
      this.touchCacheKey(blockIndex, hit);
      return hit;
    }

    const data = new Uint8Array(this.blockSizeBytes);

    const tx = this.db.transaction(["blocks"], "readonly");
    const store = tx.objectStore("blocks");
    const req = store.get(blockIndex);
    const existing: Uint8Array | undefined = await new Promise((resolve, reject) => {
      req.onsuccess = () => resolve(req.result as Uint8Array | undefined);
      req.onerror = () => reject(req.error);
    });
    await txDone(tx);

    if (existing) {
      data.set(existing);
    }

    const entry: CacheEntry = { data, dirty: false };
    this.cache.set(blockIndex, entry);
    await this.evictIfNeeded();
    return entry;
  }

  async readSectors(lba: number, buffer: Uint8Array): Promise<void> {
    assertSectorAligned(buffer.byteLength);
    const offset = checkedOffset(lba, buffer.byteLength);
    if (offset + buffer.byteLength > this.capacityBytes) {
      throw new Error("read past end of disk");
    }

    let pos = 0;
    while (pos < buffer.byteLength) {
      const abs = offset + pos;
      const blockIndex = Math.floor(abs / this.blockSizeBytes);
      const within = abs % this.blockSizeBytes;
      const chunkLen = Math.min(this.blockSizeBytes - within, buffer.byteLength - pos);

      const entry = await this.loadBlock(blockIndex);
      buffer.set(entry.data.subarray(within, within + chunkLen), pos);
      pos += chunkLen;
    }
  }

  async writeSectors(lba: number, data: Uint8Array): Promise<void> {
    assertSectorAligned(data.byteLength);
    const offset = checkedOffset(lba, data.byteLength);
    if (offset + data.byteLength > this.capacityBytes) {
      throw new Error("write past end of disk");
    }

    let pos = 0;
    while (pos < data.byteLength) {
      const abs = offset + pos;
      const blockIndex = Math.floor(abs / this.blockSizeBytes);
      const within = abs % this.blockSizeBytes;
      const chunkLen = Math.min(this.blockSizeBytes - within, data.byteLength - pos);

      const entry = await this.loadBlock(blockIndex);
      entry.data.set(data.subarray(pos, pos + chunkLen), within);
      entry.dirty = true;
      this.touchCacheKey(blockIndex, entry);
      await this.evictIfNeeded();

      pos += chunkLen;
    }
  }

  async flush(): Promise<void> {
    const dirty: Array<[number, CacheEntry]> = [];
    for (const [k, v] of this.cache) {
      if (v.dirty) dirty.push([k, v]);
    }
    if (dirty.length === 0) return;

    const tx = this.db.transaction(["blocks"], "readwrite");
    const store = tx.objectStore("blocks");
    for (const [k, v] of dirty) {
      store.put(v.data, k);
      v.dirty = false;
    }
    await txDone(tx);
  }

  async close(): Promise<void> {
    await this.flush();
    this.db.close();
  }
}

