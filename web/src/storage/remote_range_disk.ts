import { RangeSet, type ByteRange, type RemoteDiskTelemetrySnapshot } from "../platform/remote_disk";
import { assertSectorAligned, checkedOffset, SECTOR_SIZE, type AsyncSectorDisk } from "./disk";
import { RANGE_STREAM_CHUNK_SIZE } from "./chunk_sizes";
import { opfsGetRemoteCacheDir } from "./metadata";
import { OpfsAeroSparseDisk } from "./opfs_sparse";
import {
  DEFAULT_LEASE_REFRESH_MARGIN_MS,
  DiskAccessLeaseRefresher,
  fetchWithDiskAccessLease,
  type DiskAccessLease,
} from "./disk_access_lease";
import type { RemoteDiskBaseSnapshot } from "./runtime_disk_snapshot";
import { RemoteCacheManager, type RemoteCacheKeyParts, type RemoteCacheMetaV1 } from "./remote_cache_manager";

// Keep in sync with the Rust snapshot bounds where sensible.
const MAX_REMOTE_CHUNK_SIZE_BYTES = 64 * 1024 * 1024; // 64 MiB

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

type RemoteRangeDiskCacheMeta = RemoteCacheMetaV1;

const META_PERSIST_DEBOUNCE_MS = 50;

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
   * Stable cache identity for the remote image.
   *
   * MUST NOT be derived from signed URLs / bearer tokens.
   */
  cacheKeyParts: RemoteCacheKeyParts;
  /**
   * Fetch credential mode for Range requests.
   *
   * Defaults to `same-origin` so cookies are sent for same-origin endpoints but
   * not for cross-origin requests (avoids credentialed CORS unless explicitly
   * requested).
   */
  credentials?: RequestCredentials;
  chunkSize?: number;
  maxConcurrentFetches?: number;
  maxRetries?: number;
  readAheadChunks?: number;
  /**
   * For lease-based access, refresh shortly before `expiresAt`.
   */
  leaseRefreshMarginMs?: number;
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

function isAbortError(err: unknown): boolean {
  return err instanceof Error && err.name === "AbortError";
}

function makeAbortError(): Error {
  // Ensure a stable `.name === "AbortError"` across runtimes.
  try {
    return new DOMException("The operation was aborted.", "AbortError");
  } catch {
    const err = new Error("The operation was aborted.");
    err.name = "AbortError";
    return err;
  }
}

