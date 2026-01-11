import { openFileHandle, removeOpfsEntry } from "../platform/opfs";
import { RangeSet, type ByteRange, type RemoteDiskTelemetrySnapshot } from "../platform/remote_disk";
import { assertSectorAligned, checkedOffset, SECTOR_SIZE, type AsyncSectorDisk } from "./disk";
import { IdbRemoteChunkCache } from "./idb_remote_chunk_cache";
import { pickDefaultBackend, type DiskBackend } from "./metadata";

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

type RemoteChunkedDiskCacheMeta = {
  version: 1;
  manifestUrl: string;
  totalSize: number;
  chunkSize: number;
  chunkCount: number;
  chunkIndexWidth: number;
  downloaded: ByteRange[];
  accessCounter: number;
  chunkLastAccess: Record<string, number>;
};

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
   * Maximum bytes to keep in the persistent cache (LRU-evicted).
   * `null` disables eviction.
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
};

/**
 * `RemoteChunkedDiskOptions` safe to send across `postMessage` boundaries.
 * (The `store` option allows injecting a test store instance and is not transferable.)
 */
export type RemoteChunkedDiskOpenOptions = Omit<RemoteChunkedDiskOptions, "store">;

export interface BinaryStore {
  read(path: string): Promise<Uint8Array | null>;
  write(path: string, data: Uint8Array): Promise<void>;
  remove(path: string, options?: { recursive?: boolean }): Promise<void>;
}

class MemoryStore implements BinaryStore {
  private readonly files = new Map<string, Uint8Array>();

  async read(path: string): Promise<Uint8Array | null> {
    const data = this.files.get(path);
    return data ? data.slice() : null;
  }

  async write(path: string, data: Uint8Array): Promise<void> {
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
  async read(path: string): Promise<Uint8Array | null> {
    try {
      const handle = await openFileHandle(path, { create: false });
      const file = await handle.getFile();
      return new Uint8Array(await file.arrayBuffer());
    } catch {
      return null;
    }
  }

  async write(path: string, data: Uint8Array): Promise<void> {
    const handle = await openFileHandle(path, { create: true });
    const writable = await handle.createWritable({ keepExistingData: false });
    await writable.write(data);
    await writable.close();
  }

  async remove(path: string, options: { recursive?: boolean } = {}): Promise<void> {
    await removeOpfsEntry(path, options);
  }
}

type ChunkCache = {
  getChunk(chunkIndex: number): Promise<Uint8Array | null>;
  putChunk(chunkIndex: number, bytes: Uint8Array): Promise<void>;
  flush(): Promise<void>;
  clear(): Promise<void>;
  close?: () => void;
};

class IdbChunkCache implements ChunkCache {
  constructor(
    private readonly cache: IdbRemoteChunkCache,
    private readonly manifest: ParsedChunkedDiskManifest,
  ) {}

  private expectedLen(chunkIndex: number): number {
    return this.manifest.chunkSizes[chunkIndex] ?? 0;
  }

  async getChunk(chunkIndex: number): Promise<Uint8Array | null> {
    const expectedLen = this.expectedLen(chunkIndex);
    const bytes = await this.cache.get(chunkIndex);
    if (!bytes) return null;
    if (bytes.byteLength !== expectedLen) {
      // Heal: cached but mismatched size (stale/corrupt record).
      await this.cache.delete(chunkIndex);
      return null;
    }
    return bytes;
  }

  async putChunk(chunkIndex: number, bytes: Uint8Array): Promise<void> {
    const expectedLen = this.expectedLen(chunkIndex);
    if (bytes.byteLength !== expectedLen) {
      throw new Error(`chunk ${chunkIndex} length mismatch: expected=${expectedLen} actual=${bytes.byteLength}`);
    }
    await this.cache.put(chunkIndex, bytes);
  }

  async flush(): Promise<void> {
    // All writes are durable per-transaction.
  }

