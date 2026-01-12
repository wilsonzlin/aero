import { openFileHandle, removeOpfsEntry } from "../platform/opfs.ts";
import { RangeSet, type ByteRange, type RemoteDiskTelemetrySnapshot } from "../platform/remote_disk";
import { assertSectorAligned, checkedOffset, SECTOR_SIZE, type AsyncSectorDisk } from "./disk";
import { IdbRemoteChunkCache } from "./idb_remote_chunk_cache";
import { RemoteCacheManager, remoteChunkedDeliveryType, type RemoteCacheDirectoryHandle, type RemoteCacheFile, type RemoteCacheFileHandle, type RemoteCacheKeyParts, type RemoteCacheMetaV1, type RemoteCacheWritableFileStream } from "./remote_cache_manager";
import { OPFS_AERO_DIR, OPFS_DISKS_DIR, OPFS_REMOTE_CACHE_DIR, pickDefaultBackend, type DiskBackend } from "./metadata";
import { readJsonResponseWithLimit, readResponseBytesWithLimit, ResponseTooLargeError } from "./response_json";
import {
  DEFAULT_LEASE_REFRESH_MARGIN_MS,
  DiskAccessLeaseRefresher,
  fetchWithDiskAccessLease,
  fetchWithDiskAccessLeaseForUrl,
  type DiskAccessLease,
} from "./disk_access_lease";

/**
 * Defensive bounds to avoid pathological allocations / fetch buffers when handling untrusted
 * remote manifests.
 *
 * Keep in sync with the Rust snapshot bounds where sensible.
 */
export const MAX_REMOTE_CHUNK_SIZE_BYTES = 64 * 1024 * 1024; // 64 MiB
/**
 * Upper bound on manifest chunk count to avoid allocating massive JS arrays.
 *
 * 500k chunks supports multi-terabyte images with typical chunk sizes while still preventing
 * unbounded allocations on malicious inputs.
 */
export const MAX_REMOTE_CHUNK_COUNT = 500_000;
// Defensive bound: avoid reading/parsing arbitrarily large manifest JSON blobs. This must be large
// enough to support realistic chunk counts while still preventing runaway allocations from
// malicious servers.
export const MAX_REMOTE_MANIFEST_JSON_BYTES = 64 * 1024 * 1024; // 64 MiB
// Defensive bounds for user-provided tuning knobs. These values can come from untrusted snapshot
// metadata or external configuration, so keep them bounded to avoid pathological background work
// (e.g. thousands of background chunk prefetches or hundreds of concurrent 64MiB downloads).
const MAX_REMOTE_PREFETCH_SEQUENTIAL_CHUNKS = 1024;
const MAX_REMOTE_PREFETCH_SEQUENTIAL_BYTES = 512 * 1024 * 1024; // 512 MiB
const MAX_REMOTE_MAX_ATTEMPTS = 32;
const MAX_REMOTE_MAX_CONCURRENT_FETCHES = 128;
const MAX_REMOTE_INFLIGHT_BYTES = 512 * 1024 * 1024; // 512 MiB

export type ChunkedDiskManifestV1 = {
  schema: "aero.chunked-disk-image.v1";
  imageId?: string;
  version: string;
  mimeType: string;
  totalSize: number;
  chunkSize: number;
  chunkCount: number;
  chunkIndexWidth: number;
  chunks?: Array<{ size?: number; sha256?: string }>;
};

type ParsedChunkedDiskManifest = {
  totalSize: number;
  chunkSize: number;
  chunkCount: number;
  chunkIndexWidth: number;
  chunkSizes: number[];
  chunkSha256: Array<string | null>;
};

type RemoteChunkedDiskCacheMeta = RemoteCacheMetaV1;

export type RemoteChunkedDiskOptions = {
  /**
   * Fetch credential mode for manifest + chunk GETs.
   *
   * - `same-origin` (default): send cookies for same-origin requests only.
   * - `include`: send cookies for cross-origin too (requires CORS with credentials).
   * - `omit`: never send cookies (useful for signed URL/cookie setups).
   */
  credentials?: RequestCredentials;
  /**
   * Persistent cache size limit (LRU-evicted).
   *
   * - `undefined` (default): 512 MiB
   * - `null`: no eviction (unbounded cache; subject to browser storage quota)
   * - `0`: disable caching entirely (chunks are not persisted)
   */
  cacheLimitBytes?: number | null;
  /**
   * Max concurrent network fetches for chunk GETs.
   */
  maxConcurrentFetches?: number;
  /**
   * When reads are sequential, prefetch the next N chunks (best-effort).
   */
  prefetchSequentialChunks?: number;
  /**
   * Maximum number of fetch attempts per chunk (includes the first attempt).
   */
  maxAttempts?: number;
  /**
   * Initial retry delay (exponential backoff; attempt 2 waits `retryBaseDelayMs`).
   */
  retryBaseDelayMs?: number;
  /**
   * Override the cache store. Intended for tests.
   */
  store?: BinaryStore;
  /**
   * Cache backend selection (defaults to `pickDefaultBackend()`).
   *
   * When set to `"idb"`, cached chunks are stored in the DiskManager IndexedDB
   * database (persistent even without OPFS).
   */
  cacheBackend?: DiskBackend;
  /**
   * Stable cache identity for the remote disk (used as `imageId` in cache key derivation).
   *
   * This should be a control-plane identifier (e.g. database ID), not a signed URL.
   * Defaults to the manifest `imageId` when present, otherwise a normalized manifest URL
   * without query/hash components.
   */
  cacheImageId?: string;
  /**
   * Stable version identifier for the remote disk (used as `version` in cache key derivation).
   *
   * Defaults to the manifest `version`.
   */
  cacheVersion?: string;
  /**
   * For lease-based access, refresh shortly before `expiresAt`.
   */
  leaseRefreshMarginMs?: number;
};

/**
 * `RemoteChunkedDiskOptions` safe to send across `postMessage` boundaries.
 * (The `store` option allows injecting a test store instance and is not transferable.)
 */
export type RemoteChunkedDiskOpenOptions = Omit<RemoteChunkedDiskOptions, "store">;

/**
 * Minimal async byte-store interface used by the chunked remote disk cache.
 *
 * This exists so the implementation can be tested against an in-memory store without depending on
 * OPFS. It is not intended to be a general disk interface; prefer `AsyncSectorDisk` for disks.
 *
 * See `docs/20-storage-trait-consolidation.md`.
 */
export interface BinaryStore {
  read(path: string): Promise<Uint8Array<ArrayBuffer> | null>;
  write(path: string, data: Uint8Array<ArrayBuffer>): Promise<void>;
  remove(path: string, options?: { recursive?: boolean }): Promise<void>;
}

function toArrayBufferUint8(data: Uint8Array): Uint8Array<ArrayBuffer> {
  // Newer TS libdefs model typed arrays as `Uint8Array<ArrayBufferLike>`, while
  // some Web APIs still accept only `ArrayBuffer`-backed views. Most callers
  // pass ArrayBuffer-backed chunks, so avoid copies when possible.
  return data.buffer instanceof ArrayBuffer
    ? (data as unknown as Uint8Array<ArrayBuffer>)
    : new Uint8Array(data);
}

class MemoryStore implements BinaryStore {
  private readonly files = new Map<string, Uint8Array<ArrayBuffer>>();

  async read(path: string): Promise<Uint8Array<ArrayBuffer> | null> {
    const data = this.files.get(path);
    return data ? data.slice() : null;
  }

  async write(path: string, data: Uint8Array<ArrayBuffer>): Promise<void> {
    this.files.set(path, data.slice());
  }

