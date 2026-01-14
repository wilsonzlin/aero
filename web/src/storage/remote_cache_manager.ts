import { opfsGetRemoteCacheDir } from "./metadata.ts";

export type ByteRange = { start: number; end: number };

export type RemoteCacheKeyParts = {
  /**
   * Stable identifier for the logical disk image (e.g. database ID, object key, etc).
   *
   * Must NOT be derived from short-lived delivery secrets (signed URLs, bearer tokens).
   */
  imageId: string;
  /**
   * Stable version identifier for the disk image (e.g. generation number, snapshot ID).
   *
   * Must NOT be derived from delivery secrets (signed URLs, bearer tokens).
   */
  version: string;
  /**
   * Delivery scheme identifier (e.g. "range", "chunked", "gateway").
   *
   * Included to prevent collisions when the same image/version can be delivered
   * via different protocols with different caching semantics.
   */
  deliveryType: string;
};

export function remoteRangeDeliveryType(blockSizeBytes: number): string {
  if (!Number.isSafeInteger(blockSizeBytes) || blockSizeBytes <= 0) {
    throw new Error(`blockSizeBytes must be a positive safe integer (got ${blockSizeBytes})`);
  }
  return `range:${blockSizeBytes}`;
}

export function remoteChunkedDeliveryType(chunkSizeBytes: number): string {
  if (!Number.isSafeInteger(chunkSizeBytes) || chunkSizeBytes <= 0) {
    throw new Error(`chunkSizeBytes must be a positive safe integer (got ${chunkSizeBytes})`);
  }
  return `chunked:${chunkSizeBytes}`;
}

export type RemoteCacheValidators = {
  /**
   * Total expected size of the remote image in bytes.
   *
   * This MUST be provided (remote streaming can't work without a known size).
   */
  sizeBytes: number;
  /** `ETag` header value, if exposed via CORS / same-origin. */
  etag?: string | null;
  /** `Last-Modified` header value, if exposed via CORS / same-origin. */
  lastModified?: string | null;
};

export type RemoteCachePaths = {
  cacheDirName: string;
  baseCacheFileName: string;
  overlayFileName: string;
  metaFileName: string;
};

export type RemoteCacheMetaV1 = {
  version: 1;
  imageId: string;
  imageVersion: string;
  deliveryType: string;
  validators: {
    sizeBytes: number;
    etag?: string;
    lastModified?: string;
  };
  chunkSizeBytes: number;
  createdAtMs: number;
  lastAccessedAtMs: number;
  /**
   * Optional per-chunk LRU bookkeeping used by some remote disk implementations.
   *
   * These fields are not required for cache correctness, but allow stable eviction behavior
   * across page reloads when a cache size limit is enforced.
   */
  accessCounter?: number;
  chunkLastAccess?: Record<string, number>;
  cachedRanges: ByteRange[];
};

export type RemoteCacheStatus = {
  cacheKey: string;
  imageId: string;
  imageVersion: string;
  deliveryType: string;
  chunkSizeBytes: number;
  sizeBytes: number;
  etag?: string;
  lastModified?: string;
  createdAtMs: number;
  lastAccessedAtMs: number;
  cachedBytes: number;
  cachedRanges: ByteRange[];
  cachedChunks: number;
};

/**
 * Minimal file/dir handle interfaces used by {@link RemoteCacheManager}.
 *
 * These are intentionally OPFS-shaped so we can use OPFS directly in production, while tests (or
 * alternate environments) can provide lightweight in-memory implementations.
 *
 * Canonical trait note:
 * These are *not* disk abstractions. The canonical TS disk interface is `AsyncSectorDisk`.
 *
 * See `docs/20-storage-trait-consolidation.md`.
 */
export interface RemoteCacheFile {
  readonly size: number;
  text(): Promise<string>;
  arrayBuffer(): Promise<ArrayBuffer>;
}

export interface RemoteCacheWritableFileStream {
  write(data: string | Uint8Array): Promise<void>;
  close(): Promise<void>;
  abort?(reason?: unknown): Promise<void>;
  truncate?(size: number): Promise<void>;
}