  async clear(): Promise<void> {
    await this.cache.clear();
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
  if (chunkCount <= 0) throw new Error("chunkCount must be > 0");
  if (chunkIndexWidth <= 0) throw new Error("chunkIndexWidth must be > 0");

  const expectedChunkCount = Math.ceil(totalSize / chunkSize);
  if (chunkCount !== expectedChunkCount) {
    throw new Error(`chunkCount mismatch: expected=${expectedChunkCount} manifest=${chunkCount}`);
  }

  const minWidth = String(chunkCount - 1).length;
  if (chunkIndexWidth < minWidth) {
    throw new Error(`chunkIndexWidth too small: need>=${minWidth} got=${chunkIndexWidth}`);
  }

  const lastChunkSize = totalSize - chunkSize * (chunkCount - 1);
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

async function sha256Hex(data: Uint8Array): Promise<string> {
  const subtle = crypto.subtle;
  const digest = await subtle.digest("SHA-256", data);
  const bytes = new Uint8Array(digest);
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

async function stableCacheKey(url: string): Promise<string> {
  // Use SHA-256 when available, fall back to a filesystem-safe encoding.
  try {
    const data = new TextEncoder().encode(url);
    const digest = await crypto.subtle.digest("SHA-256", data);
    const bytes = new Uint8Array(digest);
    return Array.from(bytes)
      .map((b) => b.toString(16).padStart(2, "0"))
      .join("");
  } catch {
    return encodeURIComponent(url).replaceAll("%", "_").slice(0, 128);
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
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
  private metaLoaded = false;
  private meta: RemoteChunkedDiskCacheMeta;
  private rangeSet = new RangeSet();
  private metaWriteChain: Promise<void> = Promise.resolve();
  private cachedBytes = 0;

  constructor(
    private readonly store: BinaryStore,
    private readonly cacheKey: string,
    private readonly manifestUrl: string,
    private readonly manifest: ParsedChunkedDiskManifest,
    private readonly cacheLimitBytes: number | null,
  ) {
    this.meta = {
      version: 1,
      manifestUrl,
      totalSize: manifest.totalSize,
      chunkSize: manifest.chunkSize,
      chunkCount: manifest.chunkCount,
      chunkIndexWidth: manifest.chunkIndexWidth,
      downloaded: [],
      accessCounter: 0,
      chunkLastAccess: {},
    };
  }

  getCachedBytes(): number {
    return this.cachedBytes;
  }

  getCacheLimitBytes(): number | null {
    return this.cacheLimitBytes;
  }

  private chunkPath(chunkIndex: number): string {
    return `state/remote-cache/${this.cacheKey}/chunks/${chunkIndex}.bin`;
  }

  private metaPath(): string {
    return `state/remote-cache/${this.cacheKey}/meta.json`;
  }

  private chunkRange(chunkIndex: number): ByteRange {
    const start = chunkIndex * this.manifest.chunkSize;
    const size = this.manifest.chunkSizes[chunkIndex] ?? 0;
    return { start, end: start + size };
  }

  private noteAccess(chunkIndex: number): void {
    this.meta.accessCounter += 1;
    this.meta.chunkLastAccess[String(chunkIndex)] = this.meta.accessCounter;
  }

  async loadMeta(): Promise<void> {
    if (this.metaLoaded) return;
    this.metaLoaded = true;

    const raw = await this.store.read(this.metaPath());
    if (!raw) return;

    try {
      const parsed = JSON.parse(new TextDecoder().decode(raw)) as RemoteChunkedDiskCacheMeta;
      const compatible =
        parsed &&
        parsed.version === 1 &&
        parsed.manifestUrl === this.manifestUrl &&
        parsed.totalSize === this.manifest.totalSize &&
        parsed.chunkSize === this.manifest.chunkSize &&
        parsed.chunkCount === this.manifest.chunkCount &&
        parsed.chunkIndexWidth === this.manifest.chunkIndexWidth;
      if (!compatible) return;
      this.meta = parsed;
       for (const r of parsed.downloaded) {
         this.rangeSet.insert(r.start, r.end);
       }
       this.cachedBytes = this.rangeSet.totalLen();
     } catch {
       // ignore corrupt meta
     }
   }

  async getChunk(chunkIndex: number): Promise<Uint8Array | null> {
    await this.loadMeta();
    const r = this.chunkRange(chunkIndex);
    if (!this.rangeSet.containsRange(r.start, r.end)) return null;

    const expectedLen = r.end - r.start;
    const bytes = await this.store.read(this.chunkPath(chunkIndex));
    if (!bytes || bytes.length !== expectedLen) {
      // Heal: metadata said cached but file missing/corrupt.
      await this.store.remove(this.chunkPath(chunkIndex)).catch(() => {});
      this.rangeSet.remove(r.start, r.end);
      delete this.meta.chunkLastAccess[String(chunkIndex)];
      this.meta.downloaded = this.rangeSet.getRanges();
      this.cachedBytes = this.rangeSet.totalLen();
      await this.persistMeta();
      return null;
    }

    this.noteAccess(chunkIndex);
    await this.persistMeta();
    return bytes;
  }

  async putChunk(chunkIndex: number, bytes: Uint8Array): Promise<void> {
    await this.loadMeta();
    const r = this.chunkRange(chunkIndex);
    const expectedLen = r.end - r.start;
    if (bytes.length !== expectedLen) {
      throw new Error(`chunk ${chunkIndex} length mismatch: expected=${expectedLen} actual=${bytes.length}`);
    }

    await this.store.write(this.chunkPath(chunkIndex), bytes);
    this.rangeSet.insert(r.start, r.end);
    this.cachedBytes = this.rangeSet.totalLen();
    this.noteAccess(chunkIndex);
    this.meta.downloaded = this.rangeSet.getRanges();
    await this.persistMeta();
    await this.enforceCacheLimit(chunkIndex);
  }

  async flush(): Promise<void> {
    await this.loadMeta();
    await this.persistMeta();
  }

  async clear(): Promise<void> {
    await this.store.remove(`state/remote-cache/${this.cacheKey}`, { recursive: true });
    this.meta = {
      version: 1,
      manifestUrl: this.manifestUrl,
      totalSize: this.manifest.totalSize,
      chunkSize: this.manifest.chunkSize,
      chunkCount: this.manifest.chunkCount,
      chunkIndexWidth: this.manifest.chunkIndexWidth,
      downloaded: [],
      accessCounter: 0,
      chunkLastAccess: {},
    };
    this.rangeSet = new RangeSet();
    this.cachedBytes = 0;
    this.metaWriteChain = Promise.resolve();
    this.metaLoaded = true;
  }

  private async persistMeta(): Promise<void> {
    // Multiple chunk fetches can complete concurrently; serialize meta writes so that
    // older snapshots don't race and overwrite newer metadata.
    this.metaWriteChain = this.metaWriteChain
      .catch(() => {
        // Keep the chain alive even if a previous write failed.
      })
      .then(async () => {
        const json = JSON.stringify(this.meta, null, 2);
        const data = new TextEncoder().encode(json);
        await this.store.write(this.metaPath(), data);
      });
    await this.metaWriteChain;
  }

  private async enforceCacheLimit(protectedChunk: number): Promise<void> {
    if (this.cacheLimitBytes === null) return;
    while (this.cachedBytes > this.cacheLimitBytes) {
      let lruChunk: number | null = null;
      let lruCounter = Number.POSITIVE_INFINITY;
      for (const [chunkStr, counter] of Object.entries(this.meta.chunkLastAccess)) {
        const idx = Number(chunkStr);
        if (!Number.isFinite(idx) || idx === protectedChunk) continue;
        if (counter < lruCounter) {
          lruCounter = counter;
          lruChunk = idx;
        }
      }
      if (lruChunk === null) break;

      const r = this.chunkRange(lruChunk);
      await this.store.remove(this.chunkPath(lruChunk)).catch(() => {});
      this.rangeSet.remove(r.start, r.end);
      delete this.meta.chunkLastAccess[String(lruChunk)];
      this.meta.downloaded = this.rangeSet.getRanges();
      this.cachedBytes = this.rangeSet.totalLen();
      await this.persistMeta();
    }
  }
}

export class RemoteChunkedDisk implements AsyncSectorDisk {
  readonly sectorSize = SECTOR_SIZE;
  readonly capacityBytes: number;

  private readonly manifestUrl: string;
  private readonly manifest: ParsedChunkedDiskManifest;
  private readonly chunkCache: ChunkCache;
  private readonly credentials: RequestCredentials;
  private readonly prefetchSequentialChunks: number;
  private readonly semaphore: Semaphore;
  private readonly maxAttempts: number;
  private readonly retryBaseDelayMs: number;
  private readonly abort = new AbortController();

  private readonly inflight = new Map<number, Promise<Uint8Array>>();
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
    manifestUrl: string,
    manifest: ParsedChunkedDiskManifest,
    chunkCache: ChunkCache,
    options: Required<Pick<RemoteChunkedDiskOptions, "credentials" | "prefetchSequentialChunks" | "maxAttempts" | "retryBaseDelayMs">> & {
      maxConcurrentFetches: number;
    },
  ) {
    this.manifestUrl = manifestUrl;
    this.manifest = manifest;
    this.capacityBytes = manifest.totalSize;
    this.chunkCache = chunkCache;
    this.credentials = options.credentials;
    this.prefetchSequentialChunks = options.prefetchSequentialChunks;
    this.semaphore = new Semaphore(options.maxConcurrentFetches);
    this.maxAttempts = options.maxAttempts;
    this.retryBaseDelayMs = options.retryBaseDelayMs;
  }

  static async open(manifestUrl: string, options: RemoteChunkedDiskOptions = {}): Promise<RemoteChunkedDisk> {
    if (!manifestUrl) throw new Error("manifestUrl must not be empty");

    const resolved: Required<RemoteChunkedDiskOptions> = {
      credentials: options.credentials ?? "same-origin",
      cacheLimitBytes: options.cacheLimitBytes ?? 512 * 1024 * 1024,
      maxConcurrentFetches: options.maxConcurrentFetches ?? 4,
      prefetchSequentialChunks: options.prefetchSequentialChunks ?? 2,
      maxAttempts: options.maxAttempts ?? 3,
      retryBaseDelayMs: options.retryBaseDelayMs ?? 200,
      store: options.store ?? (hasOpfsRoot() ? new OpfsStore() : new MemoryStore()),
      cacheBackend: options.cacheBackend ?? pickDefaultBackend(),
    };

    if (resolved.cacheLimitBytes !== null) {
      if (!Number.isSafeInteger(resolved.cacheLimitBytes) || resolved.cacheLimitBytes < 0) {
        throw new Error(`invalid cacheLimitBytes=${resolved.cacheLimitBytes}`);
      }
    }
    if (!Number.isSafeInteger(resolved.maxConcurrentFetches) || resolved.maxConcurrentFetches <= 0) {
      throw new Error(`invalid maxConcurrentFetches=${resolved.maxConcurrentFetches}`);
    }
    if (!Number.isSafeInteger(resolved.prefetchSequentialChunks) || resolved.prefetchSequentialChunks < 0) {
      throw new Error(`invalid prefetchSequentialChunks=${resolved.prefetchSequentialChunks}`);
    }
    if (!Number.isSafeInteger(resolved.maxAttempts) || resolved.maxAttempts <= 0) {
      throw new Error(`invalid maxAttempts=${resolved.maxAttempts}`);
    }
    if (!Number.isSafeInteger(resolved.retryBaseDelayMs) || resolved.retryBaseDelayMs < 0) {
      throw new Error(`invalid retryBaseDelayMs=${resolved.retryBaseDelayMs}`);
    }

    const resp = await fetch(manifestUrl, { method: "GET", credentials: resolved.credentials });
    if (!resp.ok) throw new Error(`failed to fetch manifest: ${resp.status}`);
    const json = (await resp.json()) as unknown;
    const manifest = parseManifest(json);

    const cacheKey = await stableCacheKey(manifestUrl);
    let cache: ChunkCache;
    if (options.store) {
      // Tests can inject an in-memory store to avoid depending on OPFS/IDB.
      const opfsCache = new RemoteChunkCache(resolved.store, cacheKey, manifestUrl, manifest, resolved.cacheLimitBytes);
      await opfsCache.loadMeta();
      cache = opfsCache;
    } else if (resolved.cacheBackend === "idb" && typeof indexedDB !== "undefined") {
      const idbCache = await IdbRemoteChunkCache.open({
        cacheKey,
        signature: {
          imageId: (json as ChunkedDiskManifestV1).imageId ?? manifestUrl,
          version: (json as ChunkedDiskManifestV1).version,
          etag: resp.headers.get("etag"),
          sizeBytes: manifest.totalSize,
          chunkSize: manifest.chunkSize,
        },
        cacheLimitBytes: resolved.cacheLimitBytes,
      });
      cache = new IdbChunkCache(idbCache, manifest);
    } else {
      const store = hasOpfsRoot() ? new OpfsStore() : new MemoryStore();
      const opfsCache = new RemoteChunkCache(store, cacheKey, manifestUrl, manifest, resolved.cacheLimitBytes);
      await opfsCache.loadMeta();
      cache = opfsCache;
    }

    return new RemoteChunkedDisk(manifestUrl, manifest, cache, {
      credentials: resolved.credentials,
      maxConcurrentFetches: resolved.maxConcurrentFetches,
      prefetchSequentialChunks: resolved.prefetchSequentialChunks,
      maxAttempts: resolved.maxAttempts,
      retryBaseDelayMs: resolved.retryBaseDelayMs,
    });
  }

  getTelemetrySnapshot(): RemoteDiskTelemetrySnapshot {
    return {
      url: this.manifestUrl,
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

    const startChunk = Math.floor(offset / this.manifest.chunkSize);
    const endChunk = Math.floor((offset + buffer.byteLength - 1) / this.manifest.chunkSize);

    const promises: Promise<Uint8Array>[] = [];
    for (let chunk = startChunk; chunk <= endChunk; chunk += 1) {
      promises.push(this.getChunk(chunk));
    }

    let pos = 0;
    for (let i = 0; i < promises.length; i += 1) {
      const chunkIndex = startChunk + i;
      const bytes = await promises[i]!;
      const chunkStart = chunkIndex * this.manifest.chunkSize;
      const within = offset > chunkStart ? offset - chunkStart : 0;
      const toCopy = Math.min(buffer.byteLength - pos, bytes.length - within);
      buffer.set(bytes.subarray(within, within + toCopy), pos);
      pos += toCopy;
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
    await this.chunkCache.clear();
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
  }

  async close(): Promise<void> {
    if (this.closed) return;
    this.closed = true;
    this.abort.abort();
    await Promise.allSettled(Array.from(this.inflight.values()));
    this.inflight.clear();
    await this.flush().catch(() => {});
    this.chunkCache.close?.();
  }

  private chunkUrl(chunkIndex: number): string {
    const name = String(chunkIndex).padStart(this.manifest.chunkIndexWidth, "0");
    return new URL(`chunks/${name}.bin`, this.manifestUrl).toString();
  }

  private shouldRetry(err: unknown): boolean {
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

  private async fetchChunkOnce(chunkIndex: number, generation: number): Promise<Uint8Array> {
    const expectedLen = this.manifest.chunkSizes[chunkIndex]!;
    const expectedSha = this.manifest.chunkSha256[chunkIndex];
    const url = this.chunkUrl(chunkIndex);

    if (generation === this.cacheGeneration) {
      this.telemetry.requests += 1;
    }

    const resp = await fetch(url, {
      method: "GET",
      credentials: this.credentials,
      signal: this.abort.signal,
    });
    if (!resp.ok) {
      throw new ChunkFetchError(`chunk fetch failed: ${resp.status}`, resp.status);
    }
    const bytes = new Uint8Array(await resp.arrayBuffer());
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

  private async fetchChunkWithRetries(chunkIndex: number, generation: number): Promise<Uint8Array> {
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

  private async getChunk(chunkIndex: number): Promise<Uint8Array> {
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

    const nextChunk = Math.floor((offset + length) / this.manifest.chunkSize);
    for (let i = 0; i < this.prefetchSequentialChunks; i += 1) {
      const chunk = nextChunk + i;
      if (chunk >= this.manifest.chunkCount) break;
      void this.getChunk(chunk).catch(() => {
        // best-effort
      });
    }
  }
}
