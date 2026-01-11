import { assertSectorAligned, checkedOffset, SECTOR_SIZE, type AsyncSectorDisk } from "./disk";
import { RANGE_STREAM_CHUNK_SIZE } from "./chunk_sizes";
import { opfsGetDisksDir } from "./metadata";
import { OpfsAeroSparseDisk } from "./opfs_sparse";
import type { RemoteDiskBaseSnapshot } from "./runtime_disk_snapshot";
import type { ByteRange, RemoteDiskTelemetrySnapshot } from "../platform/remote_disk";

export function defaultRemoteRangeUrl(base: RemoteDiskBaseSnapshot): string {
  // NOTE: This is intentionally *not* a signed URL. Auth is expected to be handled
  // by the environment (same-origin session cookies, signed cookies, etc).
  return `/images/${encodeURIComponent(base.imageId)}/${encodeURIComponent(base.version)}/disk.img`;
}

export type RemoteRangeDiskTelemetry = {
  bytesDownloaded: number;
  rangeRequests: number;
  cacheHitChunks: number;
  cacheMissChunks: number;
};

type RemoteRangeDiskCacheMeta = {
  version: 1;
  /**
   * Stable identifier for the remote image (MUST NOT contain ephemeral tokens).
   */
  imageKey: string;
  sizeBytes: number;
  chunkSize: number;
  etag?: string;
  lastModified?: string;
};

export interface RemoteRangeDiskSparseCache extends AsyncSectorDisk {
  readonly blockSizeBytes: number;
  isBlockAllocated(blockIndex: number): boolean;
  writeBlock(blockIndex: number, data: Uint8Array): Promise<void>;
  readBlock(blockIndex: number, dst: Uint8Array): Promise<void>;
  /**
   * Returns the number of bytes currently materialized in the sparse file.
   *
   * This is intended for telemetry; it may include a partially-written final block.
   */
  getAllocatedBytes(): number;
}

export interface RemoteRangeDiskSparseCacheFactory {
  open(cacheId: string): Promise<RemoteRangeDiskSparseCache>;
  create(cacheId: string, opts: { diskSizeBytes: number; blockSizeBytes: number }): Promise<RemoteRangeDiskSparseCache>;
  delete?(cacheId: string): Promise<void>;
}

export interface RemoteRangeDiskMetadataStore {
  read(cacheId: string): Promise<RemoteRangeDiskCacheMeta | null>;
  write(cacheId: string, meta: RemoteRangeDiskCacheMeta): Promise<void>;
  delete(cacheId: string): Promise<void>;
}

export type RemoteRangeDiskOptions = {
  /**
   * Stable identifier for the remote image, used to key the persistent cache.
   *
   * If omitted, one is derived from `url` with query/hash stripped.
   */
  imageKey?: string;
  chunkSize?: number;
  maxConcurrentFetches?: number;
  maxRetries?: number;
  readAheadChunks?: number;
  /**
   * Optional per-chunk SHA-256 manifest; each entry must be a lowercase hex digest.
   * If provided, downloaded chunks are verified before being persisted to cache.
   */
  sha256Manifest?: string[];
  metadataStore?: RemoteRangeDiskMetadataStore;
  sparseCacheFactory?: RemoteRangeDiskSparseCacheFactory;
  fetchFn?: typeof fetch;
  /**
   * Base delay (ms) for exponential backoff retries.
   */
  retryBaseDelayMs?: number;
};

type ResolvedRemoteRangeDiskOptions = Required<
  Pick<
    RemoteRangeDiskOptions,
    | "chunkSize"
    | "maxConcurrentFetches"
    | "maxRetries"
    | "readAheadChunks"
    | "retryBaseDelayMs"
    | "fetchFn"
  >
> &
  Pick<RemoteRangeDiskOptions, "sha256Manifest">;

type RemoteProbe = {
  sizeBytes: number;
  etag?: string;
  lastModified?: string;
};

class HttpStatusError extends Error {
  constructor(
    message: string,
    readonly status: number,
  ) {
    super(message);
  }
}

class RemoteValidatorMismatchError extends Error {
  constructor(readonly status: number) {
    super(`remote validator mismatch (status=${status})`);
  }
}

async function cancelBody(resp: Response): Promise<void> {
  try {
    await resp.body?.cancel();
  } catch {
    // ignore best-effort cancellation failures
  }
}

class Semaphore {
  private inUse = 0;
  private readonly waiters: Array<() => void> = [];

  constructor(private readonly capacity: number) {
    if (!Number.isInteger(capacity) || capacity <= 0) {
      throw new Error(`invalid semaphore capacity=${capacity}`);
    }
  }