export interface RemoteCacheFileHandle {
  getFile(): Promise<RemoteCacheFile>;
  createWritable(options?: { keepExistingData?: boolean }): Promise<RemoteCacheWritableFileStream>;
}

export interface RemoteCacheDirectoryHandle {
  getDirectoryHandle(name: string, options?: { create?: boolean }): Promise<RemoteCacheDirectoryHandle>;
  getFileHandle(name: string, options?: { create?: boolean }): Promise<RemoteCacheFileHandle>;
  removeEntry(name: string, options?: { recursive?: boolean }): Promise<void>;
  entries?(): AsyncIterable<[string, RemoteCacheDirectoryHandle | RemoteCacheFileHandle]>;
}

const META_VERSION = 1 as const;
const BASE_CACHE_FILE_NAME = "base.aerospar";
const OVERLAY_FILE_NAME = "overlay.aerospar";
const META_FILE_NAME = "meta.json";

// Defensive bound for cache metadata files stored on disk. OPFS/IndexedDB state can become corrupt
// (or attacker-controlled), so avoid reading/parsing arbitrarily large JSON blobs.
const MAX_CACHE_META_BYTES = 64 * 1024 * 1024; // 64 MiB

function normalizeOptionalHeader(v: string | null | undefined): string | undefined {
  if (typeof v !== "string") return undefined;
  const trimmed = v.trim();
  return trimmed.length > 0 ? trimmed : undefined;
}

