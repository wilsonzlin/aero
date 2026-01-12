import { createSyncAccessHandleInDedicatedWorker } from "../../platform/opfs.js";

export type RemoteChunkCacheStats = {
  totalBytes: number;
  chunkCount: number;
  maxBytes: number | null;
};

export type RemoteChunkCachePutResult = {
  stored: boolean;
  evicted: number[];
};

export type RemoteChunkCacheSignature = {
  imageId: string;
  version: string;
  etag: string | null;
  sizeBytes: number;
  chunkSize: number;
};

/**
 * Backend interface for a persistent chunk cache.
 *
 * This is used by remote disk implementations that cache fixed-size chunks locally (OPFS/IDB).
 *
 * Canonical trait note:
 * This is an implementation-detail cache interface, not a disk abstraction. The canonical TS disk
 * interface is `AsyncSectorDisk`.
 *
 * See `docs/20-storage-trait-consolidation.md`.
 */
export interface RemoteChunkCacheBackend {
  getChunk(index: number, expectedByteLength: number): Promise<Uint8Array | null>;
  putChunk(index: number, data: Uint8Array): Promise<RemoteChunkCachePutResult>;
  getChunkIndices(): Promise<number[]>;
  getStats(): Promise<RemoteChunkCacheStats>;
  flush(): Promise<void>;
  clear(): Promise<void>;
}

type ChunkIndexV1 = {
  version: 1;
  signature?: RemoteChunkCacheSignature;
  chunkSize: number;
  accessCounter: number;
  chunks: Record<string, { byteLength: number; lastAccess: number }>;
};

// Defensive bound: index.json can be attacker-controlled/corrupt and should not be allowed to
// trigger arbitrarily large allocations/parses.
const MAX_INDEX_JSON_BYTES = 64 * 1024 * 1024; // 64 MiB
// Cap the number of entries we'll accept from an index file. This prevents pathological O(n) work
// on corrupt state even when the JSON file is within the byte limit.
const MAX_INDEX_CHUNK_ENTRIES = 1_000_000;

function splitPath(path: string): string[] {
  const trimmed = path.trim();
  const parts = trimmed.split("/").filter((p) => p.length > 0);
  if (parts.length === 0) return [];
  for (const part of parts) {
    if (part === "." || part === "..") {
      throw new Error('OPFS path must not contain "." or "..".');
    }
  }
  return parts;
}

function assertSafePathSegment(value: string, field: string): void {
  const trimmed = value.trim();
  if (!trimmed) throw new Error(`${field} must not be empty`);
  if (trimmed.includes("/")) throw new Error(`${field} must not contain '/'`);
  if (trimmed === "." || trimmed === "..") throw new Error(`${field} must not be '.' or '..'`);
}

async function safeRemoveEntry(
  dir: FileSystemDirectoryHandle,
  name: string,
  opts: { recursive?: boolean } = {},
): Promise<boolean> {
  try {
    await dir.removeEntry(name, { recursive: opts.recursive === true });
    return true;
  } catch (err) {
    if (err instanceof DOMException && err.name === "NotFoundError") return false;
    throw err;
  }
}

function isSafeNonNegativeInt(v: number): boolean {
  return Number.isSafeInteger(v) && v >= 0;
}

function toArrayBufferUint8(data: Uint8Array): Uint8Array<ArrayBuffer> {
  // Newer TS libdefs model typed arrays as `Uint8Array<ArrayBufferLike>`, while
  // OPFS `FileSystemWritableFileStream.write()` is still typed to accept only
  // `ArrayBuffer`-backed views. Most callers pass ArrayBuffer-backed values, so
  // avoid copies when possible.
  return data.buffer instanceof ArrayBuffer
    ? (data as unknown as Uint8Array<ArrayBuffer>)
    : new Uint8Array(data);
}

function signatureMatches(a: RemoteChunkCacheSignature | undefined, b: RemoteChunkCacheSignature): boolean {
  return (
    !!a &&
    a.imageId === b.imageId &&
    a.version === b.version &&
    a.etag === b.etag &&
    a.sizeBytes === b.sizeBytes &&
    a.chunkSize === b.chunkSize
  );
}

