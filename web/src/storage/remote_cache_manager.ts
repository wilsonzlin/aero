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
  abort?(): Promise<void>;
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

function normalizeOptionalHeader(v: string | null | undefined): string | undefined {
  if (typeof v !== "string") return undefined;
  const trimmed = v.trim();
  return trimmed.length > 0 ? trimmed : undefined;
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

function validateMeta(parsed: unknown): RemoteCacheMetaV1 | null {
  if (!parsed || typeof parsed !== "object") return null;
  const obj = parsed as Partial<RemoteCacheMetaV1>;
  if (obj.version !== META_VERSION) return null;
  if (typeof obj.imageId !== "string") return null;
  if (typeof obj.imageVersion !== "string") return null;
  if (typeof obj.deliveryType !== "string") return null;
  if (!obj.validators || typeof obj.validators !== "object") return null;
  if (typeof obj.validators.sizeBytes !== "number") return null;
  if (typeof obj.chunkSizeBytes !== "number") return null;
  if (typeof obj.createdAtMs !== "number") return null;
  if (typeof obj.lastAccessedAtMs !== "number") return null;
  if (obj.accessCounter !== undefined && typeof obj.accessCounter !== "number") return null;
  if (obj.chunkLastAccess !== undefined && (typeof obj.chunkLastAccess !== "object" || obj.chunkLastAccess === null)) {
    return null;
  }
  if (!Array.isArray(obj.cachedRanges)) return null;
  for (const r of obj.cachedRanges) {
    if (!r || typeof r !== "object") return null;
    const rr = r as Partial<ByteRange>;
    if (typeof rr.start !== "number" || typeof rr.end !== "number") return null;
  }
  return obj as RemoteCacheMetaV1;
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
    if (!parts.imageId) throw new Error("imageId must not be empty");
    if (!parts.version) throw new Error("version must not be empty");
    if (!parts.deliveryType) throw new Error("deliveryType must not be empty");

    // Version the key format so we can change it later without clobbering old caches.
    const material = JSON.stringify({
      keyVersion: 1,
      imageId: parts.imageId,
      version: parts.version,
      deliveryType: parts.deliveryType,
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
      const raw = await file.text();
      if (!raw.trim()) return null;
      const parsed = JSON.parse(raw) as unknown;
      return validateMeta(parsed);
    } catch (err) {
      if (isNotFoundError(err)) return null;
      // Corrupt/invalid meta: treat as absent so callers can invalidate/heal.
      return null;
    }
  }

  async writeMeta(cacheKey: string, meta: RemoteCacheMetaV1): Promise<void> {
    const dir = await this.getCacheDir(cacheKey, true);
    const handle = await dir.getFileHandle(META_FILE_NAME, { create: true });
    const writable = await handle.createWritable({ keepExistingData: false });
    await writable.write(JSON.stringify(meta, null, 2));
    await writable.close();
  }

  async openCache(
    parts: RemoteCacheKeyParts,
    opts: { chunkSizeBytes: number; validators: RemoteCacheValidators },
  ): Promise<{ cacheKey: string; dir: RemoteCacheDirectoryHandle; paths: RemoteCachePaths; meta: RemoteCacheMetaV1; invalidated: boolean }> {
    const cacheKey = await RemoteCacheManager.deriveCacheKey(parts);
    const now = this.now();
    const paths = this.getCachePaths(cacheKey);

    if (!Number.isSafeInteger(opts.chunkSizeBytes) || opts.chunkSizeBytes <= 0) {
      throw new Error(`invalid chunkSizeBytes=${opts.chunkSizeBytes}`);
    }
    if (!Number.isSafeInteger(opts.validators.sizeBytes) || opts.validators.sizeBytes <= 0) {
      throw new Error(`invalid validators.sizeBytes=${opts.validators.sizeBytes}`);
    }

    const expectedValidators: RemoteCacheMetaV1["validators"] = {
      sizeBytes: opts.validators.sizeBytes,
      etag: normalizeOptionalHeader(opts.validators.etag),
      lastModified: normalizeOptionalHeader(opts.validators.lastModified),
    };

    let invalidated = false;
    let meta = await this.readMeta(cacheKey);
    if (!meta || meta.chunkSizeBytes !== opts.chunkSizeBytes || !validatorsMatch(meta.validators, expectedValidators)) {
      invalidated = meta !== null;
      await this.clearCache(cacheKey);
      meta = {
        version: META_VERSION,
        imageId: parts.imageId,
        imageVersion: parts.version,
        deliveryType: parts.deliveryType,
        validators: expectedValidators,
        chunkSizeBytes: opts.chunkSizeBytes,
        createdAtMs: now,
        lastAccessedAtMs: now,
        accessCounter: 0,
        chunkLastAccess: {},
        cachedRanges: [],
      };
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