function hasOwn(obj: object, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(obj, key);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function requireRemoteCacheKeyParts(raw: unknown): RemoteCacheKeyParts {
  // Treat key parts as untrusted (can come from snapshots, persisted state, or postMessage).
  // Ignore inherited fields (prototype pollution) and require well-typed own properties.
  if (!isRecord(raw)) {
    throw new Error("cache key parts must be an object");
  }
  const rec = raw as Record<string, unknown>;
  const imageId = hasOwn(rec, "imageId") ? rec.imageId : undefined;
  const version = hasOwn(rec, "version") ? rec.version : undefined;
  const deliveryType = hasOwn(rec, "deliveryType") ? rec.deliveryType : undefined;
  if (typeof imageId !== "string" || !imageId) throw new Error("imageId must not be empty");
  if (typeof version !== "string" || !version) throw new Error("version must not be empty");
  if (typeof deliveryType !== "string" || !deliveryType) throw new Error("deliveryType must not be empty");
  const out = Object.create(null) as RemoteCacheKeyParts;
  out.imageId = imageId;
  out.version = version;
  out.deliveryType = deliveryType;
  return out;
}

function isNotFoundError(err: unknown): boolean {
  if (!err || typeof err !== "object") return false;
  const name = (err as { name?: unknown }).name;
  return name === "NotFoundError";
}

function rangeLen(r: ByteRange): number {
  return r.end - r.start;
}

function overlapsOrAdjacent(a: ByteRange, b: ByteRange): boolean {
  return a.start <= b.end && b.start <= a.end;
}

function mergeRanges(a: ByteRange, b: ByteRange): ByteRange {
  return { start: Math.min(a.start, b.start), end: Math.max(a.end, b.end) };
}

function compactRanges(ranges: ByteRange[]): ByteRange[] {
  if (ranges.length <= 1) return ranges;
  const sorted = [...ranges].sort((a, b) => a.start - b.start);
  const out: ByteRange[] = [];
  let cur = sorted[0]!;
  for (const r of sorted.slice(1)) {
    if (overlapsOrAdjacent(cur, r)) {
      cur = mergeRanges(cur, r);
    } else {
      out.push(cur);
      cur = r;
    }
  }
  out.push(cur);
  return out;
}

function toArrayBufferUint8(data: Uint8Array): Uint8Array<ArrayBuffer> {
  // Newer TS libdefs model typed arrays as `Uint8Array<ArrayBufferLike>`, while
  // some Web APIs still accept only `ArrayBuffer`-backed views. Most callers pass
  // ArrayBuffer-backed values, so avoid copies when possible.
  return data.buffer instanceof ArrayBuffer
    ? (data as unknown as Uint8Array<ArrayBuffer>)
    : new Uint8Array(data);
}

export function validateRemoteCacheMetaV1(parsed: unknown): RemoteCacheMetaV1 | null {
  if (!parsed || typeof parsed !== "object") return null;
  const obj = parsed as Partial<RemoteCacheMetaV1>;
  if (!hasOwn(obj, "version") || obj.version !== META_VERSION) return null;
  if (!hasOwn(obj, "imageId") || typeof obj.imageId !== "string") return null;
  if (!hasOwn(obj, "imageVersion") || typeof obj.imageVersion !== "string") return null;
  if (!hasOwn(obj, "deliveryType") || typeof obj.deliveryType !== "string") return null;
  if (!hasOwn(obj, "validators") || !obj.validators || typeof obj.validators !== "object" || Array.isArray(obj.validators)) return null;
  const validators = obj.validators as Record<string, unknown>;
  if (!hasOwn(validators, "sizeBytes")) return null;
  const sizeBytes = (validators as Partial<RemoteCacheMetaV1["validators"]>).sizeBytes;
  if (typeof sizeBytes !== "number" || !Number.isSafeInteger(sizeBytes) || sizeBytes <= 0) return null;
  if (!hasOwn(obj, "chunkSizeBytes")) return null;
  const chunkSizeBytes = obj.chunkSizeBytes;
  if (typeof chunkSizeBytes !== "number" || !Number.isSafeInteger(chunkSizeBytes) || chunkSizeBytes <= 0) return null;
  if (!hasOwn(obj, "createdAtMs")) return null;
  const createdAtMs = obj.createdAtMs;
  if (typeof createdAtMs !== "number" || !Number.isSafeInteger(createdAtMs) || createdAtMs < 0) return null;
  if (!hasOwn(obj, "lastAccessedAtMs")) return null;
  const lastAccessedAtMs = obj.lastAccessedAtMs;
  if (
    typeof lastAccessedAtMs !== "number" ||
    !Number.isSafeInteger(lastAccessedAtMs) ||
    lastAccessedAtMs < 0
  )
    return null;
  const accessCounter = hasOwn(obj, "accessCounter") ? obj.accessCounter : undefined;
  if (accessCounter !== undefined) {
    if (!Number.isSafeInteger(accessCounter) || accessCounter < 0) return null;
  }
  const chunkLastAccess = hasOwn(obj, "chunkLastAccess") ? obj.chunkLastAccess : undefined;
  let parsedLastAccess: Record<string, number> | undefined = undefined;
  if (chunkLastAccess !== undefined) {
    if (typeof chunkLastAccess !== "object" || chunkLastAccess === null) return null;
    if (Array.isArray(chunkLastAccess)) return null;
    const lastAccess = chunkLastAccess as Record<string, unknown>;
    let entries = 0;
    const maxChunkIndex = Math.ceil(sizeBytes / chunkSizeBytes);
    parsedLastAccess = Object.create(null) as Record<string, number>;
    for (const key in lastAccess) {
      if (!Object.prototype.hasOwnProperty.call(lastAccess, key)) continue;
      entries += 1;
      // Defensive cap to avoid O(n) work on corrupt metadata.
      if (entries > 1_000_000) return null;
      // Chunk indices are stored as base-10 integer strings ("0", "1", ...). Treat any other keys
      // as corrupt so we can invalidate and rebuild the cache directory.
      // Keys are written using `String(chunkIndex)` ("0", "1", ...). Reject non-canonical numeric
      // encodings (e.g. "01") so corrupt metadata doesn't create duplicate indices.
      if (!/^(0|[1-9]\d*)$/.test(key)) return null;
      const idx = Number(key);
      if (!Number.isSafeInteger(idx) || idx < 0 || idx >= maxChunkIndex) return null;
      const value = lastAccess[key];
      if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) return null;
      parsedLastAccess[key] = value;
    }
  }
  if (!hasOwn(obj, "cachedRanges") || !Array.isArray(obj.cachedRanges)) return null;
  if (obj.cachedRanges.length > 1_000_000) return null;
  const cachedRanges: ByteRange[] = [];
  for (const r of obj.cachedRanges) {
    if (!r || typeof r !== "object") return null;
    const rr = r as Record<string, unknown>;
    if (!hasOwn(rr, "start") || !hasOwn(rr, "end")) return null;
    const start = (rr as Partial<ByteRange>).start;
    const end = (rr as Partial<ByteRange>).end;
    if (typeof start !== "number" || typeof end !== "number") return null;
    if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start < 0 || end < start) return null;
    if (end > sizeBytes) return null;
    cachedRanges.push({ start, end });
  }

  // Return a fully-sanitized metadata object with a null prototype so callers never observe
  // inherited properties (e.g. if `Object.prototype` is polluted).
  const out: RemoteCacheMetaV1 = Object.create(null) as RemoteCacheMetaV1;
  out.version = META_VERSION;
  out.imageId = obj.imageId;
  out.imageVersion = obj.imageVersion;
  out.deliveryType = obj.deliveryType;
  out.validators = Object.create(null) as RemoteCacheMetaV1["validators"];
  out.validators.sizeBytes = sizeBytes;
  const etag = (validators as Partial<RemoteCacheMetaV1["validators"]>).etag;
  const lastModified = (validators as Partial<RemoteCacheMetaV1["validators"]>).lastModified;
  if (typeof etag === "string") out.validators.etag = etag;
  if (typeof lastModified === "string") out.validators.lastModified = lastModified;
  out.chunkSizeBytes = chunkSizeBytes;
  out.createdAtMs = createdAtMs;
  out.lastAccessedAtMs = lastAccessedAtMs;
  if (accessCounter !== undefined) out.accessCounter = accessCounter;
  if (parsedLastAccess !== undefined) out.chunkLastAccess = parsedLastAccess;
  out.cachedRanges = cachedRanges;
  return out;
}