function validateSignature(signature: unknown): RemoteChunkCacheSignature | undefined {
  if (signature === undefined) return undefined;
  if (!signature || typeof signature !== "object") return undefined;
  const s = signature as Partial<RemoteChunkCacheSignature>;
  if (typeof s.imageId !== "string" || !s.imageId.trim()) return undefined;
  if (typeof s.version !== "string" || !s.version.trim()) return undefined;
  if (s.etag !== null && s.etag !== undefined && typeof s.etag !== "string") return undefined;
  if (typeof s.sizeBytes !== "number" || !Number.isSafeInteger(s.sizeBytes) || s.sizeBytes <= 0) return undefined;
  if (typeof s.chunkSize !== "number" || !Number.isSafeInteger(s.chunkSize) || s.chunkSize <= 0) return undefined;
  return {
    imageId: s.imageId,
    version: s.version,
    etag: s.etag ?? null,
    sizeBytes: s.sizeBytes,
    chunkSize: s.chunkSize,
  };
}

function validateIndex(parsed: unknown, expectedChunkSize: number): ChunkIndexV1 | null {
  if (!parsed || typeof parsed !== "object") return null;
  const obj = parsed as Partial<ChunkIndexV1>;
  if (obj.version !== 1) return null;
  if (typeof obj.chunkSize !== "number" || !Number.isSafeInteger(obj.chunkSize) || obj.chunkSize <= 0) return null;
  if (obj.chunkSize !== expectedChunkSize) return null;
  if (typeof obj.accessCounter !== "number" || !Number.isSafeInteger(obj.accessCounter) || obj.accessCounter < 0) return null;
  if (!obj.chunks || typeof obj.chunks !== "object" || Array.isArray(obj.chunks)) return null;

  const chunks = obj.chunks as Record<string, unknown>;
  let count = 0;
  for (const key in chunks) {
    count += 1;
    if (count > MAX_INDEX_CHUNK_ENTRIES) return null;
    const meta = chunks[key];
    if (!meta || typeof meta !== "object") return null;
    const byteLength = (meta as { byteLength?: unknown }).byteLength;
    const lastAccess = (meta as { lastAccess?: unknown }).lastAccess;
    if (typeof byteLength !== "number" || !Number.isSafeInteger(byteLength) || byteLength < 0) return null;
    if (typeof lastAccess !== "number" || !Number.isSafeInteger(lastAccess) || lastAccess < 0) return null;
  }

  const signature = validateSignature(obj.signature);
  if (obj.signature !== undefined && !signature) return null;

  return {
    version: 1,
    signature,
    chunkSize: obj.chunkSize,
    accessCounter: obj.accessCounter,
    chunks: obj.chunks as ChunkIndexV1["chunks"],
  };
}

/**
 * Persistent OPFS chunk cache that stores each chunk as an individual file so
 * eviction can reclaim quota (unlike a single ever-growing sparse file).
 *
 * Layout (default basePath="aero/disks/remote-cache"):
 * - <basePath>/<cacheKey>/index.json
 * - <basePath>/<cacheKey>/chunks/<chunkIndex>.bin
 */
export class OpfsLruChunkCache implements RemoteChunkCacheBackend {
  private readonly basePathParts: string[];
  private readonly cacheKey: string;
  private readonly chunkSize: number;
  private readonly maxBytes: number | null;
  private readonly expectedSignature: RemoteChunkCacheSignature | null;

  private rootDir: FileSystemDirectoryHandle | null = null;
  private baseDir: FileSystemDirectoryHandle | null = null;
  private chunksDir: FileSystemDirectoryHandle | null = null;

  private index: ChunkIndexV1 = {
    version: 1,
    chunkSize: 0,
    accessCounter: 0,
    chunks: {},
  };
  private totalBytes = 0;
  private dirty = false;

  private opChain: Promise<void> = Promise.resolve();

  private constructor(opts: {
    cacheKey: string;
    chunkSize: number;
    maxBytes: number | null;
    basePathParts: string[];
    expectedSignature: RemoteChunkCacheSignature | null;
  }) {
    this.cacheKey = opts.cacheKey;
    this.chunkSize = opts.chunkSize;
    this.maxBytes = opts.maxBytes;
    this.basePathParts = opts.basePathParts;
    this.expectedSignature = opts.expectedSignature;
  }

