import { RangeSet, type ByteRange, type RemoteDiskTelemetrySnapshot } from "../platform/remote_disk";
import { assertSectorAligned, checkedOffset, SECTOR_SIZE, type AsyncSectorDisk } from "./disk";
import { RANGE_STREAM_CHUNK_SIZE } from "./chunk_sizes";
import { opfsGetRemoteCacheDir } from "./metadata";
import { MemorySparseDisk } from "./memory_sparse_disk";
import { OpfsAeroSparseDisk } from "./opfs_sparse";
import {
  DEFAULT_LEASE_REFRESH_MARGIN_MS,
  DiskAccessLeaseRefresher,
  fetchWithDiskAccessLease,
  type DiskAccessLease,
} from "./disk_access_lease";
import type { RemoteDiskBaseSnapshot } from "./runtime_disk_snapshot";
import { RemoteCacheManager, type RemoteCacheKeyParts, type RemoteCacheMetaV1 } from "./remote_cache_manager";
import { readResponseBytesWithLimit, ResponseTooLargeError } from "./response_json";

// Keep in sync with the Rust snapshot bounds where sensible.
const MAX_REMOTE_CHUNK_SIZE_BYTES = 64 * 1024 * 1024; // 64 MiB
// Defensive bounds for user-provided tuning knobs. These values can come from untrusted snapshot
// metadata or external configuration, so keep them bounded to avoid pathological background work
// (e.g. hundreds of concurrent 64MiB fetches).
const MAX_REMOTE_READ_AHEAD_CHUNKS = 1024;
const MAX_REMOTE_READ_AHEAD_BYTES = 512 * 1024 * 1024; // 512 MiB
const MAX_REMOTE_MAX_RETRIES = 32;
const MAX_REMOTE_MAX_CONCURRENT_FETCHES = 128;
const MAX_REMOTE_INFLIGHT_BYTES = 512 * 1024 * 1024; // 512 MiB
const MAX_REMOTE_SHA256_MANIFEST_ENTRIES = 1_000_000;
const SHA256_HEX_RE = /^[0-9a-f]{64}$/;

