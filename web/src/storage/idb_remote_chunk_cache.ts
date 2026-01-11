import { idbReq, idbTxDone, openDiskManagerDb } from "./metadata.ts";

export type RemoteChunkCacheSignature = {
  imageId: string;
  version: string;
  etag: string | null;
  lastModified: string | null;
  sizeBytes: number;
  chunkSize: number;
};

type RemoteChunkCacheMetaRecord = {
  cacheKey: string;
  imageId: string;
  version: string;
  etag: string | null;
  lastModified: string | null;
  sizeBytes: number;
  chunkSize: number;
  bytesUsed: number;
  accessCounter: number;
};

type RemoteChunkRecord = {
  cacheKey: string;
  chunkIndex: number;
  data: ArrayBuffer;
  byteLength: number;
  lastAccess: number;
};

// Bounds for per-chunk records stored in IndexedDB.
//
// These are intentionally conservative to keep individual IDB records/transactions reasonably sized,
// while still supporting Aero's canonical remote disk chunk sizes:
// - Range streaming defaults to 1 MiB
// - Chunked (manifest + chunk objects) delivery defaults to 4 MiB
const MIN_CHUNK_SIZE_BYTES = 512 * 1024;
const MAX_CHUNK_SIZE_BYTES = 8 * 1024 * 1024;

function validateChunkSize(chunkSize: number): void {
  if (!Number.isSafeInteger(chunkSize) || chunkSize <= 0) {
    throw new Error(`chunkSize must be a positive safe integer (got ${chunkSize})`);
  }
  if (chunkSize < MIN_CHUNK_SIZE_BYTES || chunkSize > MAX_CHUNK_SIZE_BYTES) {
    throw new Error(
      `chunkSize must be within ${MIN_CHUNK_SIZE_BYTES}..${MAX_CHUNK_SIZE_BYTES} bytes (got ${chunkSize})`,
    );
  }
}

function signatureMatches(
  meta: RemoteChunkCacheMetaRecord,
  expected: RemoteChunkCacheSignature,
): boolean {
  return (
    meta.imageId === expected.imageId &&
    meta.version === expected.version &&
    meta.etag === expected.etag &&
    (meta.lastModified ?? null) === expected.lastModified &&
    meta.sizeBytes === expected.sizeBytes &&
    meta.chunkSize === expected.chunkSize
  );
}

async function deleteAllChunksForCacheKey(
  chunksStore: IDBObjectStore,
  cacheKey: string,
): Promise<void> {
  // Primary key is ["cacheKey", "chunkIndex"], so we can delete by bounded key range.
  const range = IDBKeyRange.bound([cacheKey, -Infinity], [cacheKey, Infinity]);
  await new Promise<void>((resolve, reject) => {
    const req = chunksStore.openCursor(range);
    req.onerror = () => reject(req.error ?? new Error("IndexedDB cursor failed"));
    req.onsuccess = () => {
      const cursor = req.result;
      if (!cursor) {
        resolve();
        return;
      }
      cursor.delete();
      cursor.continue();
    };
  });
}

async function enforceCacheLimitInTx(opts: {
  cacheKey: string;
  chunksStore: IDBObjectStore;
  metaStore: IDBObjectStore;
  meta: RemoteChunkCacheMetaRecord;
  cacheLimitBytes: number;
  protectedChunkIndex?: number;
}): Promise<number[]> {
  const { cacheKey, chunksStore, metaStore, meta, cacheLimitBytes, protectedChunkIndex } = opts;
  const evicted: number[] = [];
  if (meta.bytesUsed <= cacheLimitBytes) return evicted;

  const index = chunksStore.index("by_cacheKey_lastAccess");
  const range = IDBKeyRange.bound([cacheKey, -Infinity], [cacheKey, Infinity]);

  await new Promise<void>((resolve, reject) => {
    const req = index.openCursor(range);
    req.onerror = () => reject(req.error ?? new Error("IndexedDB cursor failed"));
    req.onsuccess = () => {
      const cursor = req.result;
      if (!cursor) {
        resolve();
        return;
      }

      const rec = cursor.value as RemoteChunkRecord;
      const shouldSkip = protectedChunkIndex !== undefined && rec.chunkIndex === protectedChunkIndex;
      if (shouldSkip) {
        cursor.continue();
        return;
      }

      cursor.delete();
      evicted.push(rec.chunkIndex);
      meta.bytesUsed = Math.max(0, meta.bytesUsed - (rec.byteLength ?? rec.data.byteLength));

      if (meta.bytesUsed <= cacheLimitBytes) {
        resolve();
        return;
      }

      cursor.continue();
    };
  });

  metaStore.put(meta);
  return evicted;
}