  static async open(opts: {
    cacheKey: string;
    chunkSize: number;
    maxBytes: number | null;
    /**
     * OPFS base path under the origin-private root. Defaults to `aero/disks/remote-cache`.
     */
    basePath?: string;
    signature?: RemoteChunkCacheSignature;
  }): Promise<OpfsLruChunkCache> {
    assertSafePathSegment(opts.cacheKey, "cacheKey");
    if (!Number.isSafeInteger(opts.chunkSize) || opts.chunkSize <= 0) throw new Error(`invalid chunkSize=${opts.chunkSize}`);
    if (opts.maxBytes !== null && (!Number.isSafeInteger(opts.maxBytes) || opts.maxBytes < 0)) {
      throw new Error(`invalid maxBytes=${opts.maxBytes}`);
    }

    if (opts.signature) {
      if (!opts.signature.imageId) throw new Error("signature.imageId must not be empty");
      if (!opts.signature.version) throw new Error("signature.version must not be empty");
      if (!Number.isSafeInteger(opts.signature.sizeBytes) || opts.signature.sizeBytes <= 0) {
        throw new Error(`signature.sizeBytes must be a positive safe integer (got ${opts.signature.sizeBytes})`);
      }
      if (opts.signature.chunkSize !== opts.chunkSize) {
        throw new Error(
          `signature.chunkSize mismatch: signature=${opts.signature.chunkSize} opts=${opts.chunkSize}`,
        );
      }
    }

    const basePathParts = splitPath(opts.basePath ?? "aero/disks/remote-cache");
    const cache = new OpfsLruChunkCache({
      cacheKey: opts.cacheKey,
      chunkSize: opts.chunkSize,
      maxBytes: opts.maxBytes,
      basePathParts,
      expectedSignature: opts.signature ?? null,
    });
    await cache.init();
    return cache;
  }

  private enqueue<T>(fn: () => Promise<T>): Promise<T> {
    const run = this.opChain.then(fn, fn) as Promise<T>;
    this.opChain = run.then(
      () => undefined,
      () => undefined,
    );
    return run;
  }

  private requireOpfs(): void {
    if (typeof navigator === "undefined") {
      throw new Error("OPFS is unavailable (navigator is undefined).");
    }
    const storage = navigator.storage;
    if (!storage) {
      throw new Error("OPFS is unavailable (navigator.storage is missing).");
    }
    const getDirectory = (storage as StorageManager & { getDirectory?: unknown }).getDirectory as
      | ((this: StorageManager) => Promise<FileSystemDirectoryHandle>)
      | undefined;
    if (!getDirectory) {
      throw new Error("OPFS is unavailable (navigator.storage.getDirectory is missing).");
    }
  }

  private async getRootDir(): Promise<FileSystemDirectoryHandle> {
    if (this.rootDir) return this.rootDir;
    this.requireOpfs();
    this.rootDir = await navigator.storage.getDirectory();
    return this.rootDir;
  }

  private async getOrCreateBaseDir(): Promise<FileSystemDirectoryHandle> {
    if (this.baseDir) return this.baseDir;
    let dir = await this.getRootDir();
    for (const part of [...this.basePathParts, this.cacheKey]) {
      dir = await dir.getDirectoryHandle(part, { create: true });
    }
    this.baseDir = dir;
    return dir;
  }

  private async getOrCreateChunksDir(): Promise<FileSystemDirectoryHandle> {
    if (this.chunksDir) return this.chunksDir;
    const base = await this.getOrCreateBaseDir();
    this.chunksDir = await base.getDirectoryHandle("chunks", { create: true });
    return this.chunksDir;
  }

  private chunkFileName(index: number): string {
    return `${index}.bin`;
  }

  private tmpFileSuffix(): string {
    return ".tmp";
  }

  private async readIndexFile(): Promise<ChunkIndexV1 | null> {
    const base = await this.getOrCreateBaseDir();
    try {
      const handle = await base.getFileHandle("index.json", { create: false });
      const file = await handle.getFile();
      const size = file.size;
      if (!Number.isFinite(size) || size <= 0) return null;
      if (size > MAX_INDEX_JSON_BYTES) {
        // Treat absurdly large indices as corrupt and rebuild from disk.
        return null;
      }
      const raw = await file.text();
      if (!raw.trim()) return null;
      const parsed = JSON.parse(raw) as unknown;
      return validateIndex(parsed, this.chunkSize);
    } catch (err) {
      if (err instanceof DOMException && err.name === "NotFoundError") return null;
      // JSON parse errors etc: treat as missing.
      return null;
    }
  }

