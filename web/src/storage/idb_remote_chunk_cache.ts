import { idbReq, idbTxDone, openDiskManagerDb } from "./metadata.ts";

export class IdbRemoteChunkCacheQuotaError extends Error {
  override name = "IdbRemoteChunkCacheQuotaError";

  constructor(message?: string, opts?: { cause?: unknown }) {
    super(message ?? "IndexedDB quota exceeded while writing remote chunk cache", opts);
  }
}

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

function isSafeNonNegativeInt(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

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

function isQuotaExceededError(err: unknown): boolean {
  // Browser storage quota failures typically surface as a DOMException named
  // "QuotaExceededError". Firefox can use a different name for the same condition.
  if (!err) return false;
  const name =
    err instanceof DOMException || err instanceof Error
      ? err.name
      : typeof err === "object" && "name" in err
        ? ((err as { name?: unknown }).name as unknown)
        : undefined;
  return name === "QuotaExceededError" || name === "NS_ERROR_DOM_QUOTA_REACHED";
}

function signatureMatches(meta: RemoteChunkCacheMetaRecord, expected: RemoteChunkCacheSignature): boolean {
  return (
    meta.imageId === expected.imageId &&
    meta.version === expected.version &&
    meta.etag === expected.etag &&
    (meta.lastModified ?? null) === expected.lastModified &&
    meta.sizeBytes === expected.sizeBytes &&
    meta.chunkSize === expected.chunkSize
  );
}

function isValidMetaRecord(raw: unknown, cacheKey: string, signature: RemoteChunkCacheSignature): raw is RemoteChunkCacheMetaRecord {
  if (!raw || typeof raw !== "object") return false;
  const rec = raw as Partial<RemoteChunkCacheMetaRecord> & {
    cacheKey?: unknown;
    imageId?: unknown;
    version?: unknown;
    etag?: unknown;
    lastModified?: unknown;
    sizeBytes?: unknown;
    chunkSize?: unknown;
    bytesUsed?: unknown;
    accessCounter?: unknown;
  };

  if (rec.cacheKey !== cacheKey) return false;
  if (typeof rec.imageId !== "string" || rec.imageId !== signature.imageId) return false;
  if (typeof rec.version !== "string" || rec.version !== signature.version) return false;

  if (rec.etag !== undefined && rec.etag !== null && typeof rec.etag !== "string") return false;
  if ((rec.etag ?? null) !== signature.etag) return false;

  if (rec.lastModified !== undefined && rec.lastModified !== null && typeof rec.lastModified !== "string") return false;
  if ((rec.lastModified ?? null) !== signature.lastModified) return false;

  if (typeof rec.sizeBytes !== "number" || rec.sizeBytes !== signature.sizeBytes) return false;
  if (typeof rec.chunkSize !== "number" || rec.chunkSize !== signature.chunkSize) return false;
  if (!isSafeNonNegativeInt(rec.bytesUsed)) return false;
  if (!isSafeNonNegativeInt(rec.accessCounter)) return false;
  return true;
}

function expectedChunkByteLength(signature: RemoteChunkCacheSignature, chunkIndex: number): number {
  // Use BigInt to avoid overflow when chunkIndex is large; the final length is always <= chunkSize
  // (≤8 MiB), so converting back to number is safe.
  const offset = BigInt(chunkIndex) * BigInt(signature.chunkSize);
  const size = BigInt(signature.sizeBytes);
  if (offset >= size) return 0;
  const remaining = size - offset;
  const chunkSize = BigInt(signature.chunkSize);
  return Number(remaining < chunkSize ? remaining : chunkSize);
}

function bytesFromStoredChunkData(data: unknown): Uint8Array | null {
  if (data instanceof ArrayBuffer) return new Uint8Array(data);
  // Legacy/foreign implementations may persist `Uint8Array` instead of `ArrayBuffer`.
  if (data instanceof Uint8Array) return data;
  return null;
}

function arrayBufferExact(bytes: Uint8Array): ArrayBuffer {
  if (bytes.buffer instanceof ArrayBuffer && bytes.byteOffset === 0 && bytes.byteLength === bytes.buffer.byteLength) {
    return bytes.buffer;
  }
  // Copy into a new ArrayBuffer so persisted records always store the exact chunk payload.
  return bytes.slice().buffer;
}

function safeByteLengthFromChunkRecord(rec: unknown): number {
  if (!rec || typeof rec !== "object") return 0;
  const r = rec as { byteLength?: unknown; data?: unknown };
  const bytes = bytesFromStoredChunkData(r.data);
  if (bytes) return bytes.byteLength;
  if (isSafeNonNegativeInt(r.byteLength)) return r.byteLength;
  return 0;
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

      const rec = cursor.value as unknown;
      const chunkIndex = (rec as { chunkIndex?: unknown }).chunkIndex;
      const shouldSkip = protectedChunkIndex !== undefined && chunkIndex === protectedChunkIndex;
      if (shouldSkip) {
        cursor.continue();
        return;
      }

      cursor.delete();
      if (isSafeNonNegativeInt(chunkIndex)) {
        evicted.push(chunkIndex);
      }
      meta.bytesUsed = Math.max(0, meta.bytesUsed - safeByteLengthFromChunkRecord(rec));

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

    let db: IDBDatabase;
    try {
      db = await openDiskManagerDb();
    } catch (err) {
      if (isQuotaExceededError(err)) {
        throw new IdbRemoteChunkCacheQuotaError(undefined, { cause: err });
      }
      throw err;
    }
    const cache = new IdbRemoteChunkCache(db, opts.cacheKey, opts.signature, opts.cacheLimitBytes ?? null, maxCachedChunks);
    try {
      await cache.ensureCompatible();
      return cache;
    } catch (err) {
      // If initialization fails, ensure we don't leak the DB connection.
      db.close();
      if (isQuotaExceededError(err)) {
        throw new IdbRemoteChunkCacheQuotaError(undefined, { cause: err });
      }
      throw err;
    }
  }

  close(): void {
    this.pendingAccess.clear();
    this.cache.clear();
    this.db.close();
  }

  async clear(): Promise<void> {
    let tx: IDBTransaction | null = null;
    try {
      tx = this.db.transaction(["remote_chunks", "remote_chunk_meta"], "readwrite");
      const chunksStore = tx.objectStore("remote_chunks");
      const metaStore = tx.objectStore("remote_chunk_meta");
      await deleteAllChunksForCacheKey(chunksStore, this.cacheKey);
      metaStore.delete(this.cacheKey);
      await idbTxDone(tx);
    } catch (err) {
      if (isQuotaExceededError(err) || isQuotaExceededError(tx?.error)) {
        throw new IdbRemoteChunkCacheQuotaError(undefined, { cause: err });
      }
      throw err;
    } finally {
      // Always drop the in-memory cache to avoid serving stale bytes if the caller proceeds.
      this.pendingAccess.clear();
      this.cache.clear();
    }
  }

  async getStatus(): Promise<{ bytesUsed: number; cacheLimitBytes: number | null }> {
    let tx: IDBTransaction | null = null;
    try {
      tx = this.db.transaction(["remote_chunk_meta"], "readonly");
      const metaStore = tx.objectStore("remote_chunk_meta");
      const meta = (await idbReq(metaStore.get(this.cacheKey))) as unknown;
      await idbTxDone(tx);
      if (!isValidMetaRecord(meta, this.cacheKey, this.signature)) {
        return { bytesUsed: 0, cacheLimitBytes: this.cacheLimitBytes };
      }
      return { bytesUsed: meta.bytesUsed, cacheLimitBytes: this.cacheLimitBytes };
    } catch (err) {
      if (isQuotaExceededError(err) || isQuotaExceededError(tx?.error)) {
        throw new IdbRemoteChunkCacheQuotaError(undefined, { cause: err });
      }
      throw err;
    }
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
      const rec = (await idbReq(chunksStore.get([this.cacheKey, idx]))) as unknown;
      return { idx, rec };
    });
    const records = await Promise.all(reqs);

    for (const { idx, rec } of records) {
      if (!rec) continue;
      meta.accessCounter += 1;

      const bytes = bytesFromStoredChunkData((rec as { data?: unknown }).data);
      if (!bytes) {
        chunksStore.delete([this.cacheKey, idx]);
        meta.bytesUsed = Math.max(0, meta.bytesUsed - safeByteLengthFromChunkRecord(rec));
        continue;
      }

      const lastAccess = meta.accessCounter;
      const normalized: RemoteChunkRecord = {
        cacheKey: this.cacheKey,
        chunkIndex: idx,
        data: arrayBufferExact(bytes),
        byteLength: bytes.byteLength,
        lastAccess,
      };
      chunksStore.put(normalized);
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

    let tx: IDBTransaction;
    let chunksStore: IDBObjectStore;
    let metaStore: IDBObjectStore;
    try {
      tx = this.db.transaction(["remote_chunks", "remote_chunk_meta"], "readwrite");
      chunksStore = tx.objectStore("remote_chunks");
      metaStore = tx.objectStore("remote_chunk_meta");
    } catch (err) {
      // `getMany()` is used on the remote disk read path. Quota errors must not fail reads;
      // treat them as cache misses.
      if (isQuotaExceededError(err)) return out;
      throw err;
    }

    try {
      const meta = await this.getOrInitMetaAndMaybeClearInTx(metaStore, chunksStore);

      const reqs = missing.map(async (idx) => {
        const rec = (await idbReq(chunksStore.get([this.cacheKey, idx]))) as unknown;
        return { idx, rec };
      });
      const records = await Promise.all(reqs);

      for (const { idx, rec } of records) {
        if (!rec) continue;

        // Defensive: IndexedDB contents are untrusted/can be corrupt. Never serve data unless the
        // record is structurally valid and has the expected byte length for this chunk index.
        const recObj = rec as Partial<RemoteChunkRecord> & { cacheKey?: unknown; chunkIndex?: unknown; data?: unknown };
        if (recObj.cacheKey !== this.cacheKey || recObj.chunkIndex !== idx) {
          chunksStore.delete([this.cacheKey, idx]);
          meta.bytesUsed = Math.max(0, meta.bytesUsed - safeByteLengthFromChunkRecord(rec));
          continue;
        }

        const bytes = bytesFromStoredChunkData(recObj.data);
        const expectedLen = expectedChunkByteLength(this.signature, idx);
        if (!bytes || expectedLen === 0 || bytes.byteLength !== expectedLen) {
          // Corrupt/mismatched record: delete and treat as miss.
          chunksStore.delete([this.cacheKey, idx]);
          meta.bytesUsed = Math.max(0, meta.bytesUsed - safeByteLengthFromChunkRecord(rec));
          continue;
        }

        meta.accessCounter += 1;
        const lastAccess = meta.accessCounter;
        const healed: RemoteChunkRecord = {
          cacheKey: this.cacheKey,
          chunkIndex: idx,
          data: arrayBufferExact(bytes),
          byteLength: bytes.byteLength,
          lastAccess,
        };
        chunksStore.put(healed);
        out.set(idx, new Uint8Array(healed.data));
      }

      metaStore.put(meta);
      try {
        await idbTxDone(tx);
      } catch (err) {
        // Updating access metadata is best-effort. If the cache is at quota, the read path must
        // still succeed (and can fall back to network reads for misses).
        if (!isQuotaExceededError(err) && !isQuotaExceededError(tx.error)) throw err;
      }
    } catch (err) {
      // `getMany()` is used on the remote disk read path. Quota errors must not fail reads;
      // treat them as cache misses.
      if (!isQuotaExceededError(err) && !isQuotaExceededError(tx.error)) throw err;
    }

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

    const attemptPut = async (): Promise<void> => {
      const tx = this.db.transaction(["remote_chunks", "remote_chunk_meta"], "readwrite");
      const chunksStore = tx.objectStore("remote_chunks");
      const metaStore = tx.objectStore("remote_chunk_meta");

      try {
        const meta = await this.getOrInitMetaAndMaybeClearInTx(metaStore, chunksStore);
        await this.applyPendingAccessInTx(meta, chunksStore);

        const existing = (await idbReq(chunksStore.get([this.cacheKey, chunkIndex]))) as unknown;
        const oldBytes = safeByteLengthFromChunkRecord(existing);

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

        try {
          await idbTxDone(tx);
        } catch (err) {
          // Ensure we surface quota errors consistently even when the transaction abort error is not
          // the same object as the original failing request's error.
          if (isQuotaExceededError(tx.error) && !isQuotaExceededError(err)) {
            throw tx.error;
          }
          throw err;
        }

        if (this.maxCachedChunks > 0) {
          const cached = new Uint8Array(data);
          this.cache.set(chunkIndex, cached);
          this.touchCacheKey(chunkIndex, cached);
          for (const idx of evicted) this.cache.delete(idx);
          this.evictMemoryIfNeeded();
        }
      } catch (err) {
        // A QuotaExceededError can abort the transaction, causing subsequent requests in this
        // transaction to fail with AbortError/TransactionInactiveError instead of the original
        // quota exception. Consult `tx.error` so callers can reliably treat quota as non-fatal.
        if (isQuotaExceededError(tx.error) && !isQuotaExceededError(err)) {
          throw tx.error;
        }
        throw err;
      }
    };

    try {
      await attemptPut();
      return;
    } catch (err) {
      if (!isQuotaExceededError(err)) {
        throw err;
      }

      // If the cache is configured as unbounded, we have no safe eviction policy to
      // resolve the quota pressure here. Surface a typed error so callers can
      // treat the cache as disabled rather than failing the remote read.
      if (this.cacheLimitBytes === null) {
        throw new IdbRemoteChunkCacheQuotaError(undefined, { cause: err });
      }

      // Best-effort: evict older chunks to make room, then retry once. This helps
      // in environments where quota checks happen before we can commit the same-tx
      // eviction (or where the effective quota is smaller than our configured cap).
      try {
        // Evict aggressively: we don't know the true available quota (and it may be far smaller
        // than our configured cacheLimitBytes), so clearing the cache gives the best chance of
        // storing at least the currently requested chunk.
        await this.evictForQuotaInTx(0);
      } catch {
        // Best-effort eviction only; we'll retry once and then surface a typed error.
      }

      try {
        await attemptPut();
        return;
      } catch (err2) {
        if (isQuotaExceededError(err2)) {
          throw new IdbRemoteChunkCacheQuotaError(undefined, { cause: err2 });
        }
        throw err2;
      }
    }
  }

  async delete(chunkIndex: number): Promise<void> {
    if (!Number.isSafeInteger(chunkIndex) || chunkIndex < 0) {
      throw new Error(`chunkIndex must be a non-negative integer (got ${chunkIndex})`);
    }

    let tx: IDBTransaction;
    let chunksStore: IDBObjectStore;
    let metaStore: IDBObjectStore;
    try {
      tx = this.db.transaction(["remote_chunks", "remote_chunk_meta"], "readwrite");
      chunksStore = tx.objectStore("remote_chunks");
      metaStore = tx.objectStore("remote_chunk_meta");
    } catch (err) {
      // Best-effort healing; do not fail read paths if the cache DB is at quota.
      if (isQuotaExceededError(err)) {
        this.cache.delete(chunkIndex);
        this.pendingAccess.delete(chunkIndex);
        return;
      }
      throw err;
    }

    const meta = await this.getOrInitMetaAndMaybeClearInTx(metaStore, chunksStore);
    const existing = (await idbReq(chunksStore.get([this.cacheKey, chunkIndex]))) as unknown;
    if (existing) {
      chunksStore.delete([this.cacheKey, chunkIndex]);
      meta.bytesUsed = Math.max(0, meta.bytesUsed - safeByteLengthFromChunkRecord(existing));
      metaStore.put(meta);
    }

    try {
      await idbTxDone(tx);
    } catch (err) {
      // Healing deletes are best-effort; do not fail read paths if the cache DB is at quota.
      if (!isQuotaExceededError(err) && !isQuotaExceededError(tx.error)) throw err;
    } finally {
      this.cache.delete(chunkIndex);
      this.pendingAccess.delete(chunkIndex);
    }
  }

  async listChunkIndices(): Promise<number[]> {
    let tx: IDBTransaction | null = null;
    try {
      tx = this.db.transaction(["remote_chunks"], "readonly");
    } catch (err) {
      // Cache status queries are best-effort and should not fail due to quota.
      if (isQuotaExceededError(err)) return [];
      throw err;
    }

    const chunksStore = tx.objectStore("remote_chunks");
    const range = IDBKeyRange.bound([this.cacheKey, -Infinity], [this.cacheKey, Infinity]);
    const out: number[] = [];
    try {
      await new Promise<void>((resolve, reject) => {
        const req = chunksStore.openCursor(range);
        req.onerror = () => reject(req.error ?? new Error("IndexedDB cursor failed"));
        req.onsuccess = () => {
          const cursor = req.result;
          if (!cursor) {
            resolve();
            return;
          }
          const rec = cursor.value as unknown;
          const idx = (rec as { chunkIndex?: unknown }).chunkIndex;
          if (isSafeNonNegativeInt(idx)) out.push(idx);
          cursor.continue();
        };
      });
      await idbTxDone(tx);
    } catch (err) {
      // Best-effort: if the DB is at quota and the cursor/tx fails, report empty.
      if (isQuotaExceededError(err) || isQuotaExceededError(tx.error)) return [];
      throw err;
    }
    out.sort((a, b) => a - b);
    return out;
  }

  private async ensureCompatible(): Promise<void> {
    const tx = this.db.transaction(["remote_chunk_meta", "remote_chunks"], "readwrite");
    const metaStore = tx.objectStore("remote_chunk_meta");
    const chunksStore = tx.objectStore("remote_chunks");

    const meta = (await idbReq(metaStore.get(this.cacheKey))) as unknown;
    if (isValidMetaRecord(meta, this.cacheKey, this.signature) && signatureMatches(meta, this.signature)) {
      try {
        await idbTxDone(tx);
        return;
      } catch (err) {
        if (isQuotaExceededError(err) || isQuotaExceededError(tx.error)) {
          throw new IdbRemoteChunkCacheQuotaError(undefined, { cause: err });
        }
        throw err;
      }
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
    try {
      await idbTxDone(tx);
    } catch (err) {
      if (isQuotaExceededError(err) || isQuotaExceededError(tx.error)) {
        throw new IdbRemoteChunkCacheQuotaError(undefined, { cause: err });
      }
      throw err;
    }
  }

  private async getOrInitMetaAndMaybeClearInTx(
    metaStore: IDBObjectStore,
    chunksStore: IDBObjectStore,
  ): Promise<RemoteChunkCacheMetaRecord> {
    const meta = (await idbReq(metaStore.get(this.cacheKey))) as unknown;
    if (isValidMetaRecord(meta, this.cacheKey, this.signature) && signatureMatches(meta, this.signature)) return meta;

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

  private async evictForQuotaInTx(targetBytesUsed: number): Promise<void> {
    // Only meaningful for finite cache limits. Callers should check.
    const tx = this.db.transaction(["remote_chunks", "remote_chunk_meta"], "readwrite");
    const chunksStore = tx.objectStore("remote_chunks");
    const metaStore = tx.objectStore("remote_chunk_meta");

    const meta = await this.getOrInitMetaAndMaybeClearInTx(metaStore, chunksStore);

    const evicted = await enforceCacheLimitInTx({
      cacheKey: this.cacheKey,
      chunksStore,
      metaStore,
      meta,
      cacheLimitBytes: targetBytesUsed,
    });

    await idbTxDone(tx);

    for (const idx of evicted) {
      this.cache.delete(idx);
      this.pendingAccess.delete(idx);
    }
    this.evictMemoryIfNeeded();
  }
}