/**
 * IndexedDB-backed cache for remote streaming disks.
 *
 * Data layout (in `aero-disk-manager`):
 * - Store: `remote_chunks` (keyPath: ["cacheKey", "chunkIndex"])
 * - Store: `remote_chunk_meta` (keyPath: "cacheKey")
 */
export class IdbRemoteChunkCache {
  private readonly db: IDBDatabase;
  private readonly cacheKey: string;
  private readonly signature: RemoteChunkCacheSignature;
  private readonly cacheLimitBytes: number | null;
  private readonly maxCachedChunks: number;
  private readonly cache = new Map<number, Uint8Array>();
  private readonly pendingAccess = new Set<number>();

  private constructor(
    db: IDBDatabase,
    cacheKey: string,
    signature: RemoteChunkCacheSignature,
    cacheLimitBytes: number | null,
    maxCachedChunks: number,
  ) {
    this.db = db;
    this.cacheKey = cacheKey;
    this.signature = signature;
    this.cacheLimitBytes = cacheLimitBytes;
    this.maxCachedChunks = maxCachedChunks;
  }

  static async open(opts: {
    cacheKey: string;
    signature: RemoteChunkCacheSignature;
    cacheLimitBytes?: number | null;
    /**
     * Maximum number of chunks to keep in memory (LRU).
     *
     * This reduces IndexedDB roundtrips, especially for sequential reads that revisit the same
     * chunk(s) (e.g. small reads within a 1 MiB block).
     */
    maxCachedChunks?: number;
  }): Promise<IdbRemoteChunkCache> {
    if (!opts.cacheKey) throw new Error("cacheKey must not be empty");
    validateChunkSize(opts.signature.chunkSize);
    if (!Number.isSafeInteger(opts.signature.sizeBytes) || opts.signature.sizeBytes <= 0) {
      throw new Error(`sizeBytes must be a positive safe integer (got ${opts.signature.sizeBytes})`);
    }
    if (opts.cacheLimitBytes !== undefined && opts.cacheLimitBytes !== null) {
      if (!Number.isSafeInteger(opts.cacheLimitBytes) || opts.cacheLimitBytes < 0) {
        throw new Error(`cacheLimitBytes must be null or a non-negative safe integer (got ${opts.cacheLimitBytes})`);
      }
    }

    const maxCachedChunks = opts.maxCachedChunks ?? 64;
    if (!Number.isSafeInteger(maxCachedChunks) || maxCachedChunks < 0) {
      throw new Error(`maxCachedChunks must be a non-negative safe integer (got ${maxCachedChunks})`);
    }

    const db = await openDiskManagerDb();
    const cache = new IdbRemoteChunkCache(db, opts.cacheKey, opts.signature, opts.cacheLimitBytes ?? null, maxCachedChunks);
    await cache.ensureCompatible();
    return cache;
  }

  close(): void {
    this.pendingAccess.clear();
    this.cache.clear();
    this.db.close();
  }

  async clear(): Promise<void> {
    const tx = this.db.transaction(["remote_chunks", "remote_chunk_meta"], "readwrite");
    const chunksStore = tx.objectStore("remote_chunks");
    const metaStore = tx.objectStore("remote_chunk_meta");
    await deleteAllChunksForCacheKey(chunksStore, this.cacheKey);
    metaStore.delete(this.cacheKey);
    await idbTxDone(tx);
    this.pendingAccess.clear();
    this.cache.clear();
  }

  async getStatus(): Promise<{ bytesUsed: number; cacheLimitBytes: number | null }> {
    const tx = this.db.transaction(["remote_chunk_meta"], "readonly");
    const metaStore = tx.objectStore("remote_chunk_meta");
    const meta = (await idbReq(metaStore.get(this.cacheKey))) as RemoteChunkCacheMetaRecord | undefined;
    await idbTxDone(tx);
    return { bytesUsed: meta?.bytesUsed ?? 0, cacheLimitBytes: this.cacheLimitBytes };
  }

  private touchCacheKey(chunkIndex: number, bytes: Uint8Array): void {
    if (this.maxCachedChunks <= 0) return;
    this.cache.delete(chunkIndex);
    this.cache.set(chunkIndex, bytes);
  }

  private evictMemoryIfNeeded(): void {
    if (this.maxCachedChunks <= 0) {
      this.cache.clear();
      return;
    }
    while (this.cache.size > this.maxCachedChunks) {
      const lruKey = this.cache.keys().next().value as number | undefined;
      if (lruKey === undefined) break;
      this.cache.delete(lruKey);
    }
  }

  private async applyPendingAccessInTx(meta: RemoteChunkCacheMetaRecord, chunksStore: IDBObjectStore): Promise<void> {
    if (this.pendingAccess.size === 0) return;

    const indices = Array.from(this.pendingAccess);
    this.pendingAccess.clear();

    const reqs = indices.map(async (idx) => {
      const rec = (await idbReq(chunksStore.get([this.cacheKey, idx]))) as RemoteChunkRecord | undefined;
      return { idx, rec };
    });
    const records = await Promise.all(reqs);

    for (const { rec } of records) {
      if (!rec) continue;
      meta.accessCounter += 1;
      rec.lastAccess = meta.accessCounter;
      rec.byteLength = rec.byteLength ?? rec.data.byteLength;
      chunksStore.put(rec);
    }
  }