  private normalizeIndex(): void {
    // Ensure accessCounter always monotonically increases beyond all lastAccess values.
    let maxAccess = this.index.accessCounter;
    for (const meta of Object.values(this.index.chunks)) {
      if (Number.isFinite(meta.lastAccess) && meta.lastAccess > maxAccess) {
        maxAccess = meta.lastAccess;
      }
    }
    this.index.accessCounter = maxAccess;
  }

  private recomputeTotalBytes(): number {
    let total = 0;
    for (const meta of Object.values(this.index.chunks)) {
      total += meta.byteLength;
    }
    return total;
  }

  private async persistIndexIfDirty(): Promise<void> {
    if (!this.dirty) return;
    const base = await this.getOrCreateBaseDir();
    const handle = await base.getFileHandle("index.json", { create: true });
    const writable = await handle.createWritable({ keepExistingData: false });
    await writable.write(JSON.stringify(this.index, null, 2));
    await writable.close();
    this.dirty = false;
  }

  private touch(index: number): void {
    this.index.accessCounter++;
    const meta = this.index.chunks[String(index)];
    if (!meta) return;
    meta.lastAccess = this.index.accessCounter;
    this.dirty = true;
  }

  private async evictIfNeeded(): Promise<number[]> {
    const evicted: number[] = [];
    if (this.maxBytes === null) return evicted;

    while (this.totalBytes > this.maxBytes) {
      let victim: number | null = null;
      let victimAccess = Number.POSITIVE_INFINITY;

      for (const [idxStr, meta] of Object.entries(this.index.chunks)) {
        const idx = Number(idxStr);
        if (!Number.isFinite(idx)) continue;
        const access = meta.lastAccess;
        if (access < victimAccess) {
          victimAccess = access;
          victim = idx;
          continue;
        }
        if (access === victimAccess && victim !== null && idx < victim) {
          victim = idx;
        }
      }

      if (victim === null) break;

      const victimKey = String(victim);
      const meta = this.index.chunks[victimKey];
      if (!meta) break;

      await this.deleteChunkFile(victim);
      delete this.index.chunks[victimKey];
      this.totalBytes -= meta.byteLength;
      evicted.push(victim);
      this.dirty = true;
    }

    // Clamp to avoid negative drift from unexpected size/accounting issues.
    if (this.totalBytes < 0) this.totalBytes = 0;

    return evicted;
  }

  private async deleteChunkFile(index: number): Promise<void> {
    const chunks = await this.getOrCreateChunksDir();
    await safeRemoveEntry(chunks, this.chunkFileName(index));
  }

  private async wipeLocalFiles(): Promise<void> {
    const base = await this.getOrCreateBaseDir();
    await safeRemoveEntry(base, "index.json");
    await safeRemoveEntry(base, "chunks", { recursive: true });
    this.baseDir = null;
    this.chunksDir = null;
  }

  private async init(): Promise<void> {
    await this.enqueue(async () => {
      const parsed = await this.readIndexFile();
      const hasExpected = this.expectedSignature !== null;
      const parsedLooksValid =
        !!parsed &&
        parsed.version === 1 &&
        Number.isSafeInteger(parsed.chunkSize) &&
        parsed.chunkSize > 0 &&
        Number.isSafeInteger(parsed.accessCounter) &&
        typeof parsed.chunks === "object" &&
        parsed.chunkSize === this.chunkSize;

      if (hasExpected && (!parsedLooksValid || !signatureMatches(parsed?.signature, this.expectedSignature!))) {
        // The directory name is stable across versions; if the signature doesn't match, the
        // chunk payloads cannot be trusted. Delete our cache files (but do not delete the
        // entire cacheKey directory; other remote cache components may share it).
        await this.wipeLocalFiles();
      }

      // Ensure the directories exist after any wipe.
      await this.getOrCreateBaseDir();
      await this.getOrCreateChunksDir();

      // Reload after any wipe so we don't accept stale metadata.
      const parsedAfterWipe = await this.readIndexFile();
      const parsedOk =
        !!parsedAfterWipe &&
        parsedAfterWipe.version === 1 &&
        Number.isSafeInteger(parsedAfterWipe.chunkSize) &&
        parsedAfterWipe.chunkSize > 0 &&
        Number.isSafeInteger(parsedAfterWipe.accessCounter) &&
        typeof parsedAfterWipe.chunks === "object" &&
        parsedAfterWipe.chunkSize === this.chunkSize &&
        (!hasExpected || signatureMatches(parsedAfterWipe.signature, this.expectedSignature!));

      if (parsedOk) {
        this.index = parsedAfterWipe;
      } else {
        // Incompatible/missing index: start fresh (but we'll reconcile using on-disk chunks below).
        this.index = {
          version: 1,
          signature: this.expectedSignature ?? undefined,
          chunkSize: this.chunkSize,
          accessCounter: 0,
          chunks: {},
        };
        this.dirty = true;
      }

      this.normalizeIndex();
      await this.reconcileWithFilesystem();
      await this.evictIfNeeded();
      await this.persistIndexIfDirty();
    });
  }