  async acquire(): Promise<() => void> {
    if (this.inUse < this.capacity) {
      this.inUse += 1;
      return () => this.release();
    }
    await new Promise<void>((resolve) => {
      this.waiters.push(resolve);
    });
    this.inUse += 1;
    return () => this.release();
  }

  private release(): void {
    this.inUse -= 1;
    if (this.inUse < 0) this.inUse = 0;
    const next = this.waiters.shift();
    next?.();
  }
}

function isPowerOfTwo(n: number): boolean {
  return (BigInt(n) & (BigInt(n) - 1n)) === 0n;
}

function assertValidChunkSize(chunkSize: number): void {
  if (!Number.isSafeInteger(chunkSize) || chunkSize <= 0) {
    throw new Error(`invalid chunkSize=${chunkSize}`);
  }
  if (chunkSize % SECTOR_SIZE !== 0) {
    throw new Error(`chunkSize must be a multiple of ${SECTOR_SIZE}`);
  }
  if (!isPowerOfTwo(chunkSize)) {
    throw new Error("chunkSize must be a power of two");
  }
}

function assertNonNegativeSafeInteger(value: number, label: string): void {
  if (!Number.isSafeInteger(value) || value < 0) {
    throw new Error(`${label} must be a non-negative safe integer`);
  }
}

function toSafeNumber(value: bigint, label: string): number {
  const n = Number(value);
  if (!Number.isSafeInteger(n)) {
    throw new Error(`${label} is not a safe JS integer (${value})`);
  }
  return n;
}

function parseContentRangeHeader(header: string): { start: number; endInclusive: number; total: number } {
  // Example: "bytes 0-0/12345"
  const m = /^bytes\s+(\d+)-(\d+)\/(\d+|\*)$/i.exec(header.trim());
  if (!m) {
    throw new Error(`invalid Content-Range: ${header}`);
  }
  const start = BigInt(m[1]);
  const endInclusive = BigInt(m[2]);
  if (endInclusive < start) {
    throw new Error(`invalid Content-Range: ${header}`);
  }
  const totalRaw = m[3];
  if (totalRaw === "*") {
    throw new Error(`unsupported Content-Range total='*': ${header}`);
  }
  const total = BigInt(totalRaw);
  if (total <= 0n) {
    throw new Error(`invalid Content-Range total: ${header}`);
  }
  return {
    start: toSafeNumber(start, "content-range start"),
    endInclusive: toSafeNumber(endInclusive, "content-range endInclusive"),
    total: toSafeNumber(total, "content-range total"),
  };
}

function deriveStableImageKey(url: string): string {
  try {
    const base = typeof location !== "undefined" ? location.href : undefined;
    const parsed = base ? new URL(url, base) : new URL(url);
    parsed.hash = "";
    parsed.search = "";
    return parsed.toString();
  } catch {
    // Best-effort: strip query/hash without needing URL parsing.
    return url.split("#")[0]!.split("?")[0]!;
  }
}

function bytesToHex(bytes: Uint8Array): string {
  let out = "";
  for (let i = 0; i < bytes.length; i++) {
    out += bytes[i]!.toString(16).padStart(2, "0");
  }
  return out;
}

async function sha256Hex(data: Uint8Array): Promise<string> {
  const subtle = (globalThis as typeof globalThis & { crypto?: Crypto }).crypto?.subtle;
  if (!subtle) {
    throw new Error("sha256 manifest verification requires WebCrypto (crypto.subtle)");
  }
  const digest = await subtle.digest("SHA-256", data);
  return bytesToHex(new Uint8Array(digest));
}