function abortAny(signals: AbortSignal[]): AbortSignal {
  // Prefer the built-in combinator when available (avoids leaking listeners).
  const anyFn = (AbortSignal as unknown as { any?: (signals: AbortSignal[]) => AbortSignal }).any;
  if (anyFn) return anyFn(signals);

  // Fallback: create a composite signal. This can attach a small number of listeners over the disk lifetime.
  const controller = new AbortController();
  const onAbort = () => controller.abort();
  for (const s of signals) {
    if (s.aborted) {
      controller.abort();
      break;
    }
    s.addEventListener("abort", onAbort, { once: true });
  }
  return controller.signal;
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
  if (chunkSize > MAX_REMOTE_CHUNK_SIZE_BYTES) {
    throw new Error(`chunkSize too large: max=${MAX_REMOTE_CHUNK_SIZE_BYTES} got=${chunkSize}`);
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

function isWeakEtag(etag: string): boolean {
  const trimmed = etag.trimStart();
  return trimmed.startsWith("W/") || trimmed.startsWith("w/");
}

function validatorsMatch(expected: string, actual: string): boolean {
  const e = expected.trim();
  const a = actual.trim();

  const eWeak = e.startsWith("W/") || e.startsWith("w/");
  const aWeak = a.startsWith("W/") || a.startsWith("w/");

  if (eWeak && aWeak) {
    return e.slice(2).trimStart() === a.slice(2).trimStart();
  }

  return e === a;
}

function extractValidatorFromHeaders(headers: Headers): string | undefined {
  return (headers.get("etag") ?? headers.get("last-modified") ?? undefined) || undefined;
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

function parseUnsatisfiedContentRangeHeader(header: string): { total: number } {
  // Example: "bytes */12345" (used with 416 Range Not Satisfiable)
  const m = /^bytes\s+\*\/(\d+|\*)$/i.exec(header.trim());
  if (!m) {
    throw new Error(`invalid Content-Range: ${header}`);
  }
  const totalRaw = m[1];
  if (totalRaw === "*") {
    throw new Error(`unsupported Content-Range total='*': ${header}`);
  }
  const total = BigInt(totalRaw);
  if (total <= 0n) {
    throw new Error(`invalid Content-Range total: ${header}`);
  }
  return { total: toSafeNumber(total, "content-range total") };
}

function bytesToHex(bytes: Uint8Array): string {
  let out = "";
  for (let i = 0; i < bytes.length; i++) {
    out += bytes[i]!.toString(16).padStart(2, "0");
  }
  return out;
}

function toArrayBufferUint8(data: Uint8Array): Uint8Array<ArrayBuffer> {
  // Newer TS libdefs model typed arrays as `Uint8Array<ArrayBufferLike>`, while WebCrypto expects
  // `ArrayBuffer`-backed views. Most of our data comes from `Response.arrayBuffer()` and is already
  // ArrayBuffer-backed, so avoid copies when possible.
  return data.buffer instanceof ArrayBuffer ? (data as unknown as Uint8Array<ArrayBuffer>) : new Uint8Array(data);
}

async function sha256Hex(data: Uint8Array): Promise<string> {
  const subtle = (globalThis as typeof globalThis & { crypto?: Crypto }).crypto?.subtle;
  if (!subtle) {
    throw new Error("sha256 manifest verification requires WebCrypto (crypto.subtle)");
  }
  const digest = await subtle.digest("SHA-256", toArrayBufferUint8(data));
  return bytesToHex(new Uint8Array(digest));
}

function sleep(ms: number, signal?: AbortSignal): Promise<void> {
  if (!signal) {
    return new Promise((resolve) => {
      const timer = setTimeout(resolve, ms);
      (timer as unknown as { unref?: () => void }).unref?.();
    });
  }
  if (signal.aborted) {
    return Promise.reject(makeAbortError());
  }
  return new Promise((resolve, reject) => {
    let timer: ReturnType<typeof setTimeout>;
    const onAbort = () => {
      clearTimeout(timer);
      reject(makeAbortError());
    };

    timer = setTimeout(() => {
      signal.removeEventListener("abort", onAbort);
      resolve();
    }, ms);
    (timer as unknown as { unref?: () => void }).unref?.();

    signal.addEventListener("abort", onAbort, { once: true });
  });
}

function isRetryableHttpStatus(status: number): boolean {
  return status === 408 || status === 429 || status >= 500;
}

function isRetryableError(err: unknown): boolean {
  if (err instanceof RemoteValidatorMismatchError) return false;
  if (err instanceof HttpStatusError) return isRetryableHttpStatus(err.status);
  if (isAbortError(err)) return false;
  // Most other errors are treated as retryable because they might be transient (network or CDN hiccup).
  return true;
}

async function probeRemoteImage(
  lease: DiskAccessLease,
  fetchFn: typeof fetch,
  opts?: { signal?: AbortSignal },
): Promise<RemoteProbe> {
  let sizeBytes: number | null = null;
  let etag: string | undefined;
  let lastModified: string | undefined;

  try {
    const head = await fetchWithDiskAccessLease(
      lease,
      { method: "HEAD", signal: opts?.signal },
      { fetch: fetchFn, retryAuthOnce: true },
    );
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
    const probe = await fetchWithDiskAccessLease(
      lease,
      { method: "GET", headers: { Range: "bytes=0-0" }, signal: opts?.signal },
      { fetch: fetchFn, retryAuthOnce: true },
    );
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
  private managerPromise: Promise<RemoteCacheManager> | null = null;

  private async getManager(): Promise<RemoteCacheManager> {
    this.managerPromise ??= RemoteCacheManager.openOpfs();
    return await this.managerPromise;
  }

  async read(cacheId: string): Promise<RemoteRangeDiskCacheMeta | null> {
    return (await this.getManager()).readMeta(cacheId) as Promise<RemoteRangeDiskCacheMeta | null>;
  }

  async write(cacheId: string, meta: RemoteRangeDiskCacheMeta): Promise<void> {
    await (await this.getManager()).writeMeta(cacheId, meta);
  }

  async delete(cacheId: string): Promise<void> {
    await (await this.getManager()).clearCache(cacheId);
  }
}

class OpfsRemoteRangeDiskSparseCacheFactory implements RemoteRangeDiskSparseCacheFactory {
  private static baseFileName(): string {
    // Keep in sync with `RemoteCacheManager`'s canonical file names.
    return "base.aerospar";
  }

  private async getCacheDir(cacheId: string, create: boolean): Promise<FileSystemDirectoryHandle> {
    const root = await opfsGetRemoteCacheDir();
    return await root.getDirectoryHandle(cacheId, { create });
  }

  async open(cacheId: string): Promise<RemoteRangeDiskSparseCache> {
    const dir = await this.getCacheDir(cacheId, false);
    return await OpfsAeroSparseDisk.open(OpfsRemoteRangeDiskSparseCacheFactory.baseFileName(), { dir });
  }

  async create(
    cacheId: string,
    opts: { diskSizeBytes: number; blockSizeBytes: number },
  ): Promise<RemoteRangeDiskSparseCache> {
    const dir = await this.getCacheDir(cacheId, true);
    return await OpfsAeroSparseDisk.create(OpfsRemoteRangeDiskSparseCacheFactory.baseFileName(), { ...opts, dir });
  }

  async delete(cacheId: string): Promise<void> {
    const root = await opfsGetRemoteCacheDir();
    try {
      await root.removeEntry(cacheId, { recursive: true });
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
  private readonly cacheKeyParts: RemoteCacheKeyParts;
  private meta: RemoteRangeDiskCacheMeta | null = null;
  private rangeSet = new RangeSet();
  private metaWriteChain: Promise<void> = Promise.resolve();
  private metaDirty = false;
  private metaPersistTimer: ReturnType<typeof setTimeout> | null = null;
  private cacheGeneration = 0;

  private readonly inflightChunks = new Map<number, { generation: number; promise: Promise<void> }>();
  private readonly inflightWrites = new Set<Promise<void>>();
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
  private readonly leaseRefresher: DiskAccessLeaseRefresher;
  private closed = false;
  private readonly abort = new AbortController();
  private fetchAbort = new AbortController();
  private fetchSignal = abortAny([this.abort.signal, this.fetchAbort.signal]);

  private constructor(
    private readonly sourceId: string,
    private readonly lease: DiskAccessLease,
    private readonly opts: ResolvedRemoteRangeDiskOptions,
    private readonly leaseRefreshMarginMs: number,
    private readonly sha256Manifest: string[] | undefined,
    private readonly metadataStore: RemoteRangeDiskMetadataStore,
    private readonly sparseCacheFactory: RemoteRangeDiskSparseCacheFactory,
    fetchSemaphore: Semaphore,
    cacheKeyParts: RemoteCacheKeyParts,
  ) {
    this.fetchSemaphore = fetchSemaphore;
    this.cacheKeyParts = cacheKeyParts;
    this.leaseRefresher = new DiskAccessLeaseRefresher(this.lease, { refreshMarginMs: this.leaseRefreshMarginMs });
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
      url: this.sourceId,
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

  static async open(url: string, options: RemoteRangeDiskOptions): Promise<RemoteRangeDisk> {
    const sourceId = options.cacheKeyParts.imageId;
    const credentials = options.credentials ?? "same-origin";
    const lease = staticDiskLease(url, credentials);
    return await RemoteRangeDisk.openWithLease({ sourceId, lease }, options);
  }

  static async openWithLease(
    params: { sourceId: string; lease: DiskAccessLease },
    options: RemoteRangeDiskOptions,
  ): Promise<RemoteRangeDisk> {
    if (!params.sourceId) throw new Error("sourceId must not be empty");
    if (!params.lease.url) {
      await params.lease.refresh();
    }
    if (!params.lease.url) throw new Error("lease.url must not be empty");

    const chunkSize = options.chunkSize ?? RANGE_STREAM_CHUNK_SIZE;
    const maxConcurrentFetches = options.maxConcurrentFetches ?? 4;
    const maxRetries = options.maxRetries ?? 4;
    const readAheadChunks = options.readAheadChunks ?? 2;
    const retryBaseDelayMs = options.retryBaseDelayMs ?? 100;
    const leaseRefreshMarginMs = options.leaseRefreshMarginMs ?? DEFAULT_LEASE_REFRESH_MARGIN_MS;

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
    if (!Number.isInteger(leaseRefreshMarginMs) || leaseRefreshMarginMs < 0) {
      throw new Error(`invalid leaseRefreshMarginMs=${leaseRefreshMarginMs}`);
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

    const cacheId = await RemoteCacheManager.deriveCacheKey(options.cacheKeyParts);

    const metadataStore = options.metadataStore ?? new OpfsRemoteRangeDiskMetadataStore();
    const sparseCacheFactory = options.sparseCacheFactory ?? new OpfsRemoteRangeDiskSparseCacheFactory();

    const disk = new RemoteRangeDisk(
      params.sourceId,
      params.lease,
      resolvedOpts,
      leaseRefreshMarginMs,
      options.sha256Manifest,
      metadataStore,
      sparseCacheFactory,
      new Semaphore(maxConcurrentFetches),
      options.cacheKeyParts,
    );
    disk.cacheId = cacheId;
    try {
      await disk.init();
      disk.leaseRefresher.start();
    } catch (err) {
      // `init()` can fail after opening a persistent cache handle. Ensure we close it so we
      // don't leak SyncAccessHandles / file descriptors.
      await disk.close().catch(() => {});
      throw err;
    }
    return disk;
  }

  private async init(): Promise<void> {
    await this.maybeRefreshLease();
    const remote = await probeRemoteImage(this.lease, this.opts.fetchFn, { signal: this.fetchSignal });

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

    const now = Date.now();
    const etag = remote.etag ?? existingMeta?.validators.etag;
    const lastModified = remote.lastModified ?? existingMeta?.validators.lastModified;
    const metaToPersist: RemoteRangeDiskCacheMeta = {
      version: 1,
      imageId: this.cacheKeyParts.imageId,
      imageVersion: this.cacheKeyParts.version,
      deliveryType: this.cacheKeyParts.deliveryType,
      validators: {
        sizeBytes: remote.sizeBytes,
        ...(etag ? { etag } : {}),
        ...(lastModified ? { lastModified } : {}),
      },
      chunkSizeBytes: this.opts.chunkSize,
      createdAtMs: compatible && existingMeta ? existingMeta.createdAtMs : now,
      lastAccessedAtMs: now,
      cachedRanges: compatible && existingMeta ? existingMeta.cachedRanges : [],
    };
    // Normalize cached ranges so that `getCacheStatus()` reports compacted ranges even if an older
    // implementation wrote redundant spans.
    this.rangeSet = new RangeSet();
    for (const r of metaToPersist.cachedRanges) this.rangeSet.insert(r.start, r.end);
    metaToPersist.cachedRanges = this.rangeSet.getRanges();
    this.meta = metaToPersist;
    this.metaWriteChain = Promise.resolve();
    await this.metadataStore.write(this.cacheId, metaToPersist);

    // If the remote didn't expose ETag/Last-Modified, reuse whatever we had in metadata
    // so that we can still use If-Range across sessions.
    this.remoteEtag = etag;
    this.remoteLastModified = lastModified;
  }

  private isMetaCompatible(meta: RemoteRangeDiskCacheMeta, remote: RemoteProbe): boolean {
    if (!meta || meta.version !== 1) return false;
    if (meta.imageId !== this.cacheKeyParts.imageId) return false;
    if (meta.imageVersion !== this.cacheKeyParts.version) return false;
    if (meta.deliveryType !== this.cacheKeyParts.deliveryType) return false;
    if (meta.chunkSizeBytes !== this.opts.chunkSize) return false;
    if (meta.validators.sizeBytes !== remote.sizeBytes) return false;

    // Prefer ETag when the server exposes it; otherwise fall back to Last-Modified.
    if (remote.etag) {
      return meta.validators.etag ? validatorsMatch(meta.validators.etag, remote.etag) : false;
    }
    if (remote.lastModified) {
      return meta.validators.lastModified ? validatorsMatch(meta.validators.lastModified, remote.lastModified) : false;
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
    if (this.closed || !this.cache) {
      throw new Error("RemoteRangeDisk is closed");
    }
    return this.cache;
  }

  async readSectors(lba: number, buffer: Uint8Array): Promise<void> {
    if (this.closed) throw new Error("RemoteRangeDisk is closed");
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
    await this.flushPendingMetaPersist(this.cacheGeneration).catch(() => {
      // best-effort metadata persistence
    });
    await this.metaWriteChain.catch(() => {
      // best-effort metadata persistence
    });
    await this.ensureOpen().flush();
  }

  async clearCache(): Promise<void> {
    if (this.closed) throw new Error("RemoteRangeDisk is closed");
    if (this.invalidationPromise) {
      await this.invalidationPromise;
    }

    // Cancel outstanding downloads before we close/delete the cache backing file.
    // We'll re-create the controller at the end so future reads can proceed.
    this.fetchAbort.abort();

    const inflight = [...this.inflightChunks.values()].map((e) => e.promise);
    this.cacheGeneration += 1;
    this.lastReadEnd = null;
    this.resetTelemetry();

    if (this.flushTimer !== null) {
      clearTimeout(this.flushTimer);
      this.flushTimer = null;
    }
    this.flushPending = false;

    this.cancelPendingMetaPersist();
    await Promise.allSettled(inflight);
    this.inflightChunks.clear();

    const oldCache = this.cache;
    await oldCache?.close?.();
    this.cache = null;

    await this.metaWriteChain.catch(() => {
      // best-effort metadata persistence
    });
    this.metaWriteChain = Promise.resolve();
    this.meta = null;
    this.rangeSet = new RangeSet();

    await this.metadataStore.delete(this.cacheId);

    const cache = await this.sparseCacheFactory.create(this.cacheId, {
      diskSizeBytes: this.capacityBytesValue,
      blockSizeBytes: this.opts.chunkSize,
    });
    this.cache = cache;

    const now = Date.now();
    const metaToPersist: RemoteRangeDiskCacheMeta = {
      version: 1,
      imageId: this.cacheKeyParts.imageId,
      imageVersion: this.cacheKeyParts.version,
      deliveryType: this.cacheKeyParts.deliveryType,
      validators: {
        sizeBytes: this.capacityBytesValue,
        ...(this.remoteEtag ? { etag: this.remoteEtag } : {}),
        ...(this.remoteLastModified ? { lastModified: this.remoteLastModified } : {}),
      },
      chunkSizeBytes: this.opts.chunkSize,
      createdAtMs: now,
      lastAccessedAtMs: now,
      cachedRanges: [],
    };
    this.meta = metaToPersist;
    this.metaDirty = false;
    await this.metadataStore.write(this.cacheId, metaToPersist);

    // Allow subsequent reads after the clear completes.
    this.fetchAbort = new AbortController();
    this.fetchSignal = abortAny([this.abort.signal, this.fetchAbort.signal]);
  }

  async close(): Promise<void> {
    if (this.closed) return;
    this.closed = true;

    // Stop any background network activity and prevent further prefetches.
    this.abort.abort();
    this.fetchAbort.abort();

    // Stop refreshing the lease; once we're closed, no more network activity should occur.
    this.leaseRefresher.stop();

    if (this.flushTimer !== null) {
      clearTimeout(this.flushTimer);
      this.flushTimer = null;
    }
    this.flushPending = false;

    // If a cache invalidation is in progress, allow it to finish/settle, but never let it
    // prevent resource cleanup during close.
    await this.invalidationPromise?.catch(() => {});

    // Wait for any inflight chunk tasks to settle before touching the cache handle.
    await Promise.allSettled([...this.inflightChunks.values()].map((e) => e.promise));

    await this.flushPendingMetaPersist(this.cacheGeneration).catch(() => {
      // best-effort metadata persistence
    });
    await this.metaWriteChain.catch(() => {
      // best-effort metadata persistence
    });
    const cache = this.cache;
    this.cache = null;
    this.inflightChunks.clear();

    if (!cache) return;

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
    if (this.closed) throw new Error("RemoteRangeDisk is closed");
    assertNonNegativeSafeInteger(chunkIndex, "chunkIndex");
    if (this.invalidationPromise) {
      await this.invalidationPromise;
    }
    if (this.closed) throw new Error("RemoteRangeDisk is closed");
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
      if (this.closed) throw new Error("RemoteRangeDisk is closed");
      if (generation !== this.cacheGeneration) {
        // Cache was invalidated while we were waiting in the task queue.
        return await this.ensureChunkCached(chunkIndex);
      }

      const cache = this.ensureOpen();
      if (cache.isBlockAllocated(chunkIndex)) return;

      try {
        const start = performance.now();
        // Capture the signal for this operation so that a subsequent cache invalidation can
        // safely swap out `this.fetchAbort` without letting older generation tasks continue.
        const signal = this.fetchSignal;
        const bytes = await this.downloadChunkWithRetries(chunkIndex, generation, signal);
        if (this.closed) throw new Error("RemoteRangeDisk is closed");
        if (generation !== this.cacheGeneration) {
          // Cache invalidated after download; discard and let the caller retry.
          continue;
        }

        // Ensure no writes occur after `close()` or cache invalidation. These can race with
        // the above checks if the disk is closed (or generation bumped) between awaiting
        // the download and starting the write.
        if (this.closed) throw new Error("RemoteRangeDisk is closed");
        if (generation !== this.cacheGeneration) continue;

        const write = cache.writeBlock(chunkIndex, bytes);
        this.inflightWrites.add(write);
        try {
          await write;
        } finally {
          this.inflightWrites.delete(write);
        }
        if (generation === this.cacheGeneration) {
          this.lastFetchMs = performance.now() - start;
          this.lastFetchAtMs = Date.now();
        }
        this.recordCachedChunk(chunkIndex, generation);
        this.scheduleBackgroundFlush();
        return;
      } catch (err) {
        if (isAbortError(err) && generation !== this.cacheGeneration) {
          // An inflight fetch was aborted due to cache invalidation/clearCache; retry against the new cache generation.
          return await this.ensureChunkCached(chunkIndex);
        }
        if (err instanceof RemoteValidatorMismatchError && invalidations < 1) {
          invalidations += 1;
          await this.invalidateAndReopenCache();
          continue;
        }
        throw err;
      }
    }
  }

  private async downloadChunkWithRetries(
    chunkIndex: number,
    generation: number,
    signal: AbortSignal,
  ): Promise<Uint8Array> {
    let lastErr: unknown;
    for (let attempt = 0; attempt <= this.opts.maxRetries; attempt++) {
      const release = await this.fetchSemaphore.acquire();
      try {
        return await this.downloadChunkOnce(chunkIndex, generation, signal);
      } catch (err) {
        lastErr = err;
        if (attempt >= this.opts.maxRetries || !isRetryableError(err)) {
          throw err;
        }
        const delay = this.opts.retryBaseDelayMs * Math.pow(2, attempt);
        await sleep(delay, signal);
      } finally {
        release();
      }
    }
    throw lastErr instanceof Error ? lastErr : new Error(String(lastErr));
  }

  private async downloadChunkOnce(chunkIndex: number, generation: number, signal: AbortSignal): Promise<Uint8Array> {
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
    const expectedValidator = this.remoteEtag ?? this.remoteLastModified;
    const ifRangeValidator =
      this.remoteEtag && !isWeakEtag(this.remoteEtag) ? this.remoteEtag : this.remoteLastModified;
    if (ifRangeValidator) {
      headers["If-Range"] = ifRangeValidator;
    }

    if (generation === this.cacheGeneration) {
      this.telemetry.rangeRequests += 1;
      this.lastFetchRange = { start, end: endExclusive };
    }
    await this.maybeRefreshLease();
    const resp = await fetchWithDiskAccessLease(
      this.lease,
      { method: "GET", headers, signal },
      { fetch: this.opts.fetchFn, retryAuthOnce: true },
    );

    if (resp.status === 200 || resp.status === 412) {
      // Don't read the body — it could be a multi-GB full response.
      await cancelBody(resp);

      // Per RFC 7233, a server will return the full representation (200) when an If-Range
      // validator does not match. Some implementations use `412 Precondition Failed`.
      //
      // However, a server that does not support Range (or ignores the header) may also reply with
      // 200. Only treat 200 as a validator mismatch when the response provides a differing
      // validator.
      if (expectedValidator) {
        if (resp.status === 412) {
          throw new RemoteValidatorMismatchError(resp.status);
        }
        const actual = extractValidatorFromHeaders(resp.headers);
        if (actual && !validatorsMatch(expectedValidator, actual)) {
          throw new RemoteValidatorMismatchError(resp.status);
        }
      }
      throw new Error(`remote server ignored Range request (expected 206, got ${resp.status})`);
    }

    if (resp.status === 416) {
      // A 416 indicates that our requested range is not satisfiable. This can happen when the
      // representation has changed (often size drift), while our cache metadata/validators still
      // point at the previous size. Many servers include `Content-Range: bytes */<total>` for 416,
      // which we treat as a strong signal that our cached size is wrong.
      await cancelBody(resp);
      const contentRange = resp.headers.get("content-range");
      if (contentRange) {
        // Best-effort parse; if it's malformed we still treat this as a mismatch event so the
        // invalidation loop can re-probe the remote.
        try {
          parseUnsatisfiedContentRangeHeader(contentRange);
        } catch {
          // ignore parse errors
        }
      }
      throw new RemoteValidatorMismatchError(resp.status);
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

    // Servers that don't implement If-Range may still return 206 after the representation has
    // changed. When the response exposes a validator (ETag / Last-Modified), detect mismatches to
    // avoid mixing bytes from different versions under one cache identity.
    if (expectedValidator) {
      const actual = extractValidatorFromHeaders(resp.headers);
      if (actual && !validatorsMatch(expectedValidator, actual)) {
        await cancelBody(resp);
        throw new RemoteValidatorMismatchError(206);
      }
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

  private async maybeRefreshLease(): Promise<void> {
    const expiresAt = this.lease.expiresAt;
    if (!expiresAt) return;
    const refreshAtMs = expiresAt.getTime() - this.leaseRefreshMarginMs;
    if (!Number.isFinite(refreshAtMs) || Date.now() < refreshAtMs) return;
    await this.lease.refresh();
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
    (this.flushTimer as unknown as { unref?: () => void }).unref?.();
  }

  private scheduleMetaPersist(generation: number): void {
    if (!this.metaDirty) return;
    // Debounce: keep pushing out the persist timer until writes settle. This avoids emitting
    // a metadata write per cached chunk during large sequential reads.
    if (this.metaPersistTimer !== null) {
      clearTimeout(this.metaPersistTimer);
      this.metaPersistTimer = null;
    }

    this.metaPersistTimer = setTimeout(() => {
      this.metaPersistTimer = null;
      if (!this.metaDirty) return;
      if (generation !== this.cacheGeneration) return;
      this.metaDirty = false;
      void this.persistMeta(generation).catch(() => {
        // best-effort metadata persistence
      });
    }, META_PERSIST_DEBOUNCE_MS);
    (this.metaPersistTimer as unknown as { unref?: () => void }).unref?.();
  }

  private cancelPendingMetaPersist(): void {
    if (this.metaPersistTimer !== null) {
      clearTimeout(this.metaPersistTimer);
      this.metaPersistTimer = null;
    }
    this.metaDirty = false;
  }

  private async flushPendingMetaPersist(generation: number): Promise<void> {
    if (this.metaPersistTimer !== null) {
      clearTimeout(this.metaPersistTimer);
      this.metaPersistTimer = null;
    }
    if (!this.metaDirty) return;
    this.metaDirty = false;
    await this.persistMeta(generation);
  }

  private recordCachedChunk(chunkIndex: number, generation: number): void {
    const meta = this.meta;
    if (!meta) return;
    if (generation !== this.cacheGeneration) return;

    const start = chunkIndex * this.opts.chunkSize;
    const end = Math.min(start + this.opts.chunkSize, this.capacityBytesValue);
    if (end <= start) return;

    this.rangeSet.insert(start, end);
    meta.cachedRanges = this.rangeSet.getRanges();
    meta.lastAccessedAtMs = Date.now();
    this.metaDirty = true;
    this.scheduleMetaPersist(generation);
  }

  private async persistMeta(generation: number): Promise<void> {
    const meta = this.meta;
    if (!meta) return;

    // Multiple chunk fetches can complete concurrently; serialize meta writes so that older snapshots
    // don't race and overwrite newer metadata.
    this.metaWriteChain = this.metaWriteChain
      .catch(() => {
        // Keep the chain alive even if a previous write failed.
      })
      .then(async () => {
        if (generation !== this.cacheGeneration) return;
        await this.metadataStore.write(this.cacheId, meta);
      });
    await this.metaWriteChain;
  }

  private async invalidateAndReopenCache(): Promise<void> {
    if (this.closed) throw new Error("RemoteRangeDisk is closed");
    if (this.invalidationPromise) return await this.invalidationPromise;

    this.invalidationPromise = (async () => {
      this.cacheGeneration += 1;

      // Cancel inflight downloads for the previous cache generation and prepare a fresh controller
      // for subsequent reads.
      this.fetchAbort.abort();
      this.fetchAbort = new AbortController();
      this.fetchSignal = abortAny([this.abort.signal, this.fetchAbort.signal]);

      if (this.flushTimer !== null) {
        clearTimeout(this.flushTimer);
        this.flushTimer = null;
      }
      this.flushPending = false;

      this.cancelPendingMetaPersist();

      // Ensure no cache writes are in-flight before closing the underlying file handle.
      await Promise.allSettled([...this.inflightWrites]);

      const oldCache = this.cache;
      await oldCache?.close?.();
      this.cache = null;

      await this.metaWriteChain.catch(() => {
        // best-effort: ensure no metadata write is in-flight before removing the cache directory
      });
      this.metaWriteChain = Promise.resolve();
      this.meta = null;
      this.rangeSet = new RangeSet();

      await this.metadataStore.delete(this.cacheId);

      await this.maybeRefreshLease();
      const remote = await probeRemoteImage(this.lease, this.opts.fetchFn, { signal: this.fetchSignal });
      this.capacityBytesValue = remote.sizeBytes;
      this.remoteEtag = remote.etag;
      this.remoteLastModified = remote.lastModified;

      const cache = await this.sparseCacheFactory.create(this.cacheId, {
        diskSizeBytes: remote.sizeBytes,
        blockSizeBytes: this.opts.chunkSize,
      });
      this.cache = cache;

      const now = Date.now();
      const metaToPersist: RemoteRangeDiskCacheMeta = {
        version: 1,
        imageId: this.cacheKeyParts.imageId,
        imageVersion: this.cacheKeyParts.version,
        deliveryType: this.cacheKeyParts.deliveryType,
        validators: {
          sizeBytes: remote.sizeBytes,
          ...(remote.etag ? { etag: remote.etag } : {}),
          ...(remote.lastModified ? { lastModified: remote.lastModified } : {}),
        },
        chunkSizeBytes: this.opts.chunkSize,
        createdAtMs: now,
        lastAccessedAtMs: now,
        cachedRanges: [],
      };
      this.meta = metaToPersist;
      this.metaDirty = false;
      await this.metadataStore.write(this.cacheId, metaToPersist);
    })();

    try {
      await this.invalidationPromise;
    } finally {
      this.invalidationPromise = null;
    }
  }
}