  private async reconcileWithFilesystem(): Promise<void> {
    const chunks = await this.getOrCreateChunksDir();

    const found = new Map<number, number>();
    for await (const [name, handle] of chunks.entries()) {
      if (handle.kind !== "file") continue;

      if (name.endsWith(this.tmpFileSuffix())) {
        await safeRemoveEntry(chunks, name);
        this.dirty = true;
        continue;
      }

      const m = /^(\d+)\.bin$/.exec(name);
      if (!m) continue;
      const idx = Number(m[1]);
      if (!isSafeNonNegativeInt(idx)) continue;

      const file = await (handle as FileSystemFileHandle).getFile();
      found.set(idx, file.size);
    }

    // Drop index entries whose files are missing.
    for (const idxStr of Object.keys(this.index.chunks)) {
      const idx = Number(idxStr);
      if (!Number.isFinite(idx)) continue;
      if (!found.has(idx)) {
        delete this.index.chunks[idxStr];
        this.dirty = true;
      }
    }

    // Add or update entries for orphan chunk files.
    for (const [idx, byteLength] of found) {
      const key = String(idx);
      const existing = this.index.chunks[key];
      if (!existing) {
        // Treat orphans as the oldest possible chunks so they're first to evict.
        this.index.chunks[key] = { byteLength, lastAccess: 0 };
        this.dirty = true;
        continue;
      }
      if (existing.byteLength !== byteLength) {
        existing.byteLength = byteLength;
        this.dirty = true;
      }
    }

    this.normalizeIndex();
    this.totalBytes = this.recomputeTotalBytes();
  }

  async getChunk(index: number, expectedByteLength: number): Promise<Uint8Array | null> {
    return await this.enqueue(async () => {
      if (!isSafeNonNegativeInt(index)) throw new Error(`invalid chunk index ${index}`);
      if (!Number.isSafeInteger(expectedByteLength) || expectedByteLength < 0) {
        throw new Error(`invalid expectedByteLength=${expectedByteLength}`);
      }

      const meta = this.index.chunks[String(index)];
      if (!meta) return null;

      const chunks = await this.getOrCreateChunksDir();
      try {
        const handle = await chunks.getFileHandle(this.chunkFileName(index), { create: false });
        const file = await handle.getFile();
        const actualSize = file.size;
        if (actualSize !== expectedByteLength || meta.byteLength !== actualSize) {
          // Corrupt/truncated/length-mismatched chunk: delete and treat as miss.
          await safeRemoveEntry(chunks, this.chunkFileName(index));
          delete this.index.chunks[String(index)];
          this.totalBytes -= meta.byteLength;
          this.dirty = true;
          await this.persistIndexIfDirty();
          return null;
        }
        const bytes = new Uint8Array(await file.arrayBuffer());
        if (bytes.byteLength !== expectedByteLength) {
          // Defensive: arrayBuffer length must match file.size, but be safe.
          await safeRemoveEntry(chunks, this.chunkFileName(index));
          delete this.index.chunks[String(index)];
          this.totalBytes -= meta.byteLength;
          this.dirty = true;
          await this.persistIndexIfDirty();
          return null;
        }

        this.touch(index);
        return bytes;
      } catch (err) {
        if (err instanceof DOMException && err.name === "NotFoundError") {
          // Index said present, but file is missing.
          delete this.index.chunks[String(index)];
          this.totalBytes -= meta.byteLength;
          this.dirty = true;
          await this.persistIndexIfDirty();
          return null;
        }
        throw err;
      }
    });
  }