function hasOwn(obj: object, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(obj, key);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function nullProto<T extends object>(): T {
  return Object.create(null) as T;
}

function nullProtoCopy<T extends object>(value: unknown): T {
  if (!isRecord(value)) return nullProto<T>();
  // Copy only own enumerable properties into a null-prototype object so callers never observe
  // inherited fields (prototype pollution).
  return Object.assign(nullProto<T>(), value as object);
}

function requireRemoteCacheKeyParts(raw: unknown): RemoteCacheKeyParts {
  if (!isRecord(raw)) {
    throw new Error("cacheKeyParts must be an object");
  }
  const rec = raw as Record<string, unknown>;
  const imageId = hasOwn(rec, "imageId") ? rec.imageId : undefined;
  const version = hasOwn(rec, "version") ? rec.version : undefined;
  const deliveryType = hasOwn(rec, "deliveryType") ? rec.deliveryType : undefined;
  if (typeof imageId !== "string" || !imageId.trim()) throw new Error("cacheKeyParts.imageId must not be empty");
  if (typeof version !== "string" || !version.trim()) throw new Error("cacheKeyParts.version must not be empty");
  if (typeof deliveryType !== "string" || !deliveryType.trim()) throw new Error("cacheKeyParts.deliveryType must not be empty");
  const out = Object.create(null) as RemoteCacheKeyParts;
  out.imageId = imageId;
  out.version = version;
  out.deliveryType = deliveryType;
  return out;
}

/**
 * Errors from the metadata store / sparse cache backend are wrapped so that callers can fall back
 * to an ephemeral in-memory cache when OPFS is unavailable (e.g. Node, older browsers, or
 * environments without SyncAccessHandle support).
 *
 * Note: This intentionally does *not* wrap network/probe errors so that callers still see the
 * underlying fetch failure without an unrelated "cache init" wrapper.
 */
class RemoteRangeDiskCacheBackendInitError extends Error {
  constructor(readonly cause: unknown) {
    super("RemoteRangeDisk cache backend init failed");
    this.name = "RemoteRangeDiskCacheBackendInitError";
  }
}

// Process-local fallback cache for runtimes without OPFS.
const memoryFallbackMeta = new Map<string, RemoteRangeDiskCacheMeta>();
const memoryFallbackCaches = new Map<string, MemorySparseDisk>();

class MemoryRemoteRangeDiskMetadataStore implements RemoteRangeDiskMetadataStore {
  async read(cacheId: string): Promise<RemoteRangeDiskCacheMeta | null> {
    return memoryFallbackMeta.get(cacheId) ?? null;
  }

  async write(cacheId: string, meta: RemoteRangeDiskCacheMeta): Promise<void> {
    memoryFallbackMeta.set(cacheId, meta);
  }

  async delete(cacheId: string): Promise<void> {
    memoryFallbackMeta.delete(cacheId);
    memoryFallbackCaches.delete(cacheId);
  }
}

class MemoryRemoteRangeDiskSparseCacheFactory implements RemoteRangeDiskSparseCacheFactory {
  async open(cacheId: string): Promise<RemoteRangeDiskSparseCache> {
    const existing = memoryFallbackCaches.get(cacheId);
    if (!existing) throw new Error("cache not found");
    return existing;
  }

  async create(cacheId: string, opts: { diskSizeBytes: number; blockSizeBytes: number }): Promise<RemoteRangeDiskSparseCache> {
    const disk = MemorySparseDisk.create({ diskSizeBytes: opts.diskSizeBytes, blockSizeBytes: opts.blockSizeBytes });
    memoryFallbackCaches.set(cacheId, disk);
    return disk;
  }

  async delete(cacheId: string): Promise<void> {
    memoryFallbackCaches.delete(cacheId);
  }
}

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

// Throttle for updating `meta.lastAccessedAtMs` on cache hits.
//
// RemoteRangeDisk can serve very high read rates from the local sparse cache; persisting a metadata
// write on every read would amplify OPFS writes. Minute-level granularity is sufficient for cache
// pruning heuristics while keeping metadata fresh for long-lived sessions.
const META_TOUCH_THROTTLE_MS = 60_000;

/**
 * Remote Range disk sparse cache interface.
 *
 * This is a specialized extension of `AsyncSectorDisk` that exposes sparse-block operations for
 * the remote Range streaming cache implementation.
 *
 * Canonical trait note:
 * Prefer taking `AsyncSectorDisk` at API boundaries unless the caller explicitly needs sparse
 * allocation semantics.
 *
 * See `docs/20-storage-trait-consolidation.md`.
 */
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

/**
 * Factory interface for opening/creating a `RemoteRangeDiskSparseCache`.
 *
 * Canonical trait note:
 * Prefer taking `AsyncSectorDisk` at most boundaries; this factory is intentionally specialized
 * to the remote range disk caching implementation.
 *
 * See `docs/20-storage-trait-consolidation.md`.
 */
export interface RemoteRangeDiskSparseCacheFactory {
  open(cacheId: string): Promise<RemoteRangeDiskSparseCache>;
  create(cacheId: string, opts: { diskSizeBytes: number; blockSizeBytes: number }): Promise<RemoteRangeDiskSparseCache>;
  delete?(cacheId: string): Promise<void>;
}

/**
 * Persisted metadata store used by the remote range disk cache implementation.
 *
 * Canonical trait note:
 * This is *not* a general disk backend interface; it exists to keep the remote range disk cache
 * implementation testable and decoupled from OPFS/IndexedDB details.
 *
 * See `docs/20-storage-trait-consolidation.md`.
 */
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

class ProtocolError extends Error {
  override name = "ProtocolError";
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

function isQuotaExceededError(err: unknown): boolean {
  // Browser/file system quota failures typically surface as a DOMException named
  // "QuotaExceededError". Firefox uses a different name for the same condition.
  if (!err) return false;
  const isDomException = typeof DOMException !== "undefined" && err instanceof DOMException;
  const name =
    isDomException || err instanceof Error
      ? err.name
      : typeof err === "object" && "name" in err
        ? ((err as { name?: unknown }).name as unknown)
        : undefined;
  return name === "QuotaExceededError" || name === "NS_ERROR_DOM_QUOTA_REACHED";
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
  const onAbort = () => {
    controller.abort();
    // Remove any remaining listeners. Without this, a short-lived signal (e.g. per-generation abort)
    // aborting would leave the listener attached to the long-lived signal (e.g. disk close abort),
    // which can accumulate if we frequently replace the short-lived signal (clearCache/invalidate).
    for (const s of signals) {
      try {
        s.removeEventListener("abort", onAbort);
      } catch {
        // ignore
      }
    }
  };
  for (const s of signals) {
    if (s.aborted) {
      onAbort();
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

function divFloor(n: number, d: number): number {
  if (!Number.isSafeInteger(n) || !Number.isSafeInteger(d) || d <= 0 || n < 0) {
    throw new Error("divFloor: arguments must be safe non-negative integers and divisor must be > 0");
  }
  const out = Number(BigInt(n) / BigInt(d));
  if (!Number.isSafeInteger(out)) {
    throw new Error("divFloor overflow");
  }
  return out;
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

function assertIdentityContentEncoding(headers: Headers, label: string): void {
  // Byte-addressed disk streaming requires `Content-Encoding` to be identity or absent. If the
  // server applies compression, `Range` offsets apply to the encoded representation and the
  // browser may transparently decode, breaking deterministic byte reads.
  const raw = headers.get("content-encoding");
  if (!raw) return;
  const normalized = raw.trim().toLowerCase();
  if (!normalized || normalized === "identity") return;
  throw new ProtocolError(`${label} unexpected Content-Encoding: ${raw}`);
}

function assertNoTransformCacheControl(headers: Headers, label: string): void {
  // Disk streaming reads bytes by offset. Any intermediary transform (compression, format change,
  // etc) can break byte-addressed semantics, especially when combined with HTTP Range.
  //
  // Cache-Control is CORS-safelisted, so this is readable in browsers even for cross-origin
  // requests without explicit `Access-Control-Expose-Headers`.
  const raw = headers.get("cache-control");
  if (!raw) {
    throw new ProtocolError(`${label} missing Cache-Control header (expected include 'no-transform')`);
  }
  const tokens = raw
    .split(",")
    .map((t) => t.trim().toLowerCase())
    .filter((t) => t.length > 0);
  if (!tokens.includes("no-transform")) {
    throw new ProtocolError(`${label} Cache-Control missing no-transform: ${raw}`);
  }
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
  if (err instanceof ProtocolError) return false;
  if (err instanceof ResponseTooLargeError) return false;
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
    try {
      assertIdentityContentEncoding(probe.headers, "range probe");
      assertNoTransformCacheControl(probe.headers, "range probe");
    } catch (err) {
      await cancelBody(probe);
      throw err;
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
    const body = await readResponseBytesWithLimit(probe, { maxBytes: 1, label: "range probe body" });
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
  private lastMetaTouchAtMs = 0;
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

  private activeReads = 0;

  private flushTimer: ReturnType<typeof setTimeout> | null = null;
  private flushPending = false;
  private readonly leaseRefresher: DiskAccessLeaseRefresher;
  private closed = false;
  private readonly abort = new AbortController();
  private fetchAbort = new AbortController();
  private fetchSignal = abortAny([this.abort.signal, this.fetchAbort.signal]);

  /**
   * When true, the disk will not attempt any further persistent cache writes (e.g. OPFS sparse file
   * growth). This is set when we observe a quota failure while persisting a downloaded chunk.
   *
   * Reads should continue to succeed via network + in-memory blocks.
   */
  private persistentCacheWritesDisabled = false;
  private readonly inMemoryChunks = new Map<number, Uint8Array>();

  private disablePersistentCacheWrites(): void {
    if (this.persistentCacheWritesDisabled) return;
    this.persistentCacheWritesDisabled = true;

    // Stop any queued background persistence work. Once we observe quota pressure, we treat the
    // persistent cache as best-effort and avoid further writes for the remainder of the disk
    // lifetime.
    if (this.flushTimer !== null) {
      clearTimeout(this.flushTimer);
      this.flushTimer = null;
    }
    this.flushPending = false;

    // Prevent any pending metadata writes from firing after caching is disabled.
    this.cancelPendingMetaPersist();
  }

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
    let cachedBytes = 0;
    // The sparse cache stores fixed-size blocks, so its "allocated bytes" can exceed the remote
    // image size when the final block is partial. Convert back to remote bytes so telemetry is
    // consistent with other remote disk implementations.
    //
    // Additionally, quota failures while growing the sparse file can leave the persistent cache in
    // a partially-updated state. Telemetry should be best-effort: never let cache bookkeeping throw
    // (and when persistence is disabled, always report cachedBytes=0).
    if (cache && !this.persistentCacheWritesDisabled) {
      try {
        cachedBytes = cache.getAllocatedBytes();
        const remainder = totalSize % blockSize;
        if (remainder !== 0 && totalSize > 0) {
          const lastBlockIndex = divFloor(totalSize - 1, blockSize);
          // If the cache is corrupt, treat this adjustment as best-effort.
          try {
            if (cache.isBlockAllocated(lastBlockIndex)) {
              cachedBytes -= blockSize - remainder;
            }
          } catch {
            // ignore
          }
        }
        if (cachedBytes < 0) cachedBytes = 0;
        if (cachedBytes > totalSize) cachedBytes = totalSize;
      } catch {
        cachedBytes = 0;
      }
    }
    return {
      url: this.sourceId,
      totalSize,
      blockSize,
      cacheLimitBytes: this.persistentCacheWritesDisabled ? 0 : null,
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
    const safeOptions = nullProtoCopy<RemoteRangeDiskOptions>(options);
    safeOptions.cacheKeyParts = requireRemoteCacheKeyParts(safeOptions.cacheKeyParts);
    const sourceId = safeOptions.cacheKeyParts.imageId;
    const credentialsRaw = safeOptions.credentials;
    const credentials = credentialsRaw === undefined ? "same-origin" : credentialsRaw;
    if (credentials !== "same-origin" && credentials !== "include" && credentials !== "omit") {
      throw new Error(`invalid credentials=${String(credentialsRaw)}`);
    }
    const lease = staticDiskLease(url, credentials);
    return await RemoteRangeDisk.openWithLease({ sourceId, lease }, safeOptions);
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

    const safeOptions = nullProtoCopy<RemoteRangeDiskOptions>(options);
    const cacheKeyParts = requireRemoteCacheKeyParts(safeOptions.cacheKeyParts);
    safeOptions.cacheKeyParts = cacheKeyParts;

    const chunkSize = safeOptions.chunkSize ?? RANGE_STREAM_CHUNK_SIZE;
    const maxConcurrentFetches = safeOptions.maxConcurrentFetches ?? 4;
    const maxRetries = safeOptions.maxRetries ?? 4;
    const readAheadChunks = safeOptions.readAheadChunks ?? 2;
    const retryBaseDelayMs = safeOptions.retryBaseDelayMs ?? 100;
    const leaseRefreshMarginMs = safeOptions.leaseRefreshMarginMs ?? DEFAULT_LEASE_REFRESH_MARGIN_MS;

    assertValidChunkSize(chunkSize);
    if (!Number.isInteger(maxConcurrentFetches) || maxConcurrentFetches <= 0) {
      throw new Error(`invalid maxConcurrentFetches=${maxConcurrentFetches}`);
    }
    if (maxConcurrentFetches > MAX_REMOTE_MAX_CONCURRENT_FETCHES) {
      throw new Error(
        `maxConcurrentFetches too large: max=${MAX_REMOTE_MAX_CONCURRENT_FETCHES} got=${maxConcurrentFetches}`,
      );
    }
    if (!Number.isInteger(maxRetries) || maxRetries < 0) {
      throw new Error(`invalid maxRetries=${maxRetries}`);
    }
    if (maxRetries > MAX_REMOTE_MAX_RETRIES) {
      throw new Error(`maxRetries too large: max=${MAX_REMOTE_MAX_RETRIES} got=${maxRetries}`);
    }
    if (!Number.isInteger(readAheadChunks) || readAheadChunks < 0) {
      throw new Error(`invalid readAheadChunks=${readAheadChunks}`);
    }
    if (readAheadChunks > MAX_REMOTE_READ_AHEAD_CHUNKS) {
      throw new Error(`readAheadChunks too large: max=${MAX_REMOTE_READ_AHEAD_CHUNKS} got=${readAheadChunks}`);
    }
    if (!Number.isInteger(retryBaseDelayMs) || retryBaseDelayMs <= 0) {
      throw new Error(`invalid retryBaseDelayMs=${retryBaseDelayMs}`);
    }
    if (!Number.isInteger(leaseRefreshMarginMs) || leaseRefreshMarginMs < 0) {
      throw new Error(`invalid leaseRefreshMarginMs=${leaseRefreshMarginMs}`);
    }

    // Keep sequential prefetch bounded (best-effort). Compute with BigInt to avoid overflow /
    // precision loss near `Number.MAX_SAFE_INTEGER`.
    const readAheadBytes = BigInt(readAheadChunks) * BigInt(chunkSize);
    if (readAheadBytes > BigInt(MAX_REMOTE_READ_AHEAD_BYTES)) {
      throw new Error(
        `readAhead bytes too large: max=${MAX_REMOTE_READ_AHEAD_BYTES} got=${readAheadBytes.toString()}`,
      );
    }
    const inflightBytes = BigInt(maxConcurrentFetches) * BigInt(chunkSize);
    if (inflightBytes > BigInt(MAX_REMOTE_INFLIGHT_BYTES)) {
      throw new Error(`inflight bytes too large: max=${MAX_REMOTE_INFLIGHT_BYTES} got=${inflightBytes.toString()}`);
    }

    const fetchFn = safeOptions.fetchFn ?? fetch;
    const resolvedOpts: ResolvedRemoteRangeDiskOptions = {
      chunkSize,
      maxConcurrentFetches,
      maxRetries,
      readAheadChunks,
      retryBaseDelayMs,
      fetchFn,
    };

    const cacheId = await RemoteCacheManager.deriveCacheKey(cacheKeyParts);

    const usedDefaultMetadataStore = safeOptions.metadataStore === undefined;
    const usedDefaultSparseCacheFactory = safeOptions.sparseCacheFactory === undefined;

    const makeDisk = (metadataStore: RemoteRangeDiskMetadataStore, sparseCacheFactory: RemoteRangeDiskSparseCacheFactory) => {
      const disk = new RemoteRangeDisk(
        params.sourceId,
        params.lease,
        resolvedOpts,
        leaseRefreshMarginMs,
        safeOptions.sha256Manifest,
        metadataStore,
        sparseCacheFactory,
        new Semaphore(maxConcurrentFetches),
        cacheKeyParts,
      );
      disk.cacheId = cacheId;
      return disk;
    };

    const openOnce = async (
      metadataStore: RemoteRangeDiskMetadataStore,
      sparseCacheFactory: RemoteRangeDiskSparseCacheFactory,
    ): Promise<RemoteRangeDisk> => {
      const disk = makeDisk(metadataStore, sparseCacheFactory);
      try {
        await disk.init();
        disk.leaseRefresher.start();
        return disk;
      } catch (err) {
        // `init()` can fail after opening a persistent cache handle. Ensure we close it so we
        // don't leak SyncAccessHandles / file descriptors.
        await disk.close().catch(() => {});
        throw err;
      }
    };

    try {
      return await openOnce(
        safeOptions.metadataStore ?? new OpfsRemoteRangeDiskMetadataStore(),
        safeOptions.sparseCacheFactory ?? new OpfsRemoteRangeDiskSparseCacheFactory(),
      );
    } catch (err) {
      // Only fall back when the caller opted into the defaults (OPFS-backed cache) and the
      // failure came from initializing/using the cache backend.
      const allowFallback = usedDefaultMetadataStore && usedDefaultSparseCacheFactory;
      if (allowFallback && err instanceof RemoteRangeDiskCacheBackendInitError) {
        return await openOnce(new MemoryRemoteRangeDiskMetadataStore(), new MemoryRemoteRangeDiskSparseCacheFactory());
      }
      // Preserve previous error behaviour: callers should see the original backend error, not
      // the wrapper.
      if (err instanceof RemoteRangeDiskCacheBackendInitError) {
        throw err.cause instanceof Error ? err.cause : err;
      }
      throw err;
    }
  }

  private async init(): Promise<void> {
    await this.maybeRefreshLease();
    const remote = await probeRemoteImage(this.lease, this.opts.fetchFn, { signal: this.fetchSignal });

    this.capacityBytesValue = remote.sizeBytes;
    this.remoteEtag = remote.etag;
    this.remoteLastModified = remote.lastModified;

    if (this.sha256Manifest) {
      if (this.sha256Manifest.length > MAX_REMOTE_SHA256_MANIFEST_ENTRIES) {
        throw new Error(
          `sha256Manifest too large: max=${MAX_REMOTE_SHA256_MANIFEST_ENTRIES} got=${this.sha256Manifest.length}`,
        );
      }
      const expectedChunks = divFloor(remote.sizeBytes - 1, this.opts.chunkSize) + 1;
      if (this.sha256Manifest.length !== expectedChunks) {
        throw new Error(
          `sha256Manifest length mismatch: expected=${expectedChunks} actual=${this.sha256Manifest.length}`,
        );
      }
      for (let i = 0; i < this.sha256Manifest.length; i++) {
        const entry = this.sha256Manifest[i];
        if (typeof entry !== "string") {
          throw new Error("sha256Manifest entries must be 64-char hex digests");
        }
        const normalized = entry.trim().toLowerCase();
        if (!SHA256_HEX_RE.test(normalized)) {
          throw new Error("sha256Manifest entries must be 64-char hex digests");
        }
        this.sha256Manifest[i] = normalized;
      }
    }

    let existingMeta: RemoteRangeDiskCacheMeta | null = null;
    let compatible = false;
    try {
      existingMeta = await this.metadataStore.read(this.cacheId);
      compatible = existingMeta ? this.isMetaCompatible(existingMeta, remote) : false;

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
    } catch (err) {
      throw new RemoteRangeDiskCacheBackendInitError(err);
    }
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
    this.activeReads += 1;
    try {
    if (this.closed) throw new Error("RemoteRangeDisk is closed");
    if (this.invalidationPromise) {
      await this.invalidationPromise;
    }
    const startedWithPersistentCacheDisabled = this.persistentCacheWritesDisabled;
    const generation = this.cacheGeneration;
    this.ensureOpen();
    assertSectorAligned(buffer.byteLength, this.sectorSize);
    const offset = checkedOffset(lba, buffer.byteLength, this.sectorSize);
    if (offset + buffer.byteLength > this.capacityBytesValue) {
      throw new Error("read past end of disk");
    }

    if (buffer.byteLength === 0) {
      this.lastReadEnd = offset;
      return;
    }

    const startChunk = divFloor(offset, this.opts.chunkSize);
    const endChunk = divFloor(offset + buffer.byteLength - 1, this.opts.chunkSize);

    // Avoid allocating a promise per spanned chunk: large reads (or buggy guests) can request
    // extremely long ranges. Instead, cache a bounded window of chunks and advance as tasks
    // complete, similar to `RemoteChunkedDisk.readSectors`.
    const inflight = new Map<number, Promise<void>>();
    let nextChunk = startChunk;
    const maxInflight = this.opts.maxConcurrentFetches;

    const launch = (chunkIndex: number): void => {
      const task = this.ensureChunkCached(chunkIndex).finally(() => {
        inflight.delete(chunkIndex);
      });
      inflight.set(chunkIndex, task);
    };

    while (nextChunk <= endChunk && inflight.size < maxInflight) {
      launch(nextChunk);
      nextChunk += 1;
    }

    while (inflight.size > 0) {
      await Promise.race(inflight.values());
      if (generation !== this.cacheGeneration) {
        // Cache was invalidated while awaiting downloads; stop launching new chunk fetches.
        // We'll drain the existing inflight work, then restart the read against the new cache.
        continue;
      }
      while (nextChunk <= endChunk && inflight.size < maxInflight) {
        launch(nextChunk);
        nextChunk += 1;
      }
    }

    if (generation !== this.cacheGeneration) {
      // Cache was invalidated while awaiting downloads; restart the read against the new cache.
      return await this.readSectors(lba, buffer);
    }

    // If quota pressure disabled persistence during this read, restart in "memory/network" mode to
    // avoid relying on a potentially partially-written persistent sparse file.
    if (this.persistentCacheWritesDisabled && !startedWithPersistentCacheDisabled) {
      return await this.readSectors(lba, buffer);
    }

    if (this.persistentCacheWritesDisabled) {
      // Fill directly from in-memory chunks. `ensureChunkCached()` is responsible for downloading
      // any missing chunks once persistence is disabled.
      const readStart = offset;
      const readEnd = offset + buffer.byteLength;
      for (let chunkIndex = startChunk; chunkIndex <= endChunk; chunkIndex += 1) {
        const bytes = this.inMemoryChunks.get(chunkIndex);
        if (!bytes) {
          throw new Error(`missing in-memory chunk ${chunkIndex} after quota-disable read`);
        }
        const chunkStart = chunkIndex * this.opts.chunkSize;
        const chunkEnd = chunkStart + bytes.byteLength;
        const copyStart = Math.max(readStart, chunkStart);
        const copyEnd = Math.min(readEnd, chunkEnd);
        if (copyEnd <= copyStart) continue;
        const srcStart = copyStart - chunkStart;
        const dstStart = copyStart - readStart;
        const len = copyEnd - copyStart;
        buffer.set(bytes.subarray(srcStart, srcStart + len), dstStart);
      }
    } else {
      try {
        await this.ensureOpen().readSectors(lba, buffer);
      } catch (err) {
        if (isQuotaExceededError(err)) {
          // Some sparse cache implementations can surface quota errors even on read paths (e.g.
          // eviction writes dirty blocks). Reads must remain correct: disable persistence and retry
          // via network + in-memory chunks.
          this.disablePersistentCacheWrites();
          return await this.readSectors(lba, buffer);
        }
        throw err;
      }
    }
    this.touchMetaAfterRead(generation);
    this.scheduleReadAhead(offset, buffer.byteLength, endChunk);
    } finally {
      this.activeReads -= 1;
      if (this.activeReads < 0) this.activeReads = 0;
      // Once persistence is disabled, cached bytes are kept only for the duration of an active read
      // (to satisfy multi-chunk reads + safe restarts). Do not retain an unbounded in-memory cache
      // across the disk lifetime.
      if (this.activeReads === 0 && this.persistentCacheWritesDisabled) {
        this.inMemoryChunks.clear();
      }
    }
  }

  async writeSectors(_lba: number, _data: Uint8Array): Promise<void> {
    throw new Error("RemoteRangeDisk is read-only");
  }

  async flush(): Promise<void> {
    if (this.invalidationPromise) {
      await this.invalidationPromise;
    }
    if (this.persistentCacheWritesDisabled) {
      // Persistent caching is disabled (quota pressure). Flushing is a no-op so callers don't
      // trigger additional best-effort persistence work.
      return;
    }
    await this.flushPendingMetaPersist(this.cacheGeneration).catch(() => {
      // best-effort metadata persistence
    });
    await this.metaWriteChain.catch(() => {
      // best-effort metadata persistence
    });
    if (this.persistentCacheWritesDisabled) {
      // Metadata persistence can observe quota pressure and disable caching. Avoid flushing the
      // sparse cache after we've already decided to stop persistent writes.
      return;
    }
    try {
      await this.ensureOpen().flush();
    } catch (err) {
      if (isQuotaExceededError(err)) {
        this.disablePersistentCacheWrites();
        return;
      }
      throw err;
    }
  }

  async clearCache(): Promise<void> {
    if (this.closed) throw new Error("RemoteRangeDisk is closed");
    if (this.invalidationPromise) {
      await this.invalidationPromise;
    }

    if (this.persistentCacheWritesDisabled) {
      // Persistent caching is disabled (quota pressure). Avoid any further persistent writes, but
      // still drop the on-disk cache best-effort so that callers can free up storage.
      this.fetchAbort.abort();

      const inflight = [...this.inflightChunks.values()].map((e) => e.promise);
      this.cacheGeneration += 1;
      this.lastReadEnd = null;
      this.inMemoryChunks.clear();
      this.resetTelemetry();

      if (this.flushTimer !== null) {
        clearTimeout(this.flushTimer);
        this.flushTimer = null;
      }
      this.flushPending = false;

      this.cancelPendingMetaPersist();
      await Promise.allSettled(inflight);
      this.inflightChunks.clear();

      // Best-effort: allow any metadata write in-flight to settle before deleting the cache dir.
      await this.metaWriteChain.catch(() => {});
      this.metaWriteChain = Promise.resolve();
      this.meta = null;
      this.rangeSet = new RangeSet();

      // Close the persistent cache handle so OPFS can remove the backing file.
      const oldCache = this.cache;
      await oldCache?.close?.().catch(() => {});

      // Best-effort delete: if this fails, reads still work in memory/network mode.
      await this.metadataStore.delete(this.cacheId).catch(() => {});

      // Keep a non-null cache handle so `ensureOpen()` keeps working. This in-memory disk is not
      // used for reads (we read from `inMemoryChunks` once persistence is disabled), but avoids
      // special-casing `ensureOpen()` throughout the implementation.
      this.cache = MemorySparseDisk.create({ diskSizeBytes: this.capacityBytesValue, blockSizeBytes: this.opts.chunkSize });

      // Allow subsequent reads after the clear completes.
      this.fetchAbort = new AbortController();
      this.fetchSignal = abortAny([this.abort.signal, this.fetchAbort.signal]);
      return;
    }

    // Cancel outstanding downloads before we close/delete the cache backing file.
    // We'll re-create the controller at the end so future reads can proceed.
    this.fetchAbort.abort();

    const inflight = [...this.inflightChunks.values()].map((e) => e.promise);
    this.cacheGeneration += 1;
    this.lastReadEnd = null;
    this.inMemoryChunks.clear();
    this.resetTelemetry();

    if (this.flushTimer !== null) {
      clearTimeout(this.flushTimer);
      this.flushTimer = null;
    }
    this.flushPending = false;

    this.cancelPendingMetaPersist();
    await Promise.allSettled(inflight);
    this.inflightChunks.clear();

    // Quota pressure can be discovered while waiting for inflight tasks to settle (e.g. a chunk
    // finishes downloading and hits a quota error during `writeBlock`). If persistence is now
    // disabled, fall back to the quota-disabled clear path so we don't attempt further OPFS writes.
    if (this.persistentCacheWritesDisabled) {
      await this.metaWriteChain.catch(() => {});
      this.metaWriteChain = Promise.resolve();
      this.meta = null;
      this.rangeSet = new RangeSet();

      const oldCache = this.cache;
      await oldCache?.close?.().catch(() => {});
      await this.metadataStore.delete(this.cacheId).catch(() => {});
      this.cache = MemorySparseDisk.create({ diskSizeBytes: this.capacityBytesValue, blockSizeBytes: this.opts.chunkSize });

      this.fetchAbort = new AbortController();
      this.fetchSignal = abortAny([this.abort.signal, this.fetchAbort.signal]);
      return;
    }

    const oldCache = this.cache;
    try {
      await oldCache?.close?.();
    } catch (err) {
      if (isQuotaExceededError(err)) {
        this.disablePersistentCacheWrites();
      } else {
        // Ensure the disk remains usable even if the persistent cache handle could not be closed.
        // This method already bumped the cache generation + aborted inflight downloads, so we must
        // recreate `fetchAbort` before propagating.
        this.cache = MemorySparseDisk.create({ diskSizeBytes: this.capacityBytesValue, blockSizeBytes: this.opts.chunkSize });
        this.fetchAbort = new AbortController();
        this.fetchSignal = abortAny([this.abort.signal, this.fetchAbort.signal]);
        // Maintain previous behaviour: propagate unexpected close failures.
        throw err;
      }
    }
    this.cache = null;

    await this.metaWriteChain.catch(() => {
      // best-effort metadata persistence
    });
    this.metaWriteChain = Promise.resolve();
    this.meta = null;
    this.rangeSet = new RangeSet();

    try {
      await this.metadataStore.delete(this.cacheId);
    } catch (err) {
      if (isQuotaExceededError(err)) {
        this.disablePersistentCacheWrites();
      } else {
        // Make sure the disk stays usable even if cache clearing failed.
        this.cache = MemorySparseDisk.create({ diskSizeBytes: this.capacityBytesValue, blockSizeBytes: this.opts.chunkSize });
        this.fetchAbort = new AbortController();
        this.fetchSignal = abortAny([this.abort.signal, this.fetchAbort.signal]);
        throw err;
      }
    }

    if (this.persistentCacheWritesDisabled) {
      this.cache = MemorySparseDisk.create({ diskSizeBytes: this.capacityBytesValue, blockSizeBytes: this.opts.chunkSize });
      this.fetchAbort = new AbortController();
      this.fetchSignal = abortAny([this.abort.signal, this.fetchAbort.signal]);
      return;
    }

    let cache: RemoteRangeDiskSparseCache;
    try {
      cache = await this.sparseCacheFactory.create(this.cacheId, {
        diskSizeBytes: this.capacityBytesValue,
        blockSizeBytes: this.opts.chunkSize,
      });
    } catch (err) {
      if (isQuotaExceededError(err)) {
        this.disablePersistentCacheWrites();
        this.cache = MemorySparseDisk.create({ diskSizeBytes: this.capacityBytesValue, blockSizeBytes: this.opts.chunkSize });
        this.fetchAbort = new AbortController();
        this.fetchSignal = abortAny([this.abort.signal, this.fetchAbort.signal]);
        return;
      }
      this.cache = MemorySparseDisk.create({ diskSizeBytes: this.capacityBytesValue, blockSizeBytes: this.opts.chunkSize });
      this.fetchAbort = new AbortController();
      this.fetchSignal = abortAny([this.abort.signal, this.fetchAbort.signal]);
      throw err;
    }
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
    try {
      await this.metadataStore.write(this.cacheId, metaToPersist);
    } catch (err) {
      if (isQuotaExceededError(err)) {
        this.disablePersistentCacheWrites();
        await cache.close?.().catch(() => {});
        this.cache = MemorySparseDisk.create({ diskSizeBytes: this.capacityBytesValue, blockSizeBytes: this.opts.chunkSize });
        this.meta = null;
        this.rangeSet = new RangeSet();
        this.fetchAbort = new AbortController();
        this.fetchSignal = abortAny([this.abort.signal, this.fetchAbort.signal]);
        return;
      }
      await cache.close?.().catch(() => {});
      this.cache = MemorySparseDisk.create({ diskSizeBytes: this.capacityBytesValue, blockSizeBytes: this.opts.chunkSize });
      this.meta = null;
      this.rangeSet = new RangeSet();
      this.fetchAbort = new AbortController();
      this.fetchSignal = abortAny([this.abort.signal, this.fetchAbort.signal]);
      throw err;
    }

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
    let closeErr: unknown;
    try {
      await cache.flush();
    } catch (err) {
      flushErr = err;
    }
    try {
      await cache.close?.();
    } catch (err) {
      closeErr = err;
    }

    // Persistent caching is best-effort. If quota is exhausted, the cache may not be flushable, but
    // we still want to close/release the underlying handle.
    if (flushErr && !isQuotaExceededError(flushErr)) throw flushErr;
    if (closeErr && !isQuotaExceededError(closeErr)) throw closeErr;
  }

  private scheduleReadAhead(offset: number, length: number, endChunk: number): void {
    if (this.persistentCacheWritesDisabled) return;
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
    if (this.inMemoryChunks.has(chunkIndex)) {
      this.telemetry.cacheHitChunks += 1;
      return;
    }
    if (!this.persistentCacheWritesDisabled && cache.isBlockAllocated(chunkIndex)) {
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
        // Cache was invalidated (or cleared) while we were waiting in the task queue.
        // Do not automatically retry: the caller (readSectors) will restart against the
        // new cache generation if needed, while background prefetches should simply stop.
        return;
      }

      const cache = this.ensureOpen();
      if (this.persistentCacheWritesDisabled) {
        if (this.inMemoryChunks.has(chunkIndex)) return;
        // If persistence has already been disabled and no reads are active, this is likely a
        // background prefetch. Don't keep allocating RAM for cache data that won't be reused.
        if (this.activeReads === 0) return;
      } else {
        if (cache.isBlockAllocated(chunkIndex)) return;
      }

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

        if (this.persistentCacheWritesDisabled) {
          if (this.activeReads > 0) {
            this.inMemoryChunks.set(chunkIndex, bytes);
          }
          if (generation === this.cacheGeneration) {
            this.lastFetchMs = performance.now() - start;
            this.lastFetchAtMs = Date.now();
          }
          return;
        }

        const write = cache.writeBlock(chunkIndex, bytes);
        this.inflightWrites.add(write);
        try {
          await write;
        } catch (err) {
          if (isQuotaExceededError(err)) {
            // Cache is best-effort: if persistence fails due to quota pressure, continue serving
            // reads by keeping the downloaded bytes in memory and disabling further persistent
            // cache writes for the disk lifetime.
            this.disablePersistentCacheWrites();
            if (this.activeReads > 0) {
              this.inMemoryChunks.set(chunkIndex, bytes);
            }
            if (generation === this.cacheGeneration) {
              this.lastFetchMs = performance.now() - start;
              this.lastFetchAtMs = Date.now();
            }
            return;
          }
          throw err;
        } finally {
          this.inflightWrites.delete(write);
        }
        if (generation === this.cacheGeneration) {
          this.lastFetchMs = performance.now() - start;
          this.lastFetchAtMs = Date.now();
        }
        if (this.persistentCacheWritesDisabled) {
          // Another inflight chunk may have hit quota while we were writing. Do not record persistent
          // metadata once persistence is disabled, but keep the bytes in memory so subsequent reads
          // don't need to re-download.
          if (this.activeReads > 0) {
            this.inMemoryChunks.set(chunkIndex, bytes);
          }
          return;
        }
        this.recordCachedChunk(chunkIndex, generation);
        this.scheduleBackgroundFlush();
        return;
      } catch (err) {
        if (this.closed) throw new Error("RemoteRangeDisk is closed");
        if (isAbortError(err) && generation !== this.cacheGeneration) {
          // An inflight fetch was aborted due to cache invalidation/clearCache; do not retry here.
          // Foreground callers will re-issue reads once they observe the generation bump.
          return;
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
    try {
      assertIdentityContentEncoding(resp.headers, `range chunk ${chunkIndex}`);
      assertNoTransformCacheControl(resp.headers, `range chunk ${chunkIndex}`);
    } catch (err) {
      await cancelBody(resp);
      throw err;
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

    const body = await readResponseBytesWithLimit(resp, { maxBytes: expectedLen, label: `range chunk ${chunkIndex}` });
    if (generation === this.cacheGeneration) {
      this.telemetry.bytesDownloaded += body.byteLength;
    }

    if (body.byteLength !== expectedLen) {
      throw new Error(`short range read: expected=${expectedLen} actual=${body.byteLength}`);
    }

    if (this.sha256Manifest) {
      const expected = this.sha256Manifest[chunkIndex];
      if (!expected) {
        throw new Error(`sha256Manifest missing entry for chunk ${chunkIndex}`);
      }
      const actual = await sha256Hex(body);
      if (actual !== expected) {
        throw new Error(`sha256 mismatch for chunk ${chunkIndex}`);
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
    if (this.persistentCacheWritesDisabled) return;
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
        .catch((err) => {
          if (isQuotaExceededError(err)) {
            // A quota failure while flushing indicates we cannot reliably persist further cached data.
            // Disable persistent writes and continue serving reads via network + in-memory chunks.
            this.disablePersistentCacheWrites();
          }
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
    if (this.persistentCacheWritesDisabled) return;

    const start = chunkIndex * this.opts.chunkSize;
    const end = Math.min(start + this.opts.chunkSize, this.capacityBytesValue);
    if (end <= start) return;

    this.rangeSet.insert(start, end);
    meta.cachedRanges = this.rangeSet.getRanges();
    const now = Date.now();
    meta.lastAccessedAtMs = now;
    this.lastMetaTouchAtMs = now;
    this.metaDirty = true;
    this.scheduleMetaPersist(generation);
  }

  private touchMetaAfterRead(generation: number): void {
    const meta = this.meta;
    if (!meta) return;
    if (generation !== this.cacheGeneration) return;
    if (this.persistentCacheWritesDisabled) return;

    const now = Date.now();
    if (now - this.lastMetaTouchAtMs < META_TOUCH_THROTTLE_MS) return;
    this.lastMetaTouchAtMs = now;

    meta.lastAccessedAtMs = now;
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
        if (this.persistentCacheWritesDisabled) return;
        try {
          await this.metadataStore.write(this.cacheId, meta);
        } catch (err) {
          if (isQuotaExceededError(err)) {
            this.disablePersistentCacheWrites();
            return;
          }
          throw err;
        }
      });
    await this.metaWriteChain;
  }

  private async invalidateAndReopenCache(): Promise<void> {
    if (this.closed) throw new Error("RemoteRangeDisk is closed");
    if (this.invalidationPromise) return await this.invalidationPromise;

    this.invalidationPromise = (async () => {
      this.cacheGeneration += 1;
      this.inMemoryChunks.clear();

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
      try {
        await oldCache?.close?.();
      } catch (err) {
        if (isQuotaExceededError(err)) {
          // If we can't flush/close cleanly due to quota pressure, disable persistence and fall back
          // to memory/network reads. We'll still continue the invalidation flow to refresh remote
          // validators.
          this.disablePersistentCacheWrites();
        } else {
          throw err;
        }
      }
      this.cache = null;

      await this.metaWriteChain.catch(() => {
        // best-effort: ensure no metadata write is in-flight before removing the cache directory
      });
      this.metaWriteChain = Promise.resolve();
      this.meta = null;
      this.rangeSet = new RangeSet();

      await this.metadataStore.delete(this.cacheId).catch(() => {
        // best-effort cache invalidation
      });

      await this.maybeRefreshLease();
      const remote = await probeRemoteImage(this.lease, this.opts.fetchFn, { signal: this.fetchSignal });
      this.capacityBytesValue = remote.sizeBytes;
      this.remoteEtag = remote.etag;
      this.remoteLastModified = remote.lastModified;

      if (this.persistentCacheWritesDisabled) {
        // Persistent caching disabled: keep reads working via in-memory downloads only.
        this.cache = MemorySparseDisk.create({ diskSizeBytes: remote.sizeBytes, blockSizeBytes: this.opts.chunkSize });
        return;
      }

      let cache: RemoteRangeDiskSparseCache;
      try {
        cache = await this.sparseCacheFactory.create(this.cacheId, {
          diskSizeBytes: remote.sizeBytes,
          blockSizeBytes: this.opts.chunkSize,
        });
      } catch (err) {
        if (isQuotaExceededError(err)) {
          this.disablePersistentCacheWrites();
          this.cache = MemorySparseDisk.create({ diskSizeBytes: remote.sizeBytes, blockSizeBytes: this.opts.chunkSize });
          return;
        }
        throw err;
      }
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
      try {
        await this.metadataStore.write(this.cacheId, metaToPersist);
      } catch (err) {
        if (isQuotaExceededError(err)) {
          this.disablePersistentCacheWrites();
          await cache.close?.().catch(() => {});
          this.cache = MemorySparseDisk.create({ diskSizeBytes: remote.sizeBytes, blockSizeBytes: this.opts.chunkSize });
          this.meta = null;
          this.rangeSet = new RangeSet();
          return;
        }
        throw err;
      }
    })();

    try {
      await this.invalidationPromise;
    } finally {
      this.invalidationPromise = null;
    }
  }
}
