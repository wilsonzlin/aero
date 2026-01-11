import { idbReq, idbTxDone, openDiskManagerDb } from "./metadata";

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
}): Promise<void> {
  const { cacheKey, chunksStore, metaStore, meta, cacheLimitBytes, protectedChunkIndex } = opts;
  if (meta.bytesUsed <= cacheLimitBytes) return;

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
      meta.bytesUsed = Math.max(0, meta.bytesUsed - (rec.byteLength ?? rec.data.byteLength));

      if (meta.bytesUsed <= cacheLimitBytes) {
        resolve();
        return;
      }

      cursor.continue();
    };
  });

  metaStore.put(meta);
}

/**
 * IndexedDB-backed cache for remote streaming disks.
 *
 * Data layout (in `aero-disk-manager`):
 * - Store: `remote_chunks` (keyPath: ["cacheKey", "chunkIndex"])
 * - Store: `remote_chunk_meta` (keyPath: "cacheKey")
 */
export class IdbRemoteChunkCache {
  private constructor(
    private readonly db: IDBDatabase,
    private readonly cacheKey: string,
    private readonly signature: RemoteChunkCacheSignature,
    private readonly cacheLimitBytes: number | null,
  ) {}

  static async open(opts: {
    cacheKey: string;
    signature: RemoteChunkCacheSignature;
    cacheLimitBytes?: number | null;
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

    const db = await openDiskManagerDb();
    const cache = new IdbRemoteChunkCache(db, opts.cacheKey, opts.signature, opts.cacheLimitBytes ?? null);
    await cache.ensureCompatible();
    return cache;
  }

  close(): void {
    this.db.close();
  }

  async clear(): Promise<void> {
    const tx = this.db.transaction(["remote_chunks", "remote_chunk_meta"], "readwrite");
    const chunksStore = tx.objectStore("remote_chunks");
    const metaStore = tx.objectStore("remote_chunk_meta");
    await deleteAllChunksForCacheKey(chunksStore, this.cacheKey);
    metaStore.delete(this.cacheKey);
    await idbTxDone(tx);
  }

  async getStatus(): Promise<{ bytesUsed: number; cacheLimitBytes: number | null }> {
    const tx = this.db.transaction(["remote_chunk_meta"], "readonly");
    const metaStore = tx.objectStore("remote_chunk_meta");
    const meta = (await idbReq(metaStore.get(this.cacheKey))) as RemoteChunkCacheMetaRecord | undefined;
    await idbTxDone(tx);
    return { bytesUsed: meta?.bytesUsed ?? 0, cacheLimitBytes: this.cacheLimitBytes };
  }

  async get(chunkIndex: number): Promise<Uint8Array | null> {
    if (!Number.isSafeInteger(chunkIndex) || chunkIndex < 0) {
      throw new Error(`chunkIndex must be a non-negative integer (got ${chunkIndex})`);
    }

    const tx = this.db.transaction(["remote_chunks", "remote_chunk_meta"], "readwrite");
    const chunksStore = tx.objectStore("remote_chunks");
    const metaStore = tx.objectStore("remote_chunk_meta");

    const meta = await this.getOrInitMetaInTx(metaStore);
    const rec = (await idbReq(chunksStore.get([this.cacheKey, chunkIndex]))) as RemoteChunkRecord | undefined;
    if (!rec) {
      await idbTxDone(tx);
      return null;
    }

    meta.accessCounter += 1;
    rec.lastAccess = meta.accessCounter;
    chunksStore.put(rec);
    metaStore.put(meta);

    await idbTxDone(tx);
    return new Uint8Array(rec.data);
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

    const meta = await this.getOrInitMetaInTx(metaStore);

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

    if (this.cacheLimitBytes !== null && meta.bytesUsed > this.cacheLimitBytes) {
      await enforceCacheLimitInTx({
        cacheKey: this.cacheKey,
        chunksStore,
        metaStore,
        meta,
        cacheLimitBytes: this.cacheLimitBytes,
        protectedChunkIndex: chunkIndex,
      });
    }

    await idbTxDone(tx);
  }

  async delete(chunkIndex: number): Promise<void> {
    if (!Number.isSafeInteger(chunkIndex) || chunkIndex < 0) {
      throw new Error(`chunkIndex must be a non-negative integer (got ${chunkIndex})`);
    }

    const tx = this.db.transaction(["remote_chunks", "remote_chunk_meta"], "readwrite");
    const chunksStore = tx.objectStore("remote_chunks");
    const metaStore = tx.objectStore("remote_chunk_meta");

    const meta = await this.getOrInitMetaInTx(metaStore);
    const existing = (await idbReq(chunksStore.get([this.cacheKey, chunkIndex]))) as RemoteChunkRecord | undefined;
    if (existing) {
      chunksStore.delete([this.cacheKey, chunkIndex]);
      meta.bytesUsed = Math.max(0, meta.bytesUsed - (existing.byteLength ?? existing.data.byteLength));
      metaStore.put(meta);
    }

    await idbTxDone(tx);
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

  private async getOrInitMetaInTx(metaStore: IDBObjectStore): Promise<RemoteChunkCacheMetaRecord> {
    const meta = (await idbReq(metaStore.get(this.cacheKey))) as RemoteChunkCacheMetaRecord | undefined;
    if (meta && signatureMatches(meta, this.signature)) return meta;

    // Either missing, or mismatched (should be rare here); treat as invalidation.
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