  async remove(path: string, _options: { recursive?: boolean } = {}): Promise<void> {
    // Very small best-effort: support prefix delete for recursive removes.
    if (_options.recursive) {
      const prefix = path.endsWith("/") ? path : `${path}/`;
      for (const key of Array.from(this.files.keys())) {
        if (key === path || key.startsWith(prefix)) this.files.delete(key);
      }
      return;
    }
    this.files.delete(path);
  }
}

class OpfsStore implements BinaryStore {
  async read(path: string): Promise<Uint8Array<ArrayBuffer> | null> {
    try {
      const handle = await openFileHandle(path, { create: false });
      const file = await handle.getFile();
      return new Uint8Array(await file.arrayBuffer());
    } catch {
      return null;
    }
  }

  async write(path: string, data: Uint8Array<ArrayBuffer>): Promise<void> {
    const handle = await openFileHandle(path, { create: true });
    const writable = await handle.createWritable({ keepExistingData: false });
    await writable.write(toArrayBufferUint8(data));
    await writable.close();
  }

  async remove(path: string, options: { recursive?: boolean } = {}): Promise<void> {
    await removeOpfsEntry(path, options);
  }
}

const REMOTE_CACHE_ROOT_PATH = `${OPFS_AERO_DIR}/${OPFS_DISKS_DIR}/${OPFS_REMOTE_CACHE_DIR}`;

class StoreNotFoundError extends Error {
  override name = "NotFoundError";
}

function joinOpfsPath(prefix: string, name: string): string {
  if (!prefix) return name;
  return `${prefix}/${name}`;
}

function rangesEqual(a: ByteRange[], b: ByteRange[]): boolean {
  if (a.length !== b.length) return false;
  for (let i = 0; i < a.length; i += 1) {
    if (a[i]!.start !== b[i]!.start || a[i]!.end !== b[i]!.end) return false;
  }
  return true;
}

class StoreFile implements RemoteCacheFile {
  constructor(private readonly data: Uint8Array) {}

  get size(): number {
    return this.data.byteLength;
  }

  async text(): Promise<string> {
    return new TextDecoder().decode(this.data);
  }

  async arrayBuffer(): Promise<ArrayBuffer> {
    return this.data.slice().buffer;
  }
}

class StoreWritable implements RemoteCacheWritableFileStream {
  private readonly chunks: Uint8Array[] = [];
  private closed = false;

  constructor(
    private readonly store: BinaryStore,
    private readonly path: string,
    baseData?: Uint8Array,
  ) {
    if (baseData && baseData.byteLength > 0) {
      this.chunks.push(baseData);
    }
  }

  async write(data: string | Uint8Array): Promise<void> {
    if (this.closed) throw new Error("writable already closed");
    if (typeof data === "string") {
      this.chunks.push(new TextEncoder().encode(data));
    } else {
      this.chunks.push(data);
    }
  }

  async close(): Promise<void> {
    if (this.closed) return;
    this.closed = true;
    const total = this.chunks.reduce((sum, c) => sum + c.byteLength, 0);
    const out = new Uint8Array(total);
    let off = 0;
    for (const c of this.chunks) {
      out.set(c, off);
      off += c.byteLength;
    }
    await this.store.write(this.path, out);
  }
}

class StoreFileHandle implements RemoteCacheFileHandle {
  constructor(
    private readonly store: BinaryStore,
    private readonly path: string,
  ) {}

  async getFile(): Promise<RemoteCacheFile> {
    const bytes = await this.store.read(this.path);
    if (!bytes) throw new StoreNotFoundError(`missing file: ${this.path}`);
    return new StoreFile(bytes);
  }

  async createWritable(options?: { keepExistingData?: boolean }): Promise<RemoteCacheWritableFileStream> {
    const base =
      options?.keepExistingData === true
        ? await this.store.read(this.path).then((b) => (b ? b.slice() : undefined))
        : undefined;
    return new StoreWritable(this.store, this.path, base);
  }
}

class StoreDirHandle implements RemoteCacheDirectoryHandle {
  constructor(
    private readonly store: BinaryStore,
    private readonly prefix: string,
  ) {}

  async getDirectoryHandle(name: string, _options?: { create?: boolean }): Promise<RemoteCacheDirectoryHandle> {
    return new StoreDirHandle(this.store, joinOpfsPath(this.prefix, name));
  }

  async getFileHandle(name: string, _options?: { create?: boolean }): Promise<RemoteCacheFileHandle> {
    return new StoreFileHandle(this.store, joinOpfsPath(this.prefix, name));
  }

  async removeEntry(name: string, options?: { recursive?: boolean }): Promise<void> {
    await this.store.remove(joinOpfsPath(this.prefix, name), { recursive: options?.recursive === true });
  }
}

type ChunkCache = {
  getChunk(chunkIndex: number): Promise<Uint8Array<ArrayBuffer> | null>;
  putChunk(chunkIndex: number, bytes: Uint8Array<ArrayBuffer>): Promise<void>;
  /**
   * Best-effort batched cache read to reduce IndexedDB roundtrips.
   *
   * Callers should treat this as an optimization; cache misses are expected.
   */
  prefetchChunks?(chunkIndices: number[]): Promise<void>;
  getCachedBytes(): number;
  getCacheLimitBytes(): number | null;
  flush(): Promise<void>;
  clear(): Promise<void>;
  close?: () => void;
};

class NoopChunkCache implements ChunkCache {
  async getChunk(_chunkIndex: number): Promise<Uint8Array<ArrayBuffer> | null> {
    return null;
  }

  async putChunk(_chunkIndex: number, _bytes: Uint8Array<ArrayBuffer>): Promise<void> {
    // no-op
  }

  async prefetchChunks(_chunkIndices: number[]): Promise<void> {
    // no-op
  }

  getCachedBytes(): number {
    return 0;
  }

  getCacheLimitBytes(): number | null {
    return 0;
  }

  async flush(): Promise<void> {
    // no-op
  }

  async clear(): Promise<void> {
    // no-op
  }

  close(): void {
    // no-op
  }
}

class IdbChunkCache implements ChunkCache {
  private cachedBytes: number;
  private readonly cacheLimitBytes: number | null;

  constructor(
    private readonly cache: IdbRemoteChunkCache,
    private readonly manifest: ParsedChunkedDiskManifest,
    initialStatus: { bytesUsed: number; cacheLimitBytes: number | null },
  ) {
    this.cachedBytes = initialStatus.bytesUsed;
    this.cacheLimitBytes = initialStatus.cacheLimitBytes;
  }

  getCachedBytes(): number {
    return this.cachedBytes;
  }

  getCacheLimitBytes(): number | null {
    return this.cacheLimitBytes;
  }

  private expectedLen(chunkIndex: number): number {
    return this.manifest.chunkSizes[chunkIndex] ?? 0;
  }

  async getChunk(chunkIndex: number): Promise<Uint8Array<ArrayBuffer> | null> {
    const expectedLen = this.expectedLen(chunkIndex);
    const bytes = await this.cache.get(chunkIndex);
    if (!bytes) return null;
    if (bytes.byteLength !== expectedLen) {
      // Heal: cached but mismatched size (stale/corrupt record).
      await this.cache.delete(chunkIndex);
      // Best-effort: refresh cachedBytes after a heal.
      const status = await this.cache.getStatus();
      this.cachedBytes = status.bytesUsed;
      return null;
    }
    // Stored in IndexedDB as an ArrayBuffer, so this is safe.
    return bytes as Uint8Array<ArrayBuffer>;
  }

  async prefetchChunks(chunkIndices: number[]): Promise<void> {
    await this.cache.getMany(chunkIndices);
  }