  async putChunk(index: number, data: Uint8Array): Promise<RemoteChunkCachePutResult> {
    return await this.enqueue(async () => {
      if (!isSafeNonNegativeInt(index)) throw new Error(`invalid chunk index ${index}`);
      if (!(data instanceof Uint8Array)) throw new Error("data must be a Uint8Array");

      if (this.maxBytes !== null) {
        if (this.maxBytes === 0) return { stored: false, evicted: [] };
        if (data.byteLength > this.maxBytes) {
          // Too large to ever fit; skip caching entirely.
          return { stored: false, evicted: [] };
        }
      }

      const chunks = await this.getOrCreateChunksDir();
      const handle = await chunks.getFileHandle(this.chunkFileName(index), { create: true });
      const bytes = toArrayBufferUint8(data);

      // Prefer SyncAccessHandle writes when running inside a dedicated worker. This avoids building
      // `File` objects and is noticeably faster for high-frequency chunk caching.
      let sync: FileSystemSyncAccessHandle | null = null;
      try {
        sync = await createSyncAccessHandleInDedicatedWorker(handle);
      } catch {
        // Fall back to async writable stream (works on the main thread and in workers that
        // don't support SyncAccessHandle).
        sync = null;
      }
      if (sync) {
        try {
          sync.truncate(bytes.byteLength);
          const written = sync.write(bytes, { at: 0 });
          if (written !== bytes.byteLength) {
            throw new Error(`short cache write: expected=${bytes.byteLength} actual=${written}`);
          }
          sync.flush();
        } finally {
          sync.close();
        }
      } else {
        const writable = await handle.createWritable({ keepExistingData: false });
        try {
          await writable.write(bytes);
          await writable.close();
        } catch (err) {
          try {
            await writable.abort(err);
          } catch {
            // ignore abort failures
          }
          throw err;
        }
      }

      const key = String(index);
      const prev = this.index.chunks[key];
      if (prev) {
        this.totalBytes -= prev.byteLength;
      }
      this.index.chunks[key] = { byteLength: data.byteLength, lastAccess: 0 };
      this.totalBytes += data.byteLength;
      this.touch(index);

      const evicted = await this.evictIfNeeded();
      await this.persistIndexIfDirty();

      return { stored: !!this.index.chunks[key], evicted };
    });
  }

  async getChunkIndices(): Promise<number[]> {
    return await this.enqueue(async () => {
      const out: number[] = [];
      for (const key of Object.keys(this.index.chunks)) {
        const idx = Number(key);
        if (isSafeNonNegativeInt(idx)) out.push(idx);
      }
      out.sort((a, b) => a - b);
      return out;
    });
  }

  async getStats(): Promise<RemoteChunkCacheStats> {
    return await this.enqueue(async () => {
      return {
        totalBytes: this.totalBytes,
        chunkCount: Object.keys(this.index.chunks).length,
        maxBytes: this.maxBytes,
      };
    });
  }

  async flush(): Promise<void> {
    await this.enqueue(async () => {
      await this.persistIndexIfDirty();
    });
  }

  async clear(): Promise<void> {
    await this.enqueue(async () => {
      const root = await this.getRootDir();
      let dir: FileSystemDirectoryHandle = root;
      // Walk all but the last segment; if any directory is missing, we're already cleared.
      for (const part of this.basePathParts) {
        try {
          dir = await dir.getDirectoryHandle(part, { create: false });
        } catch (err) {
          if (err instanceof DOMException && err.name === "NotFoundError") {
            this.index = {
              version: 1,
              signature: this.expectedSignature ?? undefined,
              chunkSize: this.chunkSize,
              accessCounter: 0,
              chunks: {},
            };
            this.totalBytes = 0;
            this.dirty = false;
            this.baseDir = null;
            this.chunksDir = null;
            return;
          }
          throw err;
        }
      }

      await safeRemoveEntry(dir, this.cacheKey, { recursive: true });

      this.index = {
        version: 1,
        signature: this.expectedSignature ?? undefined,
        chunkSize: this.chunkSize,
        accessCounter: 0,
        chunks: {},
      };
      this.totalBytes = 0;
      this.dirty = false;
      this.baseDir = null;
      this.chunksDir = null;
    });
  }
}