  /**
   * Batch-fetch cached chunks.
   *
   * Returns a map of `chunkIndex -> bytes` for chunks that exist in the cache.
   * Missing chunks are omitted from the map.
   */
  async getMany(chunkIndices: number[]): Promise<Map<number, Uint8Array>> {
    const out = new Map<number, Uint8Array>();
    const missing: number[] = [];

    // De-dupe indices (keep first occurrence order).
    const seen = new Set<number>();
    for (const idx of chunkIndices) {
      if (!Number.isSafeInteger(idx) || idx < 0) {
        throw new Error(`chunkIndex must be a non-negative integer (got ${idx})`);
      }
      if (seen.has(idx)) continue;
      seen.add(idx);

      const hit = this.cache.get(idx);
      if (hit) {
        this.touchCacheKey(idx, hit);
        this.pendingAccess.add(idx);
        out.set(idx, hit);
      } else {
        missing.push(idx);
      }
    }

    if (missing.length === 0) return out;

    const tx = this.db.transaction(["remote_chunks", "remote_chunk_meta"], "readwrite");
    const chunksStore = tx.objectStore("remote_chunks");
    const metaStore = tx.objectStore("remote_chunk_meta");

    const meta = await this.getOrInitMetaAndMaybeClearInTx(metaStore, chunksStore);

    const reqs = missing.map(async (idx) => {
      const rec = (await idbReq(chunksStore.get([this.cacheKey, idx]))) as RemoteChunkRecord | undefined;
      return { idx, rec };
    });
    const records = await Promise.all(reqs);

    for (const { idx, rec } of records) {
      if (!rec) continue;
      meta.accessCounter += 1;
      rec.lastAccess = meta.accessCounter;
      // Heal: older versions might not have `byteLength` populated.
      rec.byteLength = rec.byteLength ?? rec.data.byteLength;
      chunksStore.put(rec);
      out.set(idx, new Uint8Array(rec.data));
    }

    metaStore.put(meta);
    await idbTxDone(tx);

    for (const [idx, bytes] of out) {
      if (!this.cache.has(idx)) {
        this.cache.set(idx, bytes);
      }
      this.touchCacheKey(idx, bytes);
    }
    this.evictMemoryIfNeeded();
    return out;
  }

  async get(chunkIndex: number): Promise<Uint8Array | null> {
    if (!Number.isSafeInteger(chunkIndex) || chunkIndex < 0) {
      throw new Error(`chunkIndex must be a non-negative integer (got ${chunkIndex})`);
    }

    const hit = this.cache.get(chunkIndex);
    if (hit) {
      this.touchCacheKey(chunkIndex, hit);
      this.pendingAccess.add(chunkIndex);
      return hit;
    }

    const res = await this.getMany([chunkIndex]);
    return res.get(chunkIndex) ?? null;
  }

  async put(chunkIndex: number, bytes: Uint8Array): Promise<void> {
    if (!Number.isSafeInteger(chunkIndex) || chunkIndex < 0) {
      throw new Error(`chunkIndex must be a non-negative integer (got ${chunkIndex})`);
    }

    // Store as an ArrayBuffer with exact length (avoid capturing a larger backing buffer).
    const data = bytes.slice().buffer;

    const tx = this.db.transaction(["remote_chunks", "remote_chunk_meta"], "readwrite");
    const chunksStore = tx.objectStore("remote_chunks");
    const metaStore = tx.objectStore("remote_chunk_meta");

    const meta = await this.getOrInitMetaAndMaybeClearInTx(metaStore, chunksStore);
    await this.applyPendingAccessInTx(meta, chunksStore);

    const existing = (await idbReq(chunksStore.get([this.cacheKey, chunkIndex]))) as RemoteChunkRecord | undefined;
    const oldBytes = existing?.byteLength ?? existing?.data?.byteLength ?? 0;

    meta.accessCounter += 1;
    const lastAccess = meta.accessCounter;

    const rec: RemoteChunkRecord = {
      cacheKey: this.cacheKey,
      chunkIndex,
      data,
      byteLength: data.byteLength,
      lastAccess,
    };
    chunksStore.put(rec);

    meta.bytesUsed = Math.max(0, meta.bytesUsed - oldBytes) + data.byteLength;
    metaStore.put(meta);

    let evicted: number[] = [];
    if (this.cacheLimitBytes !== null && meta.bytesUsed > this.cacheLimitBytes) {
      evicted = await enforceCacheLimitInTx({
        cacheKey: this.cacheKey,
        chunksStore,
        metaStore,
        meta,
        cacheLimitBytes: this.cacheLimitBytes,
        protectedChunkIndex: chunkIndex,
      });
    }

    await idbTxDone(tx);

    if (this.maxCachedChunks > 0) {
      const cached = new Uint8Array(data);
      this.cache.set(chunkIndex, cached);
      this.touchCacheKey(chunkIndex, cached);
      for (const idx of evicted) this.cache.delete(idx);
      this.evictMemoryIfNeeded();
    }
  }