  async putChunk(chunkIndex: number, bytes: Uint8Array<ArrayBuffer>): Promise<void> {
    const expectedLen = this.expectedLen(chunkIndex);
    if (bytes.byteLength !== expectedLen) {
      throw new Error(`chunk ${chunkIndex} length mismatch: expected=${expectedLen} actual=${bytes.byteLength}`);
    }
    await this.cache.put(chunkIndex, bytes);
    const status = await this.cache.getStatus();
    this.cachedBytes = status.bytesUsed;
  }

  async flush(): Promise<void> {
    // All writes are durable per-transaction.
  }

  async clear(): Promise<void> {
    await this.cache.clear();
    this.cachedBytes = 0;
  }

  close(): void {
    this.cache.close();
  }
}

class Semaphore {
  private available: number;
  private readonly waiters: Array<(release: () => void) => void> = [];

  constructor(capacity: number) {
    if (!Number.isSafeInteger(capacity) || capacity <= 0) {
      throw new Error(`invalid semaphore capacity=${capacity}`);
    }
    this.available = capacity;
  }

  async acquire(): Promise<() => void> {
    if (this.available > 0) {
      this.available -= 1;
      return () => this.release();
    }
    return await new Promise((resolve) => {
      this.waiters.push(resolve);
    });
  }

  private release(): void {
    this.available += 1;
    const next = this.waiters.shift();
    if (next) {
      this.available -= 1;
      next(() => this.release());
    }
  }
}

class ChunkFetchError extends Error {
  override name = "ChunkFetchError";
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
  }
}

class IntegrityError extends Error {
  override name = "IntegrityError";
}

function hasOpfsRoot(): boolean {
  return typeof navigator !== "undefined" && typeof navigator.storage?.getDirectory === "function";
}

function asSafeInt(value: unknown, label: string): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value)) {
    throw new Error(`${label} must be a safe integer`);
  }
  return value;
}

function toSafeNumber(value: bigint, label: string): number {
  const n = Number(value);
  if (!Number.isSafeInteger(n)) {
    throw new Error(`${label} is not a safe JS integer (${value})`);
  }
  return n;
}

function divCeil(n: number, d: number): number {
  if (!Number.isSafeInteger(n) || n < 0 || !Number.isSafeInteger(d) || d <= 0) {
    throw new Error("divCeil: arguments must be safe non-negative integers and divisor must be > 0");
  }
  const out = Number((BigInt(n) + BigInt(d) - 1n) / BigInt(d));
  if (!Number.isSafeInteger(out)) throw new Error("divCeil overflow");
  return out;
}

function divFloor(n: number, d: number): number {
  if (!Number.isSafeInteger(n) || n < 0 || !Number.isSafeInteger(d) || d <= 0) {
    throw new Error("divFloor: arguments must be safe non-negative integers and divisor must be > 0");
  }
  const out = Number(BigInt(n) / BigInt(d));
  if (!Number.isSafeInteger(out)) throw new Error("divFloor overflow");
  return out;
}

function parseManifest(raw: unknown): ParsedChunkedDiskManifest {
  if (!raw || typeof raw !== "object") {
    throw new Error("manifest.json must be a JSON object");
  }
  const obj = raw as Partial<ChunkedDiskManifestV1>;

  if (obj.schema !== "aero.chunked-disk-image.v1") {
    throw new Error(`unsupported manifest schema: ${String(obj.schema)}`);
  }

  if (typeof obj.version !== "string" || !obj.version.trim()) {
    throw new Error("manifest version must be a non-empty string");
  }
  if (typeof obj.mimeType !== "string" || !obj.mimeType.trim()) {
    throw new Error("manifest mimeType must be a non-empty string");
  }

  const totalSize = asSafeInt(obj.totalSize, "totalSize");
  const chunkSize = asSafeInt(obj.chunkSize, "chunkSize");
  const chunkCount = asSafeInt(obj.chunkCount, "chunkCount");
  const chunkIndexWidth = asSafeInt(obj.chunkIndexWidth, "chunkIndexWidth");

  if (totalSize <= 0) throw new Error("totalSize must be > 0");
  if (totalSize % SECTOR_SIZE !== 0) {
    throw new Error(`totalSize must be a multiple of ${SECTOR_SIZE}`);
  }
  if (chunkSize <= 0) throw new Error("chunkSize must be > 0");
  if (chunkSize % SECTOR_SIZE !== 0) {
    throw new Error(`chunkSize must be a multiple of ${SECTOR_SIZE}`);
  }
  if (chunkSize > MAX_REMOTE_CHUNK_SIZE_BYTES) {
    throw new Error(`chunkSize too large: max=${MAX_REMOTE_CHUNK_SIZE_BYTES} got=${chunkSize}`);
  }
  if (chunkCount <= 0) throw new Error("chunkCount must be > 0");
  if (chunkCount > MAX_REMOTE_CHUNK_COUNT) {
    throw new Error(`chunkCount too large: max=${MAX_REMOTE_CHUNK_COUNT} got=${chunkCount}`);
  }
  if (chunkIndexWidth <= 0) throw new Error("chunkIndexWidth must be > 0");

  const expectedChunkCount = divCeil(totalSize, chunkSize);
  if (chunkCount !== expectedChunkCount) {
    throw new Error(`chunkCount mismatch: expected=${expectedChunkCount} manifest=${chunkCount}`);
  }

  const minWidth = String(chunkCount - 1).length;
  if (chunkIndexWidth < minWidth) {
    throw new Error(`chunkIndexWidth too small: need>=${minWidth} got=${chunkIndexWidth}`);
  }

  const lastChunkSize = toSafeNumber(
    BigInt(totalSize) - BigInt(chunkSize) * BigInt(chunkCount - 1),
    "lastChunkSize",
  );
  if (!Number.isSafeInteger(lastChunkSize) || lastChunkSize <= 0 || lastChunkSize > chunkSize) {
    throw new Error("invalid derived final chunk size");
  }

  const chunkSizes: number[] = new Array(chunkCount);
  const chunkSha256: Array<string | null> = new Array(chunkCount).fill(null);

  if (obj.chunks !== undefined) {
    if (!Array.isArray(obj.chunks)) throw new Error("chunks must be an array when present");
    if (obj.chunks.length !== chunkCount) {
      throw new Error(`chunks.length mismatch: expected=${chunkCount} actual=${obj.chunks.length}`);
    }
    for (let i = 0; i < obj.chunks.length; i += 1) {
      const item = obj.chunks[i];
      if (!item || typeof item !== "object") throw new Error(`chunks[${i}] must be an object`);
      const size =
        item.size === undefined ? (i === chunkCount - 1 ? lastChunkSize : chunkSize) : asSafeInt(item.size, `chunks[${i}].size`);
      if (size <= 0) throw new Error(`chunks[${i}].size must be > 0`);
      if (i < chunkCount - 1 && size !== chunkSize) {
        throw new Error(`chunks[${i}].size mismatch: expected=${chunkSize} actual=${size}`);
      }
      if (i === chunkCount - 1 && size !== lastChunkSize) {
        throw new Error(`chunks[${i}].size mismatch: expected=${lastChunkSize} actual=${size}`);
      }
      chunkSizes[i] = size;

      if (item.sha256 !== undefined) {
        if (typeof item.sha256 !== "string") throw new Error(`chunks[${i}].sha256 must be a string`);
        const normalized = item.sha256.trim().toLowerCase();
        if (!/^[0-9a-f]{64}$/.test(normalized)) {
          throw new Error(`chunks[${i}].sha256 must be a 64-char hex string`);
        }
        chunkSha256[i] = normalized;
      }
    }
  } else {
    for (let i = 0; i < chunkCount; i += 1) {
      chunkSizes[i] = i === chunkCount - 1 ? lastChunkSize : chunkSize;
    }
  }

  const sum = chunkSizes.reduce((a, b) => a + b, 0);
  if (sum !== totalSize) {
    throw new Error(`chunk sizes do not sum to totalSize: sum=${sum} totalSize=${totalSize}`);
  }

  return { totalSize, chunkSize, chunkCount, chunkIndexWidth, chunkSizes, chunkSha256 };
}