async function sha256Hex(data: Uint8Array<ArrayBuffer>): Promise<string> {
  // WebCrypto is available in modern browsers and Node 20+. If unavailable, fall back.
  const subtle = (globalThis as typeof globalThis & { crypto?: Crypto }).crypto?.subtle;
  if (subtle) {
    const digest = await subtle.digest("SHA-256", toArrayBufferUint8(data));
    const bytes = new Uint8Array(digest);
    return Array.from(bytes)
      .map((b) => b.toString(16).padStart(2, "0"))
      .join("");
  }

  // Fallback: FNV-1a 64-bit (not cryptographically strong, but stable and filesystem-safe).
  let hash = 0xcbf29ce484222325n;
  const prime = 0x100000001b3n;
  for (const b of data) {
    hash ^= BigInt(b);
    hash = (hash * prime) & 0xffffffffffffffffn;
  }
  return hash.toString(16).padStart(16, "0");
}

function validatorsMatch(a: RemoteCacheMetaV1["validators"], b: RemoteCacheMetaV1["validators"]): boolean {
  if (a.sizeBytes !== b.sizeBytes) return false;
  if (a.etag !== b.etag) return false;
  if (a.lastModified !== b.lastModified) return false;
  return true;
}

export class RemoteCacheManager {
  private readonly now: () => number;
  private readonly rootDir: RemoteCacheDirectoryHandle;

  constructor(rootDir: RemoteCacheDirectoryHandle, opts: { now?: () => number } = {}) {
    this.rootDir = rootDir;
    this.now = opts.now ?? (() => Date.now());
  }

  static async openOpfs(opts: { now?: () => number } = {}): Promise<RemoteCacheManager> {
    const dir = (await opfsGetRemoteCacheDir()) as unknown as RemoteCacheDirectoryHandle;
    return new RemoteCacheManager(dir, opts);
  }

  static async deriveCacheKey(parts: RemoteCacheKeyParts): Promise<string> {
    const safeParts = requireRemoteCacheKeyParts(parts);

    // Version the key format so we can change it later without clobbering old caches.
    const material = JSON.stringify({
      keyVersion: 1,
      imageId: safeParts.imageId,
      version: safeParts.version,
      deliveryType: safeParts.deliveryType,
    });
    const hex = await sha256Hex(new TextEncoder().encode(material));
    return `rc1_${hex}`;
  }