  async delete(chunkIndex: number): Promise<void> {
    if (!Number.isSafeInteger(chunkIndex) || chunkIndex < 0) {
      throw new Error(`chunkIndex must be a non-negative integer (got ${chunkIndex})`);
    }

    const tx = this.db.transaction(["remote_chunks", "remote_chunk_meta"], "readwrite");
    const chunksStore = tx.objectStore("remote_chunks");
    const metaStore = tx.objectStore("remote_chunk_meta");

    const meta = await this.getOrInitMetaAndMaybeClearInTx(metaStore, chunksStore);
    const existing = (await idbReq(chunksStore.get([this.cacheKey, chunkIndex]))) as RemoteChunkRecord | undefined;
    if (existing) {
      chunksStore.delete([this.cacheKey, chunkIndex]);
      meta.bytesUsed = Math.max(0, meta.bytesUsed - (existing.byteLength ?? existing.data.byteLength));
      metaStore.put(meta);
    }

    await idbTxDone(tx);
    this.cache.delete(chunkIndex);
    this.pendingAccess.delete(chunkIndex);
  }

  async listChunkIndices(): Promise<number[]> {
    const tx = this.db.transaction(["remote_chunks"], "readonly");
    const chunksStore = tx.objectStore("remote_chunks");
    const range = IDBKeyRange.bound([this.cacheKey, -Infinity], [this.cacheKey, Infinity]);
    const out: number[] = [];
    await new Promise<void>((resolve, reject) => {
      const req = chunksStore.openCursor(range);
      req.onerror = () => reject(req.error ?? new Error("IndexedDB cursor failed"));
      req.onsuccess = () => {
        const cursor = req.result;
        if (!cursor) {
          resolve();
          return;
        }
        const rec = cursor.value as RemoteChunkRecord;
        out.push(rec.chunkIndex);
        cursor.continue();
      };
    });
    await idbTxDone(tx);
    out.sort((a, b) => a - b);
    return out;
  }

  private async ensureCompatible(): Promise<void> {
    const tx = this.db.transaction(["remote_chunk_meta", "remote_chunks"], "readwrite");
    const metaStore = tx.objectStore("remote_chunk_meta");
    const chunksStore = tx.objectStore("remote_chunks");

    const meta = (await idbReq(metaStore.get(this.cacheKey))) as RemoteChunkCacheMetaRecord | undefined;
    if (meta && signatureMatches(meta, this.signature)) {
      await idbTxDone(tx);
      return;
    }

    // Invalidate: signature mismatch or missing meta.
    await deleteAllChunksForCacheKey(chunksStore, this.cacheKey);
    this.pendingAccess.clear();
    this.cache.clear();

    const fresh: RemoteChunkCacheMetaRecord = {
      cacheKey: this.cacheKey,
      imageId: this.signature.imageId,
      version: this.signature.version,
      etag: this.signature.etag,
      lastModified: this.signature.lastModified,
      sizeBytes: this.signature.sizeBytes,
      chunkSize: this.signature.chunkSize,
      bytesUsed: 0,
      accessCounter: 0,
    };
    metaStore.put(fresh);
    await idbTxDone(tx);
  }

  private async getOrInitMetaAndMaybeClearInTx(
    metaStore: IDBObjectStore,
    chunksStore: IDBObjectStore,
  ): Promise<RemoteChunkCacheMetaRecord> {
    const meta = (await idbReq(metaStore.get(this.cacheKey))) as RemoteChunkCacheMetaRecord | undefined;
    if (meta && signatureMatches(meta, this.signature)) return meta;

    // Either missing, or mismatched (should be rare here); treat as invalidation.
    await deleteAllChunksForCacheKey(chunksStore, this.cacheKey);
    this.pendingAccess.clear();
    this.cache.clear();
    const fresh: RemoteChunkCacheMetaRecord = {
      cacheKey: this.cacheKey,
      imageId: this.signature.imageId,
      version: this.signature.version,
      etag: this.signature.etag,
      lastModified: this.signature.lastModified,
      sizeBytes: this.signature.sizeBytes,
      chunkSize: this.signature.chunkSize,
      bytesUsed: 0,
      accessCounter: 0,
    };
    metaStore.put(fresh);
    return fresh;
  }
}