async function sha256Hex(data: Uint8Array<ArrayBuffer>): Promise<string> {
  const subtle = crypto.subtle;
  const digest = await subtle.digest("SHA-256", toArrayBufferUint8(data));
  const bytes = new Uint8Array(digest);
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

function stableImageIdFromUrl(url: string): string {
  // Use URL parsing when possible so we can drop querystring auth material.
  // Fall back to string splitting for odd / non-standard URLs.
  try {
    const base = (globalThis as typeof globalThis & { location?: { href?: string } }).location?.href;
    const u = base ? new URL(url, base) : new URL(url);
    return `${u.origin}${u.pathname}`;
  } catch {
    const noHash = url.split("#", 1)[0] ?? url;
    return (noHash.split("?", 1)[0] ?? noHash).trim();
  }
}

function parseUrlMaybe(url: string): URL | null {
  try {
    const base = (globalThis as typeof globalThis & { location?: { href?: string } }).location?.href;
    return base ? new URL(url, base) : new URL(url);
  } catch {
    return null;
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => {
    const timer = setTimeout(resolve, ms);
    (timer as unknown as { unref?: () => void }).unref?.();
  });
}

async function retryWithBackoff<T>(
  fn: (attempt: number) => Promise<T>,
  opts: { maxAttempts: number; baseDelayMs: number; shouldRetry: (err: unknown) => boolean },
): Promise<T> {
  for (let attempt = 1; attempt <= opts.maxAttempts; attempt += 1) {
    try {
      return await fn(attempt);
    } catch (err) {
      if (attempt >= opts.maxAttempts || !opts.shouldRetry(err)) throw err;
      const delay = opts.baseDelayMs * 2 ** (attempt - 1);
      await sleep(delay);
    }
  }
  // Unreachable.
  throw new Error("retryWithBackoff exhausted");
}

class RemoteChunkCache implements ChunkCache {
  private meta: RemoteChunkedDiskCacheMeta;
  private rangeSet = new RangeSet();
  private metaWriteChain: Promise<void> = Promise.resolve();
  private metaDirty = false;
  private metaRevision = 0;
  private metaEpoch = 0;
  private metaFlushTimer: ReturnType<typeof setTimeout> | null = null;
  private metaFlushInFlight: Promise<void> | null = null;
  private cachedBytes = 0;
  private initOnce: Promise<void> | null = null;

  constructor(
    private readonly store: BinaryStore,
    private readonly manager: RemoteCacheManager,
    private readonly cacheKey: string,
    private readonly cacheKeyParts: RemoteCacheKeyParts,
    private readonly validators: { sizeBytes: number; etag: string | null; lastModified: string | null },
    private readonly manifest: ParsedChunkedDiskManifest,
    private readonly cacheLimitBytes: number | null,
    meta: RemoteChunkedDiskCacheMeta,
  ) {
    this.meta = meta;
    this.meta.accessCounter ??= 0;
    this.meta.chunkLastAccess ??= {};
    for (const r of meta.cachedRanges) this.rangeSet.insert(r.start, r.end);
    this.cachedBytes = this.rangeSet.totalLen();
  }

  getCachedBytes(): number {
    return this.cachedBytes;
  }

  getCacheLimitBytes(): number | null {
    return this.cacheLimitBytes;
  }

  private chunkPath(chunkIndex: number): string {
    return `${REMOTE_CACHE_ROOT_PATH}/${this.cacheKey}/chunks/${chunkIndex}.bin`;
  }

  private chunkRange(chunkIndex: number): ByteRange {
    const start = chunkIndex * this.manifest.chunkSize;
    const size = this.manifest.chunkSizes[chunkIndex] ?? 0;
    return { start, end: start + size };
  }

  private noteAccess(chunkIndex: number): void {
    this.meta.accessCounter = (this.meta.accessCounter ?? 0) + 1;
    (this.meta.chunkLastAccess ??= {})[String(chunkIndex)] = this.meta.accessCounter;
    this.meta.lastAccessedAtMs = Date.now();
  }

  private markMetaDirty(): void {
    this.metaDirty = true;
    this.metaRevision += 1;
    this.scheduleMetaFlush();
  }

  private scheduleMetaFlush(): void {
    // Debounce meta writes so repeated cache hits don't cause OPFS write amplification.
    const DEBOUNCE_MS = 100;
    if (this.metaFlushTimer !== null) {
      clearTimeout(this.metaFlushTimer);
      this.metaFlushTimer = null;
    }
    const epoch = this.metaEpoch;
    const timer = setTimeout(() => {
      this.metaFlushTimer = null;
      if (this.metaEpoch !== epoch) return;
      void this.flushMeta(false).catch(() => {
        // best-effort
      });
    }, DEBOUNCE_MS);
    (timer as unknown as { unref?: () => void }).unref?.();
    this.metaFlushTimer = timer;
  }

  private cachedChunkIndices(): Set<number> {
    const out = new Set<number>();
    const chunkSize = this.manifest.chunkSize;
    for (const r of this.rangeSet.getRanges()) {
      if (r.start >= r.end) continue;
      const startChunk = divFloor(r.start, chunkSize);
      const endChunk = divFloor(r.end - 1, chunkSize);
      for (let idx = startChunk; idx <= endChunk; idx += 1) {
        if (!Number.isSafeInteger(idx) || idx < 0 || idx >= this.manifest.chunkSizes.length) continue;
        out.add(idx);
      }
    }
    return out;
  }

  private reconcileLruMeta(): boolean {
    let dirty = false;

    this.meta.accessCounter ??= 0;
    this.meta.chunkLastAccess ??= {};

    const cached = this.cachedChunkIndices();
    const lastAccess = this.meta.chunkLastAccess ?? {};

    for (const chunkStr in lastAccess) {
      if (!Object.prototype.hasOwnProperty.call(lastAccess, chunkStr)) continue;
      const counterRaw = lastAccess[chunkStr];
      const idx = Number(chunkStr);
      if (!Number.isSafeInteger(idx) || idx < 0 || !cached.has(idx)) {
        delete lastAccess[chunkStr];
        dirty = true;
        continue;
      }
      if (typeof counterRaw !== "number" || !Number.isFinite(counterRaw) || counterRaw < 0) {
        lastAccess[chunkStr] = 0;
        dirty = true;
      }
    }

    for (const idx of cached) {
      const key = String(idx);
      if (lastAccess[key] === undefined) {
        // Orphan cached ranges without LRU metadata (e.g. legacy meta.json): treat as the oldest.
        lastAccess[key] = 0;
        dirty = true;
      }
    }

    // Ensure `accessCounter` monotonically increases beyond all last-access values.
    let maxCounter = this.meta.accessCounter ?? 0;
    for (const chunkStr in lastAccess) {
      if (!Object.prototype.hasOwnProperty.call(lastAccess, chunkStr)) continue;
      const counter = lastAccess[chunkStr];
      if (typeof counter === "number" && Number.isFinite(counter) && counter > maxCounter) {
        maxCounter = counter;
      }
    }
    if (this.meta.accessCounter !== maxCounter) {
      this.meta.accessCounter = maxCounter;
      dirty = true;
    }

    // Persist the compacted view of ranges (RangeSet merges adjacent ones).
    const compacted = this.rangeSet.getRanges();
    if (!rangesEqual(compacted, this.meta.cachedRanges)) {
      this.meta.cachedRanges = compacted;
      dirty = true;
    }

    return dirty;
  }

  async initialize(): Promise<void> {
    if (!this.initOnce) {
      this.initOnce = (async () => {
        const dirty = this.reconcileLruMeta();
        if (dirty) this.markMetaDirty();
        // Enforce cache size limit on open so we don't keep exceeding quota until the next download.
        await this.enforceCacheLimit(-1);
      })();
    }
    await this.initOnce;
  }

  async getChunk(chunkIndex: number): Promise<Uint8Array<ArrayBuffer> | null> {
    await this.initialize();
    const r = this.chunkRange(chunkIndex);
    if (!this.rangeSet.containsRange(r.start, r.end)) return null;

    const expectedLen = r.end - r.start;
    const bytes = await this.store.read(this.chunkPath(chunkIndex));
    if (!bytes || bytes.length !== expectedLen) {
      // Heal: metadata said cached but file missing/corrupt.
      await this.store.remove(this.chunkPath(chunkIndex)).catch(() => {});
      this.rangeSet.remove(r.start, r.end);
      delete (this.meta.chunkLastAccess ?? {})[String(chunkIndex)];
      this.meta.cachedRanges = this.rangeSet.getRanges();
      this.cachedBytes = this.rangeSet.totalLen();
      this.markMetaDirty();
      return null;
    }

    this.noteAccess(chunkIndex);
    this.markMetaDirty();
    return bytes;
  }

  async putChunk(chunkIndex: number, bytes: Uint8Array<ArrayBuffer>): Promise<void> {
    await this.initialize();
    if (this.cacheLimitBytes !== null) {
      if (this.cacheLimitBytes === 0) return;
      if (bytes.length > this.cacheLimitBytes) {
        // Chunk can never fit; skip caching entirely.
        return;
      }
    }

    const r = this.chunkRange(chunkIndex);
    const expectedLen = r.end - r.start;
    if (bytes.length !== expectedLen) {
      throw new Error(`chunk ${chunkIndex} length mismatch: expected=${expectedLen} actual=${bytes.length}`);
    }

    await this.store.write(this.chunkPath(chunkIndex), bytes);
    this.rangeSet.insert(r.start, r.end);
    this.cachedBytes = this.rangeSet.totalLen();
    this.noteAccess(chunkIndex);
    this.meta.cachedRanges = this.rangeSet.getRanges();
    this.markMetaDirty();
    await this.enforceCacheLimit(chunkIndex);
  }

  async flush(): Promise<void> {
    await this.flushMeta(true);
  }

  async clear(): Promise<void> {
    if (this.metaFlushTimer !== null) {
      clearTimeout(this.metaFlushTimer);
      this.metaFlushTimer = null;
    }
    this.metaDirty = false;
    this.metaRevision = 0;
    this.metaEpoch += 1;
    await this.manager.clearCache(this.cacheKey);
    const reopened = await this.manager.openCache(this.cacheKeyParts, {
      chunkSizeBytes: this.manifest.chunkSize,
      validators: this.validators,
    });
    this.meta = reopened.meta;
    this.meta.accessCounter ??= 0;
    this.meta.chunkLastAccess ??= {};

    this.rangeSet = new RangeSet();
    this.cachedBytes = 0;
    this.metaWriteChain = Promise.resolve();
  }

  private async flushMeta(force: boolean): Promise<void> {
    if (force && this.metaFlushTimer !== null) {
      clearTimeout(this.metaFlushTimer);
      this.metaFlushTimer = null;
    }

    // If there's already a flush running, wait for it (and re-check `metaDirty`
    // if we're doing a forced flush).
    if (this.metaFlushInFlight) {
      await this.metaFlushInFlight;
      if (!force || !this.metaDirty) {
        await this.metaWriteChain;
        return;
      }
    }

    if (!this.metaDirty) {
      await this.metaWriteChain;
      return;
    }

    const epoch = this.metaEpoch;
    const run = (async () => {
      // Multiple chunk fetches can complete concurrently; serialize meta writes so that
      // older snapshots don't race and overwrite newer metadata.
      while (this.metaDirty && this.metaEpoch === epoch) {
        let writtenRevision = -1;
        this.metaWriteChain = this.metaWriteChain
          .catch(() => {
            // Keep the chain alive even if a previous write failed.
          })
          .then(async () => {
            if (this.metaEpoch !== epoch) return;
            writtenRevision = this.metaRevision;
            await this.manager.writeMeta(this.cacheKey, this.meta);
          });
        await this.metaWriteChain;
        if (this.metaEpoch !== epoch) return;
        if (this.metaRevision === writtenRevision) {
          this.metaDirty = false;
          return;
        }
        if (!force) {
          // More meta changes arrived while writing; debounce another flush.
          this.scheduleMetaFlush();
          return;
        }
        // Forced flush: keep going until the metadata is stable.
      }
    })();

    this.metaFlushInFlight = run;
    try {
      await run;
    } finally {
      if (this.metaFlushInFlight === run) {
        this.metaFlushInFlight = null;
      }
    }
  }

  private async enforceCacheLimit(protectedChunk: number): Promise<void> {
    if (this.cacheLimitBytes === null) return;
    while (this.cachedBytes > this.cacheLimitBytes) {
      let lruChunk: number | null = null;
      let lruCounter = Number.POSITIVE_INFINITY;
      const lastAccess = this.meta.chunkLastAccess ?? {};
      for (const chunkStr in lastAccess) {
        if (!Object.prototype.hasOwnProperty.call(lastAccess, chunkStr)) continue;
        const counterRaw = lastAccess[chunkStr];
        const idx = Number(chunkStr);
        if (!Number.isSafeInteger(idx) || idx < 0 || idx === protectedChunk) continue;
        const counter = typeof counterRaw === "number" && Number.isFinite(counterRaw) ? counterRaw : 0;
        if (counter < lruCounter) {
          lruCounter = counter;
          lruChunk = idx;
        }
      }
      if (lruChunk === null) break;

      const r = this.chunkRange(lruChunk);
      await this.store.remove(this.chunkPath(lruChunk)).catch(() => {});
      this.rangeSet.remove(r.start, r.end);
      delete (this.meta.chunkLastAccess ?? {})[String(lruChunk)];
      this.meta.cachedRanges = this.rangeSet.getRanges();
      this.cachedBytes = this.rangeSet.totalLen();
      this.markMetaDirty();
    }
  }
}

export class RemoteChunkedDisk implements AsyncSectorDisk {
  readonly sectorSize = SECTOR_SIZE;
  readonly capacityBytes: number;

  private readonly sourceId: string;
  private readonly lease: DiskAccessLease;
  private readonly manifest: ParsedChunkedDiskManifest;
  private readonly chunkCache: ChunkCache;
  private readonly prefetchSequentialChunks: number;
  private readonly maxConcurrentFetches: number;
  private readonly semaphore: Semaphore;
  private readonly maxAttempts: number;
  private readonly retryBaseDelayMs: number;
  private readonly leaseRefreshMarginMs: number;
  private readonly leaseRefresher: DiskAccessLeaseRefresher;
  private readonly abort = new AbortController();

  private readonly inflight = new Map<number, Promise<Uint8Array<ArrayBuffer>>>();
  private lastReadEnd: number | null = null;
  private closed = false;

  private cacheGeneration = 0;

  private telemetry: Omit<RemoteDiskTelemetrySnapshot, "url" | "totalSize" | "blockSize" | "cacheLimitBytes" | "cachedBytes" | "inflightFetches"> & {
    lastFetchRange: ByteRange | null;
  } = {
    blockRequests: 0,
    cacheHits: 0,
    cacheMisses: 0,
    inflightJoins: 0,
    requests: 0,
    bytesDownloaded: 0,
    lastFetchMs: null,
    lastFetchAtMs: null,
    lastFetchRange: null,
  };

  private constructor(
    sourceId: string,
    lease: DiskAccessLease,
    manifest: ParsedChunkedDiskManifest,
    chunkCache: ChunkCache,
    options: Required<Pick<RemoteChunkedDiskOptions, "prefetchSequentialChunks" | "maxAttempts" | "retryBaseDelayMs" | "leaseRefreshMarginMs">> & {
      maxConcurrentFetches: number;
    },
  ) {
    this.sourceId = sourceId;
    this.lease = lease;
    this.manifest = manifest;
    this.capacityBytes = manifest.totalSize;
    this.chunkCache = chunkCache;
    this.prefetchSequentialChunks = options.prefetchSequentialChunks;
    this.maxConcurrentFetches = options.maxConcurrentFetches;
    this.semaphore = new Semaphore(options.maxConcurrentFetches);
    this.maxAttempts = options.maxAttempts;
    this.retryBaseDelayMs = options.retryBaseDelayMs;
    this.leaseRefreshMarginMs = options.leaseRefreshMarginMs;
    this.leaseRefresher = new DiskAccessLeaseRefresher(this.lease, { refreshMarginMs: this.leaseRefreshMarginMs });
  }

  static async open(manifestUrl: string, options: RemoteChunkedDiskOptions = {}): Promise<RemoteChunkedDisk> {
    if (!manifestUrl) throw new Error("manifestUrl must not be empty");
    const lease = staticDiskLease(manifestUrl, options.credentials ?? "same-origin");
    return await RemoteChunkedDisk.openWithLease({ sourceId: manifestUrl, lease }, options);
  }
  static async openWithLease(
    params: { sourceId: string; lease: DiskAccessLease },
    options: RemoteChunkedDiskOptions = {},
  ): Promise<RemoteChunkedDisk> {
    if (!params.sourceId) throw new Error("sourceId must not be empty");

    type ResolvedRemoteChunkedDiskOptions =
      Required<Omit<RemoteChunkedDiskOptions, "credentials" | "cacheImageId" | "cacheVersion">> &
        Pick<RemoteChunkedDiskOptions, "cacheImageId" | "cacheVersion">;

    // Preserve `null` to mean "no eviction" (unbounded cache), while `undefined`
    // selects the default bounded cache size.
    const resolvedCacheLimitBytes =
      options.cacheLimitBytes === undefined ? 512 * 1024 * 1024 : options.cacheLimitBytes;

    const resolved: ResolvedRemoteChunkedDiskOptions = {
      cacheLimitBytes: resolvedCacheLimitBytes,
      maxConcurrentFetches: options.maxConcurrentFetches ?? 4,
      prefetchSequentialChunks: options.prefetchSequentialChunks ?? 2,
      maxAttempts: options.maxAttempts ?? 3,
      retryBaseDelayMs: options.retryBaseDelayMs ?? 200,
      store: options.store ?? (hasOpfsRoot() ? new OpfsStore() : new MemoryStore()),
      cacheBackend: options.cacheBackend ?? pickDefaultBackend(),
      cacheImageId: options.cacheImageId,
      cacheVersion: options.cacheVersion,
      leaseRefreshMarginMs: options.leaseRefreshMarginMs ?? DEFAULT_LEASE_REFRESH_MARGIN_MS,
    };

    if (resolved.cacheLimitBytes !== null) {
      if (!Number.isSafeInteger(resolved.cacheLimitBytes) || resolved.cacheLimitBytes < 0) {
        throw new Error(`invalid cacheLimitBytes=${resolved.cacheLimitBytes}`);
      }
    }
    if (!Number.isSafeInteger(resolved.maxConcurrentFetches) || resolved.maxConcurrentFetches <= 0) {
      throw new Error(`invalid maxConcurrentFetches=${resolved.maxConcurrentFetches}`);
    }
    if (resolved.maxConcurrentFetches > MAX_REMOTE_MAX_CONCURRENT_FETCHES) {
      throw new Error(
        `maxConcurrentFetches too large: max=${MAX_REMOTE_MAX_CONCURRENT_FETCHES} got=${resolved.maxConcurrentFetches}`,
      );
    }
    if (!Number.isSafeInteger(resolved.prefetchSequentialChunks) || resolved.prefetchSequentialChunks < 0) {
      throw new Error(`invalid prefetchSequentialChunks=${resolved.prefetchSequentialChunks}`);
    }
    if (resolved.prefetchSequentialChunks > MAX_REMOTE_PREFETCH_SEQUENTIAL_CHUNKS) {
      throw new Error(
        `prefetchSequentialChunks too large: max=${MAX_REMOTE_PREFETCH_SEQUENTIAL_CHUNKS} got=${resolved.prefetchSequentialChunks}`,
      );
    }
    if (!Number.isSafeInteger(resolved.maxAttempts) || resolved.maxAttempts <= 0) {
      throw new Error(`invalid maxAttempts=${resolved.maxAttempts}`);
    }
    if (resolved.maxAttempts > MAX_REMOTE_MAX_ATTEMPTS) {
      throw new Error(`maxAttempts too large: max=${MAX_REMOTE_MAX_ATTEMPTS} got=${resolved.maxAttempts}`);
    }
    if (!Number.isSafeInteger(resolved.retryBaseDelayMs) || resolved.retryBaseDelayMs < 0) {
      throw new Error(`invalid retryBaseDelayMs=${resolved.retryBaseDelayMs}`);
    }
    if (!Number.isSafeInteger(resolved.leaseRefreshMarginMs) || resolved.leaseRefreshMarginMs < 0) {
      throw new Error(`invalid leaseRefreshMarginMs=${resolved.leaseRefreshMarginMs}`);
    }

    const resp = await fetchWithDiskAccessLease(params.lease, { method: "GET" }, { retryAuthOnce: true });
    if (!resp.ok) throw new Error(`failed to fetch manifest: ${resp.status}`);
    const json = await readJsonResponseWithLimit(resp, { maxBytes: MAX_REMOTE_MANIFEST_JSON_BYTES, label: "manifest.json" });
    const manifest = parseManifest(json);

    // Keep sequential prefetch and in-flight concurrency bounded. Compute using BigInt to avoid
    // overflow/precision loss for extreme inputs.
    const chunkSizeBytes = BigInt(manifest.chunkSize);
    const totalSizeBytes = BigInt(manifest.totalSize);
    const perFetchBytes = chunkSizeBytes < totalSizeBytes ? chunkSizeBytes : totalSizeBytes;
    const inflightBytes = BigInt(resolved.maxConcurrentFetches) * perFetchBytes;
    if (inflightBytes > BigInt(MAX_REMOTE_INFLIGHT_BYTES)) {
      throw new Error(
        `inflight bytes too large: max=${MAX_REMOTE_INFLIGHT_BYTES} got=${inflightBytes.toString()}`,
      );
    }
    const prefetchBytes = BigInt(resolved.prefetchSequentialChunks) * chunkSizeBytes;
    if (prefetchBytes > BigInt(MAX_REMOTE_PREFETCH_SEQUENTIAL_BYTES)) {
      throw new Error(
        `prefetch bytes too large: max=${MAX_REMOTE_PREFETCH_SEQUENTIAL_BYTES} got=${prefetchBytes.toString()}`,
      );
    }

    const manifestV1 = json as ChunkedDiskManifestV1;
    const derivedImageId =
      typeof manifestV1.imageId === "string" && manifestV1.imageId.trim().length > 0
        ? manifestV1.imageId.trim()
        : stableImageIdFromUrl(params.sourceId);

    const cacheImageId = (resolved.cacheImageId ?? derivedImageId).trim();
    if (!cacheImageId) {
      throw new Error("cacheImageId must not be empty");
    }
    const cacheVersion = (resolved.cacheVersion ?? manifestV1.version).trim();
    if (!cacheVersion) {
      throw new Error("cacheVersion must not be empty");
    }

    const cacheKeyParts: RemoteCacheKeyParts = {
      imageId: cacheImageId,
      version: cacheVersion,
      deliveryType: remoteChunkedDeliveryType(manifest.chunkSize),
    };
    const validators = {
      sizeBytes: manifest.totalSize,
      etag: resp.headers.get("etag"),
      lastModified: resp.headers.get("last-modified"),
    };

    let cache: ChunkCache;
    if (resolved.cacheLimitBytes === 0) {
      // cacheLimitBytes=0 is defined as "disable caching entirely". Ensure we do not open or
      // read/write any persistent cache backend (OPFS or IndexedDB).
      cache = new NoopChunkCache();
    } else if (options.store) {
      // Tests can inject an in-memory store to avoid depending on OPFS/IDB.
      const manager = new RemoteCacheManager(new StoreDirHandle(resolved.store, REMOTE_CACHE_ROOT_PATH));
      const opened = await manager.openCache(cacheKeyParts, { chunkSizeBytes: manifest.chunkSize, validators });
      cache = new RemoteChunkCache(
        resolved.store,
        manager,
        opened.cacheKey,
        cacheKeyParts,
        validators,
        manifest,
        resolved.cacheLimitBytes,
        opened.meta,
      );
    } else if (resolved.cacheBackend === "idb" && typeof indexedDB !== "undefined") {
      const cacheKey = await RemoteCacheManager.deriveCacheKey(cacheKeyParts);
      const idbCache = await IdbRemoteChunkCache.open({
        cacheKey,
        signature: {
          imageId: cacheKeyParts.imageId,
          version: cacheKeyParts.version,
          etag: resp.headers.get("etag"),
          lastModified: resp.headers.get("last-modified"),
          sizeBytes: manifest.totalSize,
          chunkSize: manifest.chunkSize,
        },
        cacheLimitBytes: resolved.cacheLimitBytes,
      });
      const status = await idbCache.getStatus();
      cache = new IdbChunkCache(idbCache, manifest, status);
    } else {
      const manager = new RemoteCacheManager(new StoreDirHandle(resolved.store, REMOTE_CACHE_ROOT_PATH));
      const opened = await manager.openCache(cacheKeyParts, { chunkSizeBytes: manifest.chunkSize, validators });
      cache = new RemoteChunkCache(
        resolved.store,
        manager,
        opened.cacheKey,
        cacheKeyParts,
        validators,
        manifest,
        resolved.cacheLimitBytes,
        opened.meta,
      );
    }

    if (cache instanceof RemoteChunkCache) {
      await cache.initialize();
    }

    const disk = new RemoteChunkedDisk(cacheImageId, params.lease, manifest, cache, {
      maxConcurrentFetches: resolved.maxConcurrentFetches,
      prefetchSequentialChunks: resolved.prefetchSequentialChunks,
      maxAttempts: resolved.maxAttempts,
      retryBaseDelayMs: resolved.retryBaseDelayMs,
      leaseRefreshMarginMs: resolved.leaseRefreshMarginMs,
    });
    disk.leaseRefresher.start();
    return disk;
  }

  getTelemetrySnapshot(): RemoteDiskTelemetrySnapshot {
    return {
      url: this.sourceId,
      totalSize: this.capacityBytes,
      blockSize: this.manifest.chunkSize,
      cacheLimitBytes: this.chunkCache.getCacheLimitBytes(),
      cachedBytes: this.chunkCache.getCachedBytes(),

      blockRequests: this.telemetry.blockRequests,
      cacheHits: this.telemetry.cacheHits,
      cacheMisses: this.telemetry.cacheMisses,
      inflightJoins: this.telemetry.inflightJoins,

      requests: this.telemetry.requests,
      bytesDownloaded: this.telemetry.bytesDownloaded,

      inflightFetches: this.inflight.size,

      lastFetchMs: this.telemetry.lastFetchMs,
      lastFetchAtMs: this.telemetry.lastFetchAtMs,
      lastFetchRange: this.telemetry.lastFetchRange ? { ...this.telemetry.lastFetchRange } : null,
    };
  }

  async readSectors(lba: number, buffer: Uint8Array): Promise<void> {
    if (this.closed) throw new Error("disk is closed");
    assertSectorAligned(buffer.byteLength, this.sectorSize);
    const offset = checkedOffset(lba, buffer.byteLength, this.sectorSize);
    if (offset + buffer.byteLength > this.capacityBytes) {
      throw new Error("read past end of disk");
    }
    if (buffer.byteLength === 0) {
      this.lastReadEnd = offset;
      return;
    }

    const startChunk = divFloor(offset, this.manifest.chunkSize);
    const endChunk = divFloor(offset + buffer.byteLength - 1, this.manifest.chunkSize);

    // Batch-load cached chunks when using IndexedDB. This reduces IDB roundtrips when a read spans
    // multiple chunks (e.g. large sequential reads).
    if (this.chunkCache.prefetchChunks && endChunk > startChunk) {
      const indices: number[] = [];
      for (let chunk = startChunk; chunk <= endChunk; chunk += 1) indices.push(chunk);
      await this.chunkCache.prefetchChunks(indices);
    }

    const readStart = offset;
    const readEnd = offset + buffer.byteLength;
    const chunkSize = this.manifest.chunkSize;

    const copyFromChunk = (chunkIndex: number, bytes: Uint8Array<ArrayBuffer>): void => {
      const chunkStart = chunkIndex * chunkSize;
      const chunkEnd = chunkStart + bytes.length;
      const copyStart = Math.max(readStart, chunkStart);
      const copyEnd = Math.min(readEnd, chunkEnd);
      if (copyEnd <= copyStart) return;

      const srcStart = copyStart - chunkStart;
      const dstStart = copyStart - readStart;
      const len = copyEnd - copyStart;
      buffer.set(bytes.subarray(srcStart, srcStart + len), dstStart);
    };

    // Avoid allocating/promising all spanned chunks at once: keeping an array of
    // promises can retain many resolved multi-megabyte ArrayBuffers until the
    // whole read completes. Instead, process a bounded window of chunks and
    // copy them into the caller's buffer as they arrive.
    const window = new Map<number, Promise<void>>();
    let nextChunk = startChunk;
    const maxInflight = this.maxConcurrentFetches;

    const launch = (chunkIndex: number): void => {
      const task = this.getChunk(chunkIndex)
        .then((bytes) => {
          copyFromChunk(chunkIndex, bytes);
        })
        .finally(() => {
          window.delete(chunkIndex);
        });
      window.set(chunkIndex, task);
    };

    while (nextChunk <= endChunk && window.size < maxInflight) {
      launch(nextChunk);
      nextChunk += 1;
    }

    while (window.size > 0) {
      await Promise.race(window.values());
      while (nextChunk <= endChunk && window.size < maxInflight) {
        launch(nextChunk);
        nextChunk += 1;
      }
    }

    this.maybePrefetch(offset, buffer.byteLength, endChunk);
  }

  async writeSectors(_lba: number, _data: Uint8Array): Promise<void> {
    throw new Error("RemoteChunkedDisk is read-only");
  }

  async flush(): Promise<void> {
    await this.chunkCache.flush();
  }

  async clearCache(): Promise<void> {
    this.cacheGeneration += 1;
    this.inflight.clear();
    this.lastReadEnd = null;
    this.telemetry = {
      blockRequests: 0,
      cacheHits: 0,
      cacheMisses: 0,
      inflightJoins: 0,
      requests: 0,
      bytesDownloaded: 0,
      lastFetchMs: null,
      lastFetchAtMs: null,
      lastFetchRange: null,
    };
    await this.chunkCache.clear();
  }

  async close(): Promise<void> {
    if (this.closed) return;
    this.closed = true;
    this.leaseRefresher.stop();
    this.abort.abort();
    await Promise.allSettled(Array.from(this.inflight.values()));
    this.inflight.clear();
    await this.flush().catch(() => {});
    this.chunkCache.close?.();
  }

  private chunkUrl(chunkIndex: number): string {
    const name = String(chunkIndex).padStart(this.manifest.chunkIndexWidth, "0");
    const manifestUrl = parseUrlMaybe(this.lease.url);
    if (manifestUrl) {
      const url = new URL(`chunks/${name}.bin`, manifestUrl);
      // Preserve querystring auth material (e.g. signed URLs) when deriving chunk URLs.
      // This intentionally does not affect cache key derivation, which uses stable identifiers.
      url.search = manifestUrl.search;
      return url.toString();
    }

    // Non-standard URLs or environments without a base URL (e.g. tests) can leave us without a parsed URL.
    // Fall back to string manipulation so relative paths still work.
    const base = this.lease.url;
    const noHash = base.split("#", 1)[0] ?? base;
    const [pathPart, queryPart] = noHash.split("?", 2) as [string, string?];
    const slash = pathPart.lastIndexOf("/");
    const prefix = slash >= 0 ? pathPart.slice(0, slash + 1) : "";
    const chunkPath = `${prefix}chunks/${name}.bin`;
    return queryPart ? `${chunkPath}?${queryPart}` : chunkPath;
  }

  private shouldRetry(err: unknown): boolean {
    if (err instanceof ResponseTooLargeError) return false;
    if (err instanceof IntegrityError) return true;
    if (err instanceof ChunkFetchError) {
      if (err.status === 429) return true;
      if (err.status >= 500) return true;
      return false;
    }
    // Network errors, timeouts, etc.
    if (err instanceof Error && err.name === "AbortError") return false;
    return true;
  }

  private async fetchChunkOnce(chunkIndex: number, generation: number): Promise<Uint8Array<ArrayBuffer>> {
    const expectedLen = this.manifest.chunkSizes[chunkIndex]!;
    const expectedSha = this.manifest.chunkSha256[chunkIndex];

    if (generation === this.cacheGeneration) {
      this.telemetry.requests += 1;
    }

    await this.maybeRefreshLease();
    const resp = await fetchWithDiskAccessLeaseForUrl(
      this.lease,
      () => this.chunkUrl(chunkIndex),
      { method: "GET", signal: this.abort.signal },
      { retryAuthOnce: true },
    );
    if (!resp.ok) {
      throw new ChunkFetchError(`chunk fetch failed: ${resp.status}`, resp.status);
    }
    const bytes = await readResponseBytesWithLimit(resp, { maxBytes: expectedLen, label: `chunk ${chunkIndex}` });
    if (bytes.length !== expectedLen) {
      throw new Error(`chunk ${chunkIndex} length mismatch: expected=${expectedLen} actual=${bytes.length}`);
    }
    if (generation === this.cacheGeneration) {
      this.telemetry.bytesDownloaded += bytes.byteLength;
    }

    if (expectedSha) {
      const actual = await sha256Hex(bytes);
      if (actual !== expectedSha) {
        throw new IntegrityError(`chunk ${chunkIndex} sha256 mismatch: expected=${expectedSha} actual=${actual}`);
      }
    }

    return bytes;
  }

  private async fetchChunkWithRetries(chunkIndex: number, generation: number): Promise<Uint8Array<ArrayBuffer>> {
    return await retryWithBackoff(
      async (_attempt) => {
        const release = await this.semaphore.acquire();
        try {
          return await this.fetchChunkOnce(chunkIndex, generation);
        } finally {
          release();
        }
      },
      {
        maxAttempts: this.maxAttempts,
        baseDelayMs: this.retryBaseDelayMs,
        shouldRetry: (err) => this.shouldRetry(err),
      },
    );
  }

  private async getChunk(chunkIndex: number): Promise<Uint8Array<ArrayBuffer>> {
    if (chunkIndex < 0 || chunkIndex >= this.manifest.chunkCount) {
      throw new Error(`chunkIndex out of range: ${chunkIndex}`);
    }

    const generation = this.cacheGeneration;
    this.telemetry.blockRequests += 1;

    const cached = await this.chunkCache.getChunk(chunkIndex);
    if (cached) {
      if (generation === this.cacheGeneration) {
        this.telemetry.cacheHits += 1;
      }
      return cached;
    }

    const existing = this.inflight.get(chunkIndex);
    if (existing) {
      if (generation === this.cacheGeneration) {
        this.telemetry.inflightJoins += 1;
      }
      return await existing;
    }

    if (generation === this.cacheGeneration) {
      this.telemetry.cacheMisses += 1;
      const start = chunkIndex * this.manifest.chunkSize;
      const end = start + this.manifest.chunkSizes[chunkIndex]!;
      this.telemetry.lastFetchRange = { start, end };
    }
    const startTime = performance.now();

    const task = (async () => {
      const bytes = await this.fetchChunkWithRetries(chunkIndex, generation);
      // If the cache was cleared (or the disk closed), allow the read to succeed
      // but avoid writing into a cache that the caller explicitly cleared.
      if (generation === this.cacheGeneration && !this.closed) {
        await this.chunkCache.putChunk(chunkIndex, bytes);
        this.telemetry.lastFetchMs = performance.now() - startTime;
        this.telemetry.lastFetchAtMs = Date.now();
      }
      return bytes;
    })();

    this.inflight.set(chunkIndex, task);
    try {
      return await task;
    } finally {
      // Only remove if this task is still the active inflight entry for the chunk.
      if (this.inflight.get(chunkIndex) === task) {
        this.inflight.delete(chunkIndex);
      }
    }
  }

  private maybePrefetch(offset: number, length: number, lastChunk: number): void {
    const sequential = this.lastReadEnd !== null && this.lastReadEnd === offset;
    this.lastReadEnd = offset + length;
    if (!sequential) return;

    const nextChunk = divFloor(offset + length, this.manifest.chunkSize);
    for (let i = 0; i < this.prefetchSequentialChunks; i += 1) {
      const chunk = nextChunk + i;
      if (chunk >= this.manifest.chunkCount) break;
      void this.getChunk(chunk).catch(() => {
        // best-effort
      });
    }
  }

  private async maybeRefreshLease(): Promise<void> {
    const expiresAt = this.lease.expiresAt;
    if (!expiresAt) return;
    const refreshAtMs = expiresAt.getTime() - this.leaseRefreshMarginMs;
    if (!Number.isFinite(refreshAtMs) || Date.now() < refreshAtMs) return;
    await this.lease.refresh();
  }
}

function staticDiskLease(url: string, credentialsMode: RequestCredentials): DiskAccessLease {
  const lease: DiskAccessLease = {
    url,
    expiresAt: undefined,
    credentialsMode,
    async refresh() {
      return lease;
    },
  };
  return lease;
}