  getCachePaths(cacheKey: string): RemoteCachePaths {
    return {
      cacheDirName: cacheKey,
      baseCacheFileName: BASE_CACHE_FILE_NAME,
      overlayFileName: OVERLAY_FILE_NAME,
      metaFileName: META_FILE_NAME,
    };
  }

  private async getCacheDir(cacheKey: string, create: boolean): Promise<RemoteCacheDirectoryHandle> {
    return await this.rootDir.getDirectoryHandle(cacheKey, { create });
  }

  async readMeta(cacheKey: string): Promise<RemoteCacheMetaV1 | null> {
    const dir = await this.getCacheDir(cacheKey, false).catch((err) => {
      if (isNotFoundError(err)) return null;
      throw err;
    });
    if (!dir) return null;

    try {
      const handle = await dir.getFileHandle(META_FILE_NAME, { create: false });
      const file = await handle.getFile();
      if (file.size === 0) return null;
      if (file.size > MAX_CACHE_META_BYTES) {
        // Treat absurdly large meta files as corrupt and allow callers to invalidate.
        return null;
      }
      const raw = await file.text();
      if (!raw.trim()) return null;
      const parsed = JSON.parse(raw) as unknown;
      return validateRemoteCacheMetaV1(parsed);
    } catch (err) {
      if (isNotFoundError(err)) return null;
      // Corrupt/invalid meta: treat as absent so callers can invalidate/heal.
      return null;
    }
  }

  /**
   * Best-effort "touch" for an existing cache's meta.json to update `lastAccessedAtMs`.
   *
   * This intentionally does *not* create or delete any cache entries: it only updates metadata
   * when a valid meta.json already exists. Callers should treat failures as non-fatal (e.g. OPFS
   * quota errors, transient filesystem issues).
   */
  async touchMeta(cacheKey: string): Promise<void> {
    const meta = await this.readMeta(cacheKey);
    if (!meta) return;
    meta.lastAccessedAtMs = this.now();
    await this.writeMeta(cacheKey, meta);
  }

  async writeMeta(cacheKey: string, meta: RemoteCacheMetaV1): Promise<void> {
    const dir = await this.getCacheDir(cacheKey, true);
    const handle = await dir.getFileHandle(META_FILE_NAME, { create: true });
    let writable: RemoteCacheWritableFileStream;
    let truncateFallback = false;
    try {
      writable = await handle.createWritable({ keepExistingData: false });
    } catch {
      // Some implementations may not accept options; fall back to default.
      writable = await handle.createWritable();
      truncateFallback = true;
    }
    if (truncateFallback) {
      // Defensive: some implementations behave like `keepExistingData=true` when the options bag is
      // unsupported. Truncate explicitly so overwriting a shorter file doesn't leave trailing bytes.
      try {
        await writable.truncate?.(0);
      } catch {
        // ignore
      }
    }
    try {
      await writable.write(JSON.stringify(meta, null, 2));
      await writable.close();
    } catch (err) {
      try {
        await writable.abort?.(err);
      } catch {
        // ignore abort failures
      }
      throw err;
    }
  }