async function stableCacheId(key: string): Promise<string> {
  try {
    const subtle = (globalThis as typeof globalThis & { crypto?: Crypto }).crypto?.subtle;
    if (!subtle) throw new Error("missing crypto.subtle");
    const data = new TextEncoder().encode(key);
    const digest = await subtle.digest("SHA-256", data);
    return bytesToHex(new Uint8Array(digest));
  } catch {
    return encodeURIComponent(key).replaceAll("%", "_").slice(0, 128);
  }
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

function isRetryableHttpStatus(status: number): boolean {
  return status === 408 || status === 429 || status >= 500;
}

function isRetryableError(err: unknown): boolean {
  if (err instanceof RemoteValidatorMismatchError) return false;
  if (err instanceof HttpStatusError) return isRetryableHttpStatus(err.status);
  // Most other errors are treated as retryable because they might be transient (network or CDN hiccup).
  return true;
}

async function probeRemoteImage(url: string, fetchFn: typeof fetch): Promise<RemoteProbe> {
  let sizeBytes: number | null = null;
  let etag: string | undefined;
  let lastModified: string | undefined;

  try {
    const head = await fetchFn(url, { method: "HEAD" });
    if (head.ok) {
      const lenStr = head.headers.get("content-length");
      if (lenStr) {
        const len = Number(lenStr);
        if (Number.isSafeInteger(len) && len > 0) {
          sizeBytes = len;
        }
      }
      etag = head.headers.get("etag") ?? undefined;
      lastModified = head.headers.get("last-modified") ?? undefined;
    }
  } catch {
    // ignore, fall back to range probe if needed
  }

  if (sizeBytes === null) {
    const probe = await fetchFn(url, { method: "GET", headers: { Range: "bytes=0-0" } });
    if (probe.status === 200) {
      await cancelBody(probe);
      throw new Error("remote server ignored Range probe (expected 206 Partial Content, got 200 OK)");
    }
    if (probe.status !== 206) {
      await cancelBody(probe);
      throw new HttpStatusError(`unexpected range probe status ${probe.status}`, probe.status);
    }

    const contentRange = probe.headers.get("content-range");
    if (!contentRange) {
      await cancelBody(probe);
      throw new Error(
        "Range probe returned 206 Partial Content, but Content-Range is not visible. " +
          "If this is cross-origin, the server must set Access-Control-Expose-Headers: Content-Range, Content-Length.",
      );
    }
    const parsed = parseContentRangeHeader(contentRange);
    if (parsed.start !== 0 || parsed.endInclusive !== 0) {
      await cancelBody(probe);
      throw new Error(`Range probe returned unexpected Content-Range: ${contentRange}`);
    }
    sizeBytes = parsed.total;

    // Ensure the body matches the probed range; avoid trusting the header alone.
    const body = new Uint8Array(await probe.arrayBuffer());
    if (body.byteLength !== 1) {
      throw new Error(`Range probe returned unexpected body length ${body.byteLength} (expected 1)`);
    }

    etag ??= probe.headers.get("etag") ?? undefined;
    lastModified ??= probe.headers.get("last-modified") ?? undefined;
  }

  if (sizeBytes === null || !Number.isSafeInteger(sizeBytes) || sizeBytes <= 0) {
    throw new Error("remote server did not provide a readable image size");
  }

  return { sizeBytes, etag, lastModified };
}

class OpfsRemoteRangeDiskMetadataStore implements RemoteRangeDiskMetadataStore {
  private static fileName(cacheId: string): string {
    return `remote-range-cache-${cacheId}.json`;
  }

  async read(cacheId: string): Promise<RemoteRangeDiskCacheMeta | null> {
    const dir = await opfsGetDisksDir();
    const name = OpfsRemoteRangeDiskMetadataStore.fileName(cacheId);
    try {
      const handle = await dir.getFileHandle(name, { create: false });
      const file = await handle.getFile();
      if (file.size === 0) return null;
      const raw = await file.text();
      const parsed = JSON.parse(raw) as Partial<RemoteRangeDiskCacheMeta> | null;
      if (!parsed || parsed.version !== 1) return null;
      if (typeof parsed.imageKey !== "string") return null;
      if (typeof parsed.sizeBytes !== "number") return null;
      if (typeof parsed.chunkSize !== "number") return null;
      return parsed as RemoteRangeDiskCacheMeta;
    } catch (err) {
      if (err instanceof DOMException && err.name === "NotFoundError") return null;
      return null;
    }
  }

  async write(cacheId: string, meta: RemoteRangeDiskCacheMeta): Promise<void> {
    const dir = await opfsGetDisksDir();
    const handle = await dir.getFileHandle(OpfsRemoteRangeDiskMetadataStore.fileName(cacheId), { create: true });
    const writable = await handle.createWritable({ keepExistingData: false });
    await writable.write(JSON.stringify(meta));
    await writable.close();
  }

  async delete(cacheId: string): Promise<void> {
    const dir = await opfsGetDisksDir();
    try {
      await dir.removeEntry(OpfsRemoteRangeDiskMetadataStore.fileName(cacheId));
    } catch (err) {
      if (err instanceof DOMException && err.name === "NotFoundError") return;
      // ignore other failures (best-effort)
    }
  }
}

class OpfsRemoteRangeDiskSparseCacheFactory implements RemoteRangeDiskSparseCacheFactory {
  private static fileName(cacheId: string): string {
    return `remote-range-cache-${cacheId}.aerospar`;
  }

  async open(cacheId: string): Promise<RemoteRangeDiskSparseCache> {
    return await OpfsAeroSparseDisk.open(OpfsRemoteRangeDiskSparseCacheFactory.fileName(cacheId));
  }

  async create(
    cacheId: string,
    opts: { diskSizeBytes: number; blockSizeBytes: number },
  ): Promise<RemoteRangeDiskSparseCache> {
    return await OpfsAeroSparseDisk.create(OpfsRemoteRangeDiskSparseCacheFactory.fileName(cacheId), opts);
  }

  async delete(cacheId: string): Promise<void> {
    const dir = await opfsGetDisksDir();
    try {
      await dir.removeEntry(OpfsRemoteRangeDiskSparseCacheFactory.fileName(cacheId));
    } catch (err) {
      if (err instanceof DOMException && err.name === "NotFoundError") return;
      // ignore other failures (best-effort)
    }
  }
}

export class RemoteRangeDisk implements AsyncSectorDisk {
  readonly sectorSize = SECTOR_SIZE;

  private capacityBytesValue = 0;

  private remoteEtag: string | undefined;
  private remoteLastModified: string | undefined;

  private cache: RemoteRangeDiskSparseCache | null = null;
  private cacheId = "";
  private imageKey = "";
  private cacheGeneration = 0;

  private readonly inflightChunks = new Map<number, { generation: number; promise: Promise<void> }>();
  private readonly fetchSemaphore: Semaphore;
  private invalidationPromise: Promise<void> | null = null;

  private lastReadEnd: number | null = null;

  private telemetry: RemoteRangeDiskTelemetry = {
    bytesDownloaded: 0,
    rangeRequests: 0,
    cacheHitChunks: 0,
    cacheMissChunks: 0,
  };
  private blockRequests = 0;
  private inflightJoins = 0;
  private lastFetchMs: number | null = null;
  private lastFetchAtMs: number | null = null;
  private lastFetchRange: ByteRange | null = null;

  private flushTimer: ReturnType<typeof setTimeout> | null = null;
  private flushPending = false;

  private constructor(
    private readonly url: string,
    private readonly opts: ResolvedRemoteRangeDiskOptions,
    private readonly sha256Manifest: string[] | undefined,
    private readonly metadataStore: RemoteRangeDiskMetadataStore,
    private readonly sparseCacheFactory: RemoteRangeDiskSparseCacheFactory,
    fetchSemaphore: Semaphore,
  ) {
    this.fetchSemaphore = fetchSemaphore;
  }

  get capacityBytes(): number {
    return this.capacityBytesValue;
  }

  getTelemetry(): RemoteRangeDiskTelemetry {
    return { ...this.telemetry };
  }

  getTelemetrySnapshot(): RemoteDiskTelemetrySnapshot {
    const cache = this.cache;
    const totalSize = this.capacityBytesValue;
    const blockSize = this.opts.chunkSize;
    // The sparse cache stores fixed-size blocks, so its "allocated bytes" can exceed the
    // remote image size when the final block is partial. Convert back to remote bytes so
    // telemetry is consistent with other remote disk implementations.
    let cachedBytes = cache ? cache.getAllocatedBytes() : 0;
    const remainder = totalSize % blockSize;
    if (cache && remainder !== 0 && totalSize > 0) {
      const lastBlockIndex = Math.floor((totalSize - 1) / blockSize);
      if (cache.isBlockAllocated(lastBlockIndex)) {
        cachedBytes -= blockSize - remainder;
      }
    }
    if (cachedBytes < 0) cachedBytes = 0;
    if (cachedBytes > totalSize) cachedBytes = totalSize;
    return {
      url: this.url,
      totalSize,
      blockSize,
      cacheLimitBytes: null,
      cachedBytes,

      blockRequests: this.blockRequests,
      cacheHits: this.telemetry.cacheHitChunks,
      cacheMisses: this.telemetry.cacheMissChunks,
      inflightJoins: this.inflightJoins,

      requests: this.telemetry.rangeRequests,
      bytesDownloaded: this.telemetry.bytesDownloaded,

      inflightFetches: this.inflightChunks.size,

      lastFetchMs: this.lastFetchMs,
      lastFetchAtMs: this.lastFetchAtMs,
      lastFetchRange: this.lastFetchRange ? { ...this.lastFetchRange } : null,
    };
  }

  static async open(url: string, options: RemoteRangeDiskOptions = {}): Promise<RemoteRangeDisk> {
    const chunkSize = options.chunkSize ?? RANGE_STREAM_CHUNK_SIZE;
    const maxConcurrentFetches = options.maxConcurrentFetches ?? 4;
    const maxRetries = options.maxRetries ?? 4;
    const readAheadChunks = options.readAheadChunks ?? 2;
    const retryBaseDelayMs = options.retryBaseDelayMs ?? 100;

    assertValidChunkSize(chunkSize);
    if (!Number.isInteger(maxConcurrentFetches) || maxConcurrentFetches <= 0) {
      throw new Error(`invalid maxConcurrentFetches=${maxConcurrentFetches}`);
    }
    if (!Number.isInteger(maxRetries) || maxRetries < 0) {
      throw new Error(`invalid maxRetries=${maxRetries}`);
    }
    if (!Number.isInteger(readAheadChunks) || readAheadChunks < 0) {
      throw new Error(`invalid readAheadChunks=${readAheadChunks}`);
    }
    if (!Number.isInteger(retryBaseDelayMs) || retryBaseDelayMs <= 0) {
      throw new Error(`invalid retryBaseDelayMs=${retryBaseDelayMs}`);
    }

    const fetchFn = options.fetchFn ?? fetch;
    const resolvedOpts: ResolvedRemoteRangeDiskOptions = {
      chunkSize,
      maxConcurrentFetches,
      maxRetries,
      readAheadChunks,
      retryBaseDelayMs,
      fetchFn,
    };

    const imageKey = options.imageKey ?? deriveStableImageKey(url);
    if (!imageKey) {
      throw new Error("imageKey must not be empty");
    }
    const cacheId = await stableCacheId(imageKey);

    const metadataStore = options.metadataStore ?? new OpfsRemoteRangeDiskMetadataStore();
    const sparseCacheFactory = options.sparseCacheFactory ?? new OpfsRemoteRangeDiskSparseCacheFactory();

    const disk = new RemoteRangeDisk(
      url,
      resolvedOpts,
      options.sha256Manifest,
      metadataStore,
      sparseCacheFactory,
      new Semaphore(maxConcurrentFetches),
    );
    disk.cacheId = cacheId;
    disk.imageKey = imageKey;
    try {
      await disk.init();
    } catch (err) {
      // `init()` can fail after opening a persistent cache handle. Ensure we close it so we
      // don't leak SyncAccessHandles / file descriptors.
      await disk.close().catch(() => {});
      throw err;
    }
    return disk;
  }

  private async init(): Promise<void> {
    const remote = await probeRemoteImage(this.url, this.opts.fetchFn);

    this.capacityBytesValue = remote.sizeBytes;
    this.remoteEtag = remote.etag;
    this.remoteLastModified = remote.lastModified;

    const existingMeta = await this.metadataStore.read(this.cacheId);
    const compatible = existingMeta ? this.isMetaCompatible(existingMeta, remote) : false;

    if (!compatible) {
      // Best-effort cleanup of old metadata; ignore failures.
      await this.metadataStore.delete(this.cacheId);
    }

    const cache = await this.openOrCreateCache(remote, compatible);
    this.cache = cache;

    const metaToPersist: RemoteRangeDiskCacheMeta = {
      version: 1,
      imageKey: this.imageKey,
      sizeBytes: remote.sizeBytes,
      chunkSize: this.opts.chunkSize,
      etag: remote.etag,
      lastModified: remote.lastModified,
    };
    await this.metadataStore.write(this.cacheId, metaToPersist);

    // If the remote didn't expose ETag/Last-Modified, reuse whatever we had in metadata
    // so that we can still use If-Range across sessions.
    this.remoteEtag ??= existingMeta?.etag;
    this.remoteLastModified ??= existingMeta?.lastModified;
  }

  private isMetaCompatible(meta: RemoteRangeDiskCacheMeta, remote: RemoteProbe): boolean {
    if (!meta || meta.version !== 1) return false;
    if (meta.imageKey !== this.imageKey) return false;
    if (meta.chunkSize !== this.opts.chunkSize) return false;
    if (meta.sizeBytes !== remote.sizeBytes) return false;

    // Prefer ETag when the server exposes it; otherwise fall back to Last-Modified.
    if (remote.etag) {
      return meta.etag === remote.etag;
    }
    if (remote.lastModified) {
      return meta.lastModified === remote.lastModified;
    }
    // No validator exposed; size+chunk alignment is all we can validate.
    return true;
  }

  private async openOrCreateCache(remote: RemoteProbe, compatible: boolean): Promise<RemoteRangeDiskSparseCache> {
    if (compatible) {
      try {
        const opened = await this.sparseCacheFactory.open(this.cacheId);
        if (opened.capacityBytes === remote.sizeBytes && opened.blockSizeBytes === this.opts.chunkSize) {
          return opened;
        }
        await opened.close?.();
      } catch {
        // Fall back to create below.
      }
    }

    return await this.sparseCacheFactory.create(this.cacheId, {
      diskSizeBytes: remote.sizeBytes,
      blockSizeBytes: this.opts.chunkSize,
    });
  }

  private ensureOpen(): RemoteRangeDiskSparseCache {
    if (!this.cache) {
      throw new Error("RemoteRangeDisk is closed");
    }
    return this.cache;
  }

  async readSectors(lba: number, buffer: Uint8Array): Promise<void> {
    if (this.invalidationPromise) {
      await this.invalidationPromise;
    }
    const generation = this.cacheGeneration;
    const cache = this.ensureOpen();
    assertSectorAligned(buffer.byteLength, this.sectorSize);
    const offset = checkedOffset(lba, buffer.byteLength, this.sectorSize);
    if (offset + buffer.byteLength > this.capacityBytesValue) {
      throw new Error("read past end of disk");
    }

    if (buffer.byteLength === 0) {
      this.lastReadEnd = offset;
      return;
    }

    const startChunk = Math.floor(offset / this.opts.chunkSize);
    const endChunk = Math.floor((offset + buffer.byteLength - 1) / this.opts.chunkSize);

    const pending: Array<Promise<void>> = [];
    for (let chunk = startChunk; chunk <= endChunk; chunk++) {
      pending.push(this.ensureChunkCached(chunk));
    }
    await Promise.all(pending);

    if (generation !== this.cacheGeneration) {
      // Cache was invalidated while awaiting downloads; restart the read against the new cache.
      return await this.readSectors(lba, buffer);
    }

    await this.ensureOpen().readSectors(lba, buffer);
    this.scheduleReadAhead(offset, buffer.byteLength, endChunk);
  }

  async writeSectors(_lba: number, _data: Uint8Array): Promise<void> {
    throw new Error("RemoteRangeDisk is read-only");
  }

  async flush(): Promise<void> {
    if (this.invalidationPromise) {
      await this.invalidationPromise;
    }
    await this.ensureOpen().flush();
  }

  async clearCache(): Promise<void> {
    if (this.invalidationPromise) {
      await this.invalidationPromise;
    }
    this.cacheGeneration += 1;
    this.inflightChunks.clear();
    this.lastReadEnd = null;
    this.resetTelemetry();

    if (this.flushTimer !== null) {
      clearTimeout(this.flushTimer);
      this.flushTimer = null;
    }
    this.flushPending = false;

    await this.metadataStore.delete(this.cacheId);

    const oldCache = this.cache;
    await oldCache?.close?.();

    const cache = await this.sparseCacheFactory.create(this.cacheId, {
      diskSizeBytes: this.capacityBytesValue,
      blockSizeBytes: this.opts.chunkSize,
    });
    this.cache = cache;

    const metaToPersist: RemoteRangeDiskCacheMeta = {
      version: 1,
      imageKey: this.imageKey,
      sizeBytes: this.capacityBytesValue,
      chunkSize: this.opts.chunkSize,
      etag: this.remoteEtag,
      lastModified: this.remoteLastModified,
    };
    await this.metadataStore.write(this.cacheId, metaToPersist);
  }

  async close(): Promise<void> {
    if (!this.cache) return;
    if (this.invalidationPromise) {
      await this.invalidationPromise;
    }
    if (this.flushTimer !== null) {
      clearTimeout(this.flushTimer);
      this.flushTimer = null;
    }
    this.flushPending = false;
    const cache = this.cache;
    this.cache = null;
    this.inflightChunks.clear();
    let flushErr: unknown;
    try {
      await cache.flush();
    } catch (err) {
      flushErr = err;
    }
    try {
      await cache.close?.();
    } catch (err) {
      if (!flushErr) flushErr = err;
    }
    if (flushErr) throw flushErr;
  }

  private scheduleReadAhead(offset: number, length: number, endChunk: number): void {
    const sequential = this.lastReadEnd !== null && this.lastReadEnd === offset;
    this.lastReadEnd = offset + length;
    if (!sequential) return;
    if (this.opts.readAheadChunks <= 0) return;

    for (let i = 1; i <= this.opts.readAheadChunks; i++) {
      const nextChunk = endChunk + i;
      const start = nextChunk * this.opts.chunkSize;
      if (start >= this.capacityBytesValue) break;
      void this.ensureChunkCached(nextChunk).catch(() => {
        // best-effort prefetch
      });
    }
  }

  private async ensureChunkCached(chunkIndex: number): Promise<void> {
    assertNonNegativeSafeInteger(chunkIndex, "chunkIndex");
    if (this.invalidationPromise) {
      await this.invalidationPromise;
    }
    const generation = this.cacheGeneration;
    const cache = this.ensureOpen();

    const blockStart = chunkIndex * this.opts.chunkSize;
    if (!Number.isSafeInteger(blockStart)) {
      throw new Error("chunk offset overflow");
    }
    if (blockStart >= this.capacityBytesValue) {
      throw new Error("chunkIndex out of range");
    }

    this.blockRequests += 1;
    if (cache.isBlockAllocated(chunkIndex)) {
      this.telemetry.cacheHitChunks += 1;
      return;
    }

    const inflight = this.inflightChunks.get(chunkIndex);
    if (inflight && inflight.generation === generation) {
      this.inflightJoins += 1;
      return await inflight.promise;
    }

    this.telemetry.cacheMissChunks += 1;
    const end = Math.min(blockStart + this.opts.chunkSize, this.capacityBytesValue);
    this.lastFetchRange = { start: blockStart, end };
    const promise = this.fetchAndStoreChunk(chunkIndex, generation);
    this.inflightChunks.set(chunkIndex, { generation, promise });
    try {
      await promise;
    } finally {
      const current = this.inflightChunks.get(chunkIndex);
      if (current?.promise === promise) {
        this.inflightChunks.delete(chunkIndex);
      }
    }
  }

  private async fetchAndStoreChunk(chunkIndex: number, generation: number): Promise<void> {
    let invalidations = 0;
    while (true) {
      if (generation !== this.cacheGeneration) {
        // Cache was invalidated while we were waiting in the task queue.
        return await this.ensureChunkCached(chunkIndex);
      }

      const cache = this.ensureOpen();
      if (cache.isBlockAllocated(chunkIndex)) return;

      try {
        const start = performance.now();
        const bytes = await this.downloadChunkWithRetries(chunkIndex, generation);
        if (generation !== this.cacheGeneration) {
          // Cache invalidated after download; discard and let the caller retry.
          continue;
        }

        await cache.writeBlock(chunkIndex, bytes);
        if (generation === this.cacheGeneration) {
          this.lastFetchMs = performance.now() - start;
          this.lastFetchAtMs = Date.now();
        }
        this.scheduleBackgroundFlush();
        return;
      } catch (err) {
        if (err instanceof RemoteValidatorMismatchError && invalidations < 1) {
          invalidations += 1;
          await this.invalidateAndReopenCache();
          continue;
        }
        throw err;
      }
    }
  }

  private async downloadChunkWithRetries(chunkIndex: number, generation: number): Promise<Uint8Array> {
    let lastErr: unknown;
    for (let attempt = 0; attempt <= this.opts.maxRetries; attempt++) {
      const release = await this.fetchSemaphore.acquire();
      try {
        return await this.downloadChunkOnce(chunkIndex, generation);
      } catch (err) {
        lastErr = err;
        if (attempt >= this.opts.maxRetries || !isRetryableError(err)) {
          throw err;
        }
        const delay = this.opts.retryBaseDelayMs * Math.pow(2, attempt);
        await sleep(delay);
      } finally {
        release();
      }
    }
    throw lastErr instanceof Error ? lastErr : new Error(String(lastErr));
  }

  private async downloadChunkOnce(chunkIndex: number, generation: number): Promise<Uint8Array> {
    const start = chunkIndex * this.opts.chunkSize;
    const endExclusive = Math.min(start + this.opts.chunkSize, this.capacityBytesValue);
    if (endExclusive <= start) {
      throw new Error("chunk range is empty");
    }
    const endInclusive = endExclusive - 1;
    const expectedLen = endExclusive - start;

    if (!Number.isSafeInteger(start) || !Number.isSafeInteger(endInclusive) || !Number.isSafeInteger(expectedLen)) {
      throw new Error("chunk range overflow");
    }

    const headers: Record<string, string> = {
      Range: `bytes=${start}-${endInclusive}`,
    };
    if (this.remoteEtag) {
      headers["If-Range"] = this.remoteEtag;
    } else if (this.remoteLastModified) {
      // If-Range also accepts an HTTP-date; `Last-Modified` is already formatted as one.
      headers["If-Range"] = this.remoteLastModified;
    }
    const hasIfRange = "If-Range" in headers;

    if (generation === this.cacheGeneration) {
      this.telemetry.rangeRequests += 1;
      this.lastFetchRange = { start, end: endExclusive };
    }
    const resp = await this.opts.fetchFn(this.url, { method: "GET", headers });

    if (resp.status === 200 || resp.status === 412) {
      // Don't read the body — it could be a multi-GB full response.
      await cancelBody(resp);
      // If-Range mismatch (or a server that ignores Range entirely).
      if (hasIfRange) {
        throw new RemoteValidatorMismatchError(resp.status);
      }
      throw new Error(`remote server ignored Range request (expected 206, got ${resp.status})`);
    }

    if (resp.status !== 206) {
      await cancelBody(resp);
      throw new HttpStatusError(`unexpected range response status ${resp.status}`, resp.status);
    }

    const contentRange = resp.headers.get("content-range");
    if (!contentRange) {
      await cancelBody(resp);
      throw new Error(
        "Range request returned 206 Partial Content, but Content-Range is not visible. " +
          "If this is cross-origin, the server must set Access-Control-Expose-Headers: Content-Range, Content-Length.",
      );
    }

    const parsed = parseContentRangeHeader(contentRange);
    if (parsed.start !== start || parsed.endInclusive !== endInclusive) {
      await cancelBody(resp);
      throw new Error(`Content-Range mismatch: expected bytes ${start}-${endInclusive}, got ${contentRange}`);
    }
    if (parsed.total !== this.capacityBytesValue) {
      // Image size changed without us noticing; treat like an invalidation event.
      await cancelBody(resp);
      throw new RemoteValidatorMismatchError(206);
    }

    const body = new Uint8Array(await resp.arrayBuffer());
    if (generation === this.cacheGeneration) {
      this.telemetry.bytesDownloaded += body.byteLength;
    }

    if (body.byteLength !== expectedLen) {
      throw new Error(`short range read: expected=${expectedLen} actual=${body.byteLength}`);
    }

    if (this.sha256Manifest) {
      const expected = this.sha256Manifest[chunkIndex];
      if (expected) {
        const actual = await sha256Hex(body);
        if (actual !== expected) {
          throw new Error(`sha256 mismatch for chunk ${chunkIndex}`);
        }
      }
    }

    if (body.byteLength === this.opts.chunkSize) {
      return body;
    }

    // Last chunk: pad to full blockSize for the sparse cache.
    const padded = new Uint8Array(this.opts.chunkSize);
    padded.set(body);
    return padded;
  }

  private resetTelemetry(): void {
    this.telemetry = {
      bytesDownloaded: 0,
      rangeRequests: 0,
      cacheHitChunks: 0,
      cacheMissChunks: 0,
    };
    this.blockRequests = 0;
    this.inflightJoins = 0;
    this.lastFetchMs = null;
    this.lastFetchAtMs = null;
    this.lastFetchRange = null;
  }

  private scheduleBackgroundFlush(): void {
    if (this.flushPending) return;
    this.flushPending = true;

    // Defer flushing until after the critical read completes. `OpfsAeroSparseDisk.flush()`
    // is synchronous under the hood, so even an un-awaited call would still block the
    // current microtask queue. Use a macrotask to keep the caller latency low.
    this.flushTimer = setTimeout(() => {
      this.flushTimer = null;
      const cache = this.cache;
      if (!cache) {
        this.flushPending = false;
        return;
      }
      void cache
        .flush()
        .catch(() => {
          // best-effort cache durability
        })
        .finally(() => {
          this.flushPending = false;
        });
    }, 0);
  }

  private async invalidateAndReopenCache(): Promise<void> {
    if (this.invalidationPromise) return await this.invalidationPromise;

    this.invalidationPromise = (async () => {
      this.cacheGeneration += 1;
      this.inflightChunks.clear();

      if (this.flushTimer !== null) {
        clearTimeout(this.flushTimer);
        this.flushTimer = null;
      }
      this.flushPending = false;

      await this.metadataStore.delete(this.cacheId);

      const remote = await probeRemoteImage(this.url, this.opts.fetchFn);
      this.capacityBytesValue = remote.sizeBytes;
      this.remoteEtag = remote.etag;
      this.remoteLastModified = remote.lastModified;

      const oldCache = this.cache;
      await oldCache?.close?.();

      const cache = await this.sparseCacheFactory.create(this.cacheId, {
        diskSizeBytes: remote.sizeBytes,
        blockSizeBytes: this.opts.chunkSize,
      });
      this.cache = cache;

      const metaToPersist: RemoteRangeDiskCacheMeta = {
        version: 1,
        imageKey: this.imageKey,
        sizeBytes: remote.sizeBytes,
        chunkSize: this.opts.chunkSize,
        etag: remote.etag,
        lastModified: remote.lastModified,
      };
      await this.metadataStore.write(this.cacheId, metaToPersist);
    })();

    try {
      await this.invalidationPromise;
    } finally {
      this.invalidationPromise = null;
    }
  }
}