  async openCache(
    parts: RemoteCacheKeyParts,
    opts: { chunkSizeBytes: number; validators: RemoteCacheValidators },
  ): Promise<{ cacheKey: string; dir: RemoteCacheDirectoryHandle; paths: RemoteCachePaths; meta: RemoteCacheMetaV1; invalidated: boolean }> {
    const safeParts = requireRemoteCacheKeyParts(parts);
    const cacheKey = await RemoteCacheManager.deriveCacheKey(safeParts);
    const now = this.now();
    const paths = this.getCachePaths(cacheKey);

    if (!Number.isSafeInteger(opts.chunkSizeBytes) || opts.chunkSizeBytes <= 0) {
      throw new Error(`invalid chunkSizeBytes=${opts.chunkSizeBytes}`);
    }
    if (!Number.isSafeInteger(opts.validators.sizeBytes) || opts.validators.sizeBytes <= 0) {
      throw new Error(`invalid validators.sizeBytes=${opts.validators.sizeBytes}`);
    }

    const expectedValidators: RemoteCacheMetaV1["validators"] = Object.create(null) as RemoteCacheMetaV1["validators"];
    expectedValidators.sizeBytes = opts.validators.sizeBytes;
    const expectedEtag = normalizeOptionalHeader(opts.validators.etag);
    if (expectedEtag !== undefined) expectedValidators.etag = expectedEtag;
    const expectedLastModified = normalizeOptionalHeader(opts.validators.lastModified);
    if (expectedLastModified !== undefined) expectedValidators.lastModified = expectedLastModified;

    let invalidated = false;
    let meta = await this.readMeta(cacheKey);
    if (!meta || meta.chunkSizeBytes !== opts.chunkSizeBytes || !validatorsMatch(meta.validators, expectedValidators)) {
      invalidated = meta !== null;
      await this.clearCache(cacheKey);
      const next = Object.create(null) as RemoteCacheMetaV1;
      next.version = META_VERSION;
      next.imageId = safeParts.imageId;
      next.imageVersion = safeParts.version;
      next.deliveryType = safeParts.deliveryType;
      next.validators = expectedValidators;
      next.chunkSizeBytes = opts.chunkSizeBytes;
      next.createdAtMs = now;
      next.lastAccessedAtMs = now;
      next.accessCounter = 0;
      next.chunkLastAccess = Object.create(null) as Record<string, number>;
      next.cachedRanges = [];
      meta = next;
      await this.writeMeta(cacheKey, meta);
    } else {
      // Touch: update the access timestamp (do not mutate cached ranges).
      meta.lastAccessedAtMs = now;
      await this.writeMeta(cacheKey, meta);
    }

    const dir = await this.getCacheDir(cacheKey, true);
    return { cacheKey, dir, paths, meta, invalidated };
  }

  async recordCachedRange(cacheKey: string, start: number, end: number): Promise<void> {
    if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start < 0 || end < 0 || end < start) {
      throw new Error(`invalid range start=${start} end=${end}`);
    }
    const meta = await this.readMeta(cacheKey);
    if (!meta) throw new Error(`cache meta missing for ${cacheKey}`);

    const nextRanges = compactRanges([...meta.cachedRanges, { start, end }]);
    meta.cachedRanges = nextRanges;
    meta.lastAccessedAtMs = this.now();
    await this.writeMeta(cacheKey, meta);
  }

  async getCacheStatus(cacheKey: string): Promise<RemoteCacheStatus | null> {
    const meta = await this.readMeta(cacheKey);
    if (!meta) return null;

    const cachedBytes = meta.cachedRanges.reduce((sum, r) => sum + rangeLen(r), 0);
    const cachedChunks = meta.cachedRanges.reduce((sum, r) => sum + Math.ceil(rangeLen(r) / meta.chunkSizeBytes), 0);
    return {
      cacheKey,
      imageId: meta.imageId,
      imageVersion: meta.imageVersion,
      deliveryType: meta.deliveryType,
      chunkSizeBytes: meta.chunkSizeBytes,
      sizeBytes: meta.validators.sizeBytes,
      etag: meta.validators.etag,
      lastModified: meta.validators.lastModified,
      createdAtMs: meta.createdAtMs,
      lastAccessedAtMs: meta.lastAccessedAtMs,
      cachedBytes,
      cachedRanges: [...meta.cachedRanges],
      cachedChunks,
    };
  }

  async clearCache(cacheKey: string): Promise<void> {
    try {
      await this.rootDir.removeEntry(cacheKey, { recursive: true });
    } catch (err) {
      if (isNotFoundError(err)) return;
      throw err;
    }
  }

  async clearAllRemoteCaches(): Promise<void> {
    const entries = this.rootDir.entries?.bind(this.rootDir);
    if (!entries) {
      throw new Error("clearAllRemoteCaches requires directory iteration support");
    }
    for await (const [name] of entries()) {
      await this.clearCache(name);
    }
  }
}
