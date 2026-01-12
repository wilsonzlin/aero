import type { AsyncSectorDisk } from "../storage/disk.ts";
import { RANGE_STREAM_CHUNK_SIZE } from "../storage/chunk_sizes.ts";
import { IdbRemoteChunkCache } from "../storage/idb_remote_chunk_cache.ts";
import { pickDefaultBackend, type DiskBackend } from "../storage/metadata.ts";
import { OpfsLruChunkCache } from "../storage/remote/opfs_lru_chunk_cache.ts";
import { RemoteCacheManager, remoteRangeDeliveryType, type RemoteCacheKeyParts } from "../storage/remote_cache_manager.ts";
import {
  DEFAULT_LEASE_REFRESH_MARGIN_MS,
  DiskAccessLeaseRefresher,
  fetchWithDiskAccessLease,
  type DiskAccessLease,
} from "../storage/disk_access_lease.ts";

export type ByteRange = { start: number; end: number };

export const REMOTE_DISK_SECTOR_SIZE = 512;
// Defensive bounds for remote range streaming. `RemoteStreamingDisk` downloads whole blocks into
// memory, so extremely large `blockSize` or aggressive prefetch settings can cause pathological
// allocations and background work.
//
// Keep these in sync with the remote storage layer (`RemoteRangeDisk` / `RemoteChunkedDisk`) where
// possible.
const MAX_REMOTE_BLOCK_SIZE_BYTES = 64 * 1024 * 1024; // 64 MiB
const MAX_REMOTE_PREFETCH_SEQUENTIAL_BLOCKS = 1024;
const MAX_REMOTE_PREFETCH_SEQUENTIAL_BYTES = 512 * 1024 * 1024; // 512 MiB

function rangeLen(r: ByteRange): number {
  return r.end - r.start;
}

function overlapsOrAdjacent(a: ByteRange, b: ByteRange): boolean {
  return a.start <= b.end && b.start <= a.end;
}

function mergeRanges(a: ByteRange, b: ByteRange): ByteRange {
  return { start: Math.min(a.start, b.start), end: Math.max(a.end, b.end) };
}

function divFloor(n: number, d: number): number {
  if (!Number.isSafeInteger(n) || !Number.isSafeInteger(d) || d <= 0 || n < 0) {
    throw new Error("divFloor: arguments must be safe non-negative integers and divisor must be > 0");
  }
  const out = Number(BigInt(n) / BigInt(d));
  if (!Number.isSafeInteger(out)) throw new Error("divFloor overflow");
  return out;
}

export class RangeSet {
  private ranges: ByteRange[] = [];

  getRanges(): ByteRange[] {
    return [...this.ranges];
  }

  totalLen(): number {
    return this.ranges.reduce((sum, r) => sum + rangeLen(r), 0);
  }

  containsRange(start: number, end: number): boolean {
    if (start >= end) return true;
    for (const r of this.ranges) {
      if (r.end <= start) continue;
      return r.start <= start && r.end >= end;
    }
    return false;
  }

  insert(start: number, end: number): void {
    if (start >= end) return;
    let next: ByteRange = { start, end };
    const out: ByteRange[] = [];
    let inserted = false;
    for (const r of this.ranges) {
      if (r.end < next.start) {
        out.push(r);
        continue;
      }
      if (next.end < r.start) {
        if (!inserted) {
          out.push(next);
          inserted = true;
        }
        out.push(r);
        continue;
      }
      next = mergeRanges(next, r);
    }
    if (!inserted) out.push(next);
    this.ranges = compactRanges(out);
  }

  remove(start: number, end: number): void {
    if (start >= end) return;
    const out: ByteRange[] = [];
    for (const r of this.ranges) {
      if (r.end <= start || r.start >= end) {
        out.push(r);
        continue;
      }
      if (r.start < start) out.push({ start: r.start, end: start });
      if (r.end > end) out.push({ start: end, end: r.end });
    }
    this.ranges = compactRanges(out);
  }
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

export type RemoteDiskProbeResult = {
  size: number;
  etag: string | null;
  lastModified: string | null;
  acceptRanges: string;
  rangeProbeStatus: number;
  partialOk: boolean;
  contentRange: string;
};

class RemoteValidatorMismatchError extends Error {
  status: number;

  constructor(status: number) {
    super(`remote validator mismatch (status=${status})`);
    this.status = status;
  }
}

async function cancelBody(resp: Response): Promise<void> {
  try {
    await resp.body?.cancel();
  } catch {
    // ignore best-effort cancellation failures
  }
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

function extractValidatorFromHeaders(headers: Headers): string | null {
  return headers.get("etag") ?? headers.get("last-modified");
}

export async function probeRemoteDisk(
  url: string,
  opts: { credentials?: RequestCredentials } = {},
): Promise<RemoteDiskProbeResult> {
  let acceptRanges = "";
  let size: number | null = null;
  let etag: string | null = null;
  let lastModified: string | null = null;

  // Prefer HEAD for a cheap size probe, but fall back to a Range GET for servers that
  // disallow HEAD (or omit Content-Length from HEAD).
  try {
    const head = await fetch(url, { method: "HEAD", credentials: opts.credentials });
    if (head.ok) {
      const headSize = Number(head.headers.get("content-length") ?? "NaN");
      if (Number.isFinite(headSize) && headSize > 0) {
        size = headSize;
      }
      acceptRanges = head.headers.get("accept-ranges") ?? "";
      etag = head.headers.get("etag");
      lastModified = head.headers.get("last-modified");
    }
  } catch {
    // ignore; fall back to GET probe
  }

  const probe = await fetch(url, { method: "GET", headers: { Range: "bytes=0-0" }, credentials: opts.credentials });
  const contentRange = probe.headers.get("content-range") ?? "";
  const partialOk = probe.status === 206;
  if (!etag) etag = probe.headers.get("etag");
  if (!lastModified) lastModified = probe.headers.get("last-modified");

  if (size === null && partialOk) {
    if (!contentRange) {
      throw new Error(
        "Range probe returned 206 Partial Content, but Content-Range is not visible. " +
          "If this is cross-origin, the server must set Access-Control-Expose-Headers: Content-Range, Content-Length.",
      );
    }
    size = parseContentRangeHeader(contentRange).total;
  }

  if (size === null || !Number.isFinite(size) || size <= 0) {
    throw new Error(
      "Remote server did not provide a readable image size via Content-Length (HEAD) or Content-Range (Range GET).",
    );
  }

  if (!acceptRanges) {
    acceptRanges = probe.headers.get("accept-ranges") ?? "";
  }

  return {
    size,
    etag,
    lastModified,
    acceptRanges,
    rangeProbeStatus: probe.status,
    partialOk,
    contentRange,
  };
}

function parseContentRangeHeader(header: string): { start: number; endExclusive: number; total: number } {
  // Example: "bytes 0-0/12345"
  const trimmed = header.trim();
  if (!trimmed.startsWith("bytes ")) {
    throw new Error(`invalid Content-Range (expected 'bytes ...'): ${header}`);
  }
  const rest = trimmed.slice("bytes ".length);
  const parts = rest.split("/");
  if (parts.length !== 2) {
    throw new Error(`invalid Content-Range: ${header}`);
  }
  const [rangePart, totalPart] = parts;
  const rangeParts = rangePart.split("-");
  if (rangeParts.length !== 2) {
    throw new Error(`invalid Content-Range: ${header}`);
  }
  const start = Number(rangeParts[0]);
  const endInclusive = Number(rangeParts[1]);
  const total = Number(totalPart);
  if (!Number.isSafeInteger(start) || !Number.isSafeInteger(endInclusive) || !Number.isSafeInteger(total) || total <= 0) {
    throw new Error(`invalid Content-Range numbers: ${header}`);
  }
  const endExclusive = endInclusive + 1;
  if (!Number.isSafeInteger(endExclusive) || endExclusive <= start) {
    throw new Error(`invalid Content-Range: ${header}`);
  }
  return { start, endExclusive, total };
}

export type RemoteDiskCacheStatus = {
  totalSize: number;
  cachedBytes: number;
  cachedRanges: ByteRange[];
  cacheLimitBytes: number | null;
};

export type RemoteDiskOptions = {
  blockSize?: number;
  /**
   * Maximum bytes to keep in the persistent cache (LRU-evicted).
   *
   * - `undefined` (default): use the default limit (currently 512 MiB)
   * - `null`: disable eviction (unbounded cache; subject to browser storage quota)
   * - `0`: disable caching entirely (no OPFS/IDB usage; always fetch via HTTP Range)
   */
  cacheLimitBytes?: number | null;
  prefetchSequentialBlocks?: number;
  cacheBackend?: DiskBackend;
  /**
   * Fetch credential mode for Range requests.
   *
   * Defaults to `same-origin` so cookies are sent for same-origin endpoints but not for
   * cross-origin requests (avoids credentialed CORS unless explicitly requested).
   */
  credentials?: RequestCredentials;
  /**
   * Stable cache identity for the remote disk (used as `imageId` in cache key derivation).
   *
   * This should be a control-plane identifier (e.g. database ID), not a signed URL.
   * Defaults to a normalized URL without query/hash components.
   */
  cacheImageId?: string;
  /**
   * Stable version identifier for the remote disk (used as `version` in cache key derivation).
   *
   * Defaults to `"1"` and should be set when the control plane can provide an immutable version
   * (generation number, snapshot ID, etc).
   */
  cacheVersion?: string;
  /**
   * Override validator used for cache binding when response headers are not readable
   * (e.g. cross-origin without `Access-Control-Expose-Headers: ETag`).
   *
   * If omitted, we bind to the probed response `ETag` when available.
   */
  cacheEtag?: string | null;
  /**
   * Optional expected size for the remote disk image. When provided, a mismatch becomes an error.
   */
  expectedSizeBytes?: number;
  /**
   * For lease-based access, refresh shortly before `expiresAt`.
   */
  leaseRefreshMarginMs?: number;
};

type ResolvedRemoteDiskOptions = {
  blockSize: number;
  cacheLimitBytes: number | null;
  prefetchSequentialBlocks: number;
  cacheBackend: DiskBackend;
  leaseRefreshMarginMs: number;
};

function normalizeCredentials(credentials: RequestCredentials | undefined): RequestCredentials {
  const resolved = credentials ?? "same-origin";
  if (resolved !== "same-origin" && resolved !== "include" && resolved !== "omit") {
    throw new Error(`Invalid credentials mode: ${String(credentials)}`);
  }
  return resolved;
}

function normalizeCacheVersion(version: string | undefined): string {
  const resolved = (version ?? "1").trim();
  if (!resolved) {
    throw new Error("cacheVersion must not be empty");
  }
  return resolved;
}

export type RemoteDiskTelemetrySnapshot = {
  url: string;
  totalSize: number;
  blockSize: number;
  cacheLimitBytes: number | null;
  cachedBytes: number;

  blockRequests: number;
  cacheHits: number;
  cacheMisses: number;
  inflightJoins: number;

  requests: number;
  bytesDownloaded: number;

  inflightFetches: number;

  /**
   * Duration of the most recently completed fetch+persist path.
   *
   * This is intended as a lightweight tuning signal (not a high-resolution profiler).
   */
  lastFetchMs: number | null;
  lastFetchAtMs: number | null;
  lastFetchRange: ByteRange | null;
};

type RemoteDiskTelemetry = {
  blockRequests: number;
  cacheHits: number;
  cacheMisses: number;
  inflightJoins: number;
  requests: number;
  bytesDownloaded: number;
  lastFetchMs: number | null;
  lastFetchAtMs: number | null;
  lastFetchRange: ByteRange | null;
};

function stableImageIdFromUrl(url: string): string {
  // Use URL parsing when possible so we can drop querystring auth material.
  // Fall back to string splitting for relative URLs.
  try {
    const base = (globalThis as typeof globalThis & { location?: { href?: string } }).location?.href;
    const u = base ? new URL(url, base) : new URL(url);
    return `${u.origin}${u.pathname}`;
  } catch {
    const noHash = url.split("#", 1)[0] ?? url;
    return (noHash.split("?", 1)[0] ?? noHash).trim();
  }
}

function cacheKeyPartsFromUrl(url: string, options: RemoteDiskOptions, blockSize: number): RemoteCacheKeyParts {
  const imageId = (options.cacheImageId ?? stableImageIdFromUrl(url)).trim();
  if (!imageId) {
    throw new Error("cacheImageId must not be empty");
  }
  return {
    imageId,
    // Without an explicit control-plane version, treat this as a single logical stream
    // and rely on validators (ETag/Last-Modified/size) for safe invalidation.
    version: normalizeCacheVersion(options.cacheVersion),
    // Include block size in the key material so different cache chunking strategies don't fight
    // (and so we never store delivery secrets like signed URLs in the key).
    deliveryType: remoteRangeDeliveryType(blockSize),
  };
}

export class RemoteStreamingDisk implements AsyncSectorDisk {
  readonly sectorSize = REMOTE_DISK_SECTOR_SIZE;
  readonly capacityBytes: number;
  private readonly sourceId: string;
  private readonly lease: DiskAccessLease;
  private readonly totalSize: number;
  private readonly blockSize: number;
  private readonly cacheLimitBytes: number | null;
  private readonly prefetchSequentialBlocks: number;
  private readonly leaseRefreshMarginMs: number;
  private readonly cacheBackend: DiskBackend;

  private opfsCache: OpfsLruChunkCache | null = null;

  private rangeSet: RangeSet;
  private cachedBytes = 0;
  private lastReadEnd: number | null = null;
  private readonly inflight = new Map<number, Promise<Uint8Array>>();
  private cacheGeneration = 0;
  private idbCache: IdbRemoteChunkCache | null = null;
  private readonly leaseRefresher: DiskAccessLeaseRefresher;
  private remoteEtag: string | null = null;
  private remoteLastModified: string | null = null;
  private validatorReprobePromise: Promise<void> | null = null;

  private telemetry: RemoteDiskTelemetry = {
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
    totalSize: number,
    options: ResolvedRemoteDiskOptions,
    opfsCache?: OpfsLruChunkCache,
  ) {
    this.sourceId = sourceId;
    this.lease = lease;
    this.totalSize = totalSize;
    this.capacityBytes = totalSize;
    this.blockSize = options.blockSize;
    this.cacheLimitBytes = options.cacheLimitBytes;
    this.prefetchSequentialBlocks = options.prefetchSequentialBlocks;
    this.cacheBackend = options.cacheBackend;
    this.leaseRefreshMarginMs = options.leaseRefreshMarginMs;
    this.opfsCache = opfsCache ?? null;

    this.rangeSet = new RangeSet();
    this.leaseRefresher = new DiskAccessLeaseRefresher(this.lease, { refreshMarginMs: this.leaseRefreshMarginMs });
  }

  static async open(url: string, options: RemoteDiskOptions = {}): Promise<RemoteStreamingDisk> {
    const lease = staticDiskLease(url, normalizeCredentials(options.credentials));
    return await RemoteStreamingDisk.openWithLease({ sourceId: url, lease }, options);
  }

  static async openWithLease(
    params: { sourceId: string; lease: DiskAccessLease; etag?: string | null },
    options: RemoteDiskOptions = {},
  ): Promise<RemoteStreamingDisk> {
    if (!params.sourceId) throw new Error("sourceId must not be empty");

    const resolvedCacheLimitBytes =
      options.cacheLimitBytes === undefined ? 512 * 1024 * 1024 : options.cacheLimitBytes;

    const resolved: ResolvedRemoteDiskOptions = {
      blockSize: options.blockSize ?? RANGE_STREAM_CHUNK_SIZE,
      cacheLimitBytes: resolvedCacheLimitBytes,
      prefetchSequentialBlocks: options.prefetchSequentialBlocks ?? 2,
      cacheBackend: options.cacheBackend ?? pickDefaultBackend(),
      leaseRefreshMarginMs: options.leaseRefreshMarginMs ?? DEFAULT_LEASE_REFRESH_MARGIN_MS,
    };

    if (!Number.isSafeInteger(resolved.blockSize) || resolved.blockSize <= 0) {
      throw new Error(`Invalid blockSize=${resolved.blockSize}`);
    }
    if (resolved.blockSize > MAX_REMOTE_BLOCK_SIZE_BYTES) {
      throw new Error(
        `blockSize too large: max=${MAX_REMOTE_BLOCK_SIZE_BYTES} got=${resolved.blockSize}`,
      );
    }
    if (resolved.blockSize % REMOTE_DISK_SECTOR_SIZE !== 0) {
      throw new Error(`blockSize must be a multiple of ${REMOTE_DISK_SECTOR_SIZE}`);
    }
    if (resolved.cacheLimitBytes !== null) {
      if (!Number.isSafeInteger(resolved.cacheLimitBytes) || resolved.cacheLimitBytes < 0) {
        throw new Error(`Invalid cacheLimitBytes=${resolved.cacheLimitBytes}`);
      }
    }
    if (!Number.isSafeInteger(resolved.prefetchSequentialBlocks) || resolved.prefetchSequentialBlocks < 0) {
      throw new Error(`Invalid prefetchSequentialBlocks=${resolved.prefetchSequentialBlocks}`);
    }
    if (resolved.prefetchSequentialBlocks > MAX_REMOTE_PREFETCH_SEQUENTIAL_BLOCKS) {
      throw new Error(
        `prefetchSequentialBlocks too large: max=${MAX_REMOTE_PREFETCH_SEQUENTIAL_BLOCKS} got=${resolved.prefetchSequentialBlocks}`,
      );
    }
    const prefetchBytes = BigInt(resolved.prefetchSequentialBlocks) * BigInt(resolved.blockSize);
    if (prefetchBytes > BigInt(MAX_REMOTE_PREFETCH_SEQUENTIAL_BYTES)) {
      throw new Error(
        `prefetch bytes too large: max=${MAX_REMOTE_PREFETCH_SEQUENTIAL_BYTES} got=${prefetchBytes.toString()}`,
      );
    }
    if (!Number.isSafeInteger(resolved.leaseRefreshMarginMs) || resolved.leaseRefreshMarginMs < 0) {
      throw new Error(`Invalid leaseRefreshMarginMs=${resolved.leaseRefreshMarginMs}`);
    }

    const expectedSizeBytes = options.expectedSizeBytes;
    if (expectedSizeBytes !== undefined) {
      if (!Number.isSafeInteger(expectedSizeBytes) || expectedSizeBytes <= 0) {
        throw new Error(`Invalid expectedSizeBytes=${expectedSizeBytes}`);
      }
    }

    const probe = await probeRemoteDisk(params.lease.url, { credentials: params.lease.credentialsMode });
    if (!probe.partialOk) {
      throw new Error(
        "Remote server does not appear to support HTTP Range requests (required). " +
          "Ensure it returns 206 Partial Content and exposes Content-Range via CORS.",
      );
    }
    if (expectedSizeBytes !== undefined) {
      if (expectedSizeBytes !== probe.size) {
        throw new Error(`Remote disk size mismatch: expected=${expectedSizeBytes} actual=${probe.size}`);
      }
    }

    const parts = cacheKeyPartsFromUrl(params.sourceId, options, resolved.blockSize);
    // Cache disabled: do not touch OPFS / IndexedDB at all (use direct Range fetches only).
    // Note: `cacheLimitBytes: null` means "unlimited cache", so `0` is the explicit disable signal.
    if (resolved.cacheLimitBytes === 0) {
      const disk = new RemoteStreamingDisk(parts.imageId, params.lease, probe.size, resolved);
      disk.remoteEtag = probe.etag;
      disk.remoteLastModified = probe.lastModified;
      disk.leaseRefresher.start();
      return disk;
    }
    const cacheKey = await RemoteCacheManager.deriveCacheKey(parts);
    const resolvedEtag = options.cacheEtag !== undefined ? options.cacheEtag : params.etag ?? probe.etag;
    const validators = { sizeBytes: probe.size, etag: resolvedEtag, lastModified: probe.lastModified };

    if (resolved.cacheBackend === "idb") {
      const disk = new RemoteStreamingDisk(parts.imageId, params.lease, probe.size, resolved);
      disk.remoteEtag = probe.etag;
      disk.remoteLastModified = probe.lastModified;
      disk.idbCache = await IdbRemoteChunkCache.open({
        cacheKey,
        signature: {
          imageId: parts.imageId,
          version: parts.version,
          etag: resolvedEtag,
          lastModified: probe.lastModified,
          sizeBytes: probe.size,
          chunkSize: resolved.blockSize,
        },
        cacheLimitBytes: resolved.cacheLimitBytes,
      });
      const status = await disk.idbCache.getStatus();
      disk.cachedBytes = status.bytesUsed;
      disk.leaseRefresher.start();
      return disk;
    }

    const manager = await RemoteCacheManager.openOpfs();
    // Ensure the cache directory is bound to the current validators (ETag/Last-Modified/size).
    // If the remote image changed, this will clear any previously cached bytes.
    await manager.openCache(parts, { chunkSizeBytes: resolved.blockSize, validators });

    const opfsCache = await OpfsLruChunkCache.open({
      cacheKey,
      chunkSize: resolved.blockSize,
      maxBytes: resolved.cacheLimitBytes,
    });

    const disk = new RemoteStreamingDisk(parts.imageId, params.lease, probe.size, resolved, opfsCache);
    disk.remoteEtag = probe.etag;
    disk.remoteLastModified = probe.lastModified;
    const indices = await opfsCache.getChunkIndices();
    for (const idx of indices) {
      const r = disk.blockRange(idx);
      disk.rangeSet.insert(r.start, r.end);
    }
    disk.cachedBytes = (await opfsCache.getStats()).totalBytes;
    disk.leaseRefresher.start();
    return disk;
  }

  async getCacheStatus(): Promise<RemoteDiskCacheStatus> {
    if (this.cacheLimitBytes === 0) {
      return {
        totalSize: this.totalSize,
        cachedBytes: 0,
        cachedRanges: [],
        cacheLimitBytes: 0,
      };
    }
    if (this.cacheBackend === "idb") {
      if (!this.idbCache) throw new Error("Remote disk IDB cache not initialized");
      const status = await this.idbCache.getStatus();
      this.cachedBytes = status.bytesUsed;
      const indices = await this.idbCache.listChunkIndices();
      const set = new RangeSet();
      for (const idx of indices) {
        const r = this.blockRange(idx);
        set.insert(r.start, r.end);
      }
      return {
        totalSize: this.totalSize,
        cachedBytes: status.bytesUsed,
        cachedRanges: set.getRanges(),
        cacheLimitBytes: this.cacheLimitBytes,
      };
    }

    if (!this.opfsCache) throw new Error("Remote disk OPFS cache not initialized");
    this.cachedBytes = (await this.opfsCache.getStats()).totalBytes;
    return {
      totalSize: this.totalSize,
      cachedBytes: this.cachedBytes,
      cachedRanges: this.rangeSet.getRanges(),
      cacheLimitBytes: this.cacheLimitBytes,
    };
  }

  getTelemetrySnapshot(): RemoteDiskTelemetrySnapshot {
    return {
      url: this.sourceId,
      totalSize: this.totalSize,
      blockSize: this.blockSize,
      cacheLimitBytes: this.cacheLimitBytes,
      cachedBytes: this.cachedBytes,

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

  async flushCache(): Promise<void> {
    if (this.cacheLimitBytes === 0) return;
    if (this.cacheBackend === "idb") return;
    await this.opfsCache?.flush();
  }

  async readInto(offset: number, dest: Uint8Array, onLog?: (msg: string) => void): Promise<void> {
    const length = dest.byteLength;
    if (length === 0) {
      this.lastReadEnd = offset;
      return;
    }
    if (offset + length > this.totalSize) {
      throw new Error("Read beyond end of image.");
    }

    let invalidations = 0;
    while (true) {
      const generation = this.cacheGeneration;
      const startBlock = divFloor(offset, this.blockSize);
      const endBlock = divFloor(offset + length - 1, this.blockSize);

      try {
        // Batch-load cached blocks when using IndexedDB. This reduces IDB roundtrips when a read spans
        // multiple blocks (e.g. large sequential reads).
        if (this.cacheBackend === "idb" && this.idbCache && endBlock > startBlock) {
          const indices: number[] = [];
          for (let block = startBlock; block <= endBlock; block += 1) indices.push(block);
          await this.idbCache.getMany(indices);
        }

        let written = 0;
        for (let block = startBlock; block <= endBlock; block++) {
          const bytes = await this.getBlock(block, onLog);
          if (generation !== this.cacheGeneration) {
            // The cache was invalidated while we were reading (clearCache or validator mismatch). Restart
            // the read against the new cache generation.
            break;
          }
          const blockStart = block * this.blockSize;
          const inBlockStart = offset > blockStart ? offset - blockStart : 0;
          const toCopy = Math.min(length - written, bytes.length - inBlockStart);
          dest.set(bytes.subarray(inBlockStart, inBlockStart + toCopy), written);
          written += toCopy;
        }

        if (generation !== this.cacheGeneration) {
          continue;
        }

        // Prefetch is best-effort and should not delay the caller's read completion.
        void this.maybePrefetch(offset, length, onLog).catch(() => {});
        return;
      } catch (err) {
        if (err instanceof RemoteValidatorMismatchError && invalidations < 1) {
          invalidations += 1;
          await this.reprobeValidatorAndClearCache();
          continue;
        }
        throw err;
      }
    }
  }

  async read(offset: number, length: number, onLog?: (msg: string) => void): Promise<Uint8Array> {
    const out = new Uint8Array(length);
    await this.readInto(offset, out, onLog);
    return out;
  }

  async readSectors(lba: number, buffer: Uint8Array, onLog?: (msg: string) => void): Promise<void> {
    if (buffer.byteLength % REMOTE_DISK_SECTOR_SIZE !== 0) {
      throw new Error(`unaligned buffer length ${buffer.byteLength} (expected multiple of ${REMOTE_DISK_SECTOR_SIZE})`);
    }
    const offset = lba * REMOTE_DISK_SECTOR_SIZE;
    if (!Number.isSafeInteger(offset)) {
      throw new Error(`offset overflow (lba=${lba})`);
    }
    await this.readInto(offset, buffer, onLog);
  }

  async writeSectors(_lba: number, _data: Uint8Array): Promise<void> {
    throw new Error("remote disk is read-only");
  }

  async flush(): Promise<void> {
    await this.flushCache();
  }

  async clearCache(): Promise<void> {
    this.cacheGeneration += 1;

    // Reset in-memory bookkeeping immediately so any reads that occur while the
    // underlying cache clear is in-flight will contribute to the new generation's
    // telemetry (see `remote_disk_idb.test.ts`).
    this.rangeSet = new RangeSet();
    this.cachedBytes = 0;
    this.lastReadEnd = null;
    this.inflight.clear();
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

    if (this.cacheLimitBytes === 0) return;

    if (this.cacheBackend === "idb") {
      if (!this.idbCache) throw new Error("Remote disk IDB cache not initialized");
      await this.idbCache.clear();
      return;
    }

    if (!this.opfsCache) throw new Error("Remote disk OPFS cache not initialized");
    await this.opfsCache.clear();
  }

  async close(): Promise<void> {
    this.leaseRefresher.stop();
    this.idbCache?.close();
    this.idbCache = null;
  }

  private async maybePrefetch(offset: number, length: number, onLog?: (msg: string) => void): Promise<void> {
    const sequential = this.lastReadEnd !== null && this.lastReadEnd === offset;
    this.lastReadEnd = offset + length;
    if (!sequential) return;

    const nextOffset = offset + length;
    const nextBlock = divFloor(nextOffset, this.blockSize);
    for (let i = 0; i < this.prefetchSequentialBlocks; i++) {
      const block = nextBlock + i;
      if (block * this.blockSize >= this.totalSize) break;
      try {
        await this.getBlock(block, onLog);
      } catch (err) {
        if (err instanceof RemoteValidatorMismatchError) {
          await this.reprobeValidatorAndClearCache();
          return;
        }
        // best-effort prefetch
      }
    }
  }

  private blockRange(blockIndex: number): ByteRange {
    const start = blockIndex * this.blockSize;
    const end = Math.min(start + this.blockSize, this.totalSize);
    return { start, end };
  }

  private async getBlock(blockIndex: number, onLog?: (msg: string) => void): Promise<Uint8Array> {
    if (this.validatorReprobePromise) {
      await this.validatorReprobePromise;
    }

    this.telemetry.blockRequests++;
    if (this.cacheLimitBytes === 0) {
      return await this.getBlockNoCache(blockIndex, onLog);
    }
    if (this.cacheBackend === "idb") {
      return await this.getBlockIdb(blockIndex, onLog);
    }

    const r = this.blockRange(blockIndex);
    if (!this.opfsCache) throw new Error("Remote disk OPFS cache not initialized");

    const generation = this.cacheGeneration;
    const expectedLen = r.end - r.start;
    const cached = await this.opfsCache.getChunk(blockIndex, expectedLen);
    if (cached) {
      // If the cache was cleared while we were awaiting OPFS reads, treat this as a
      // best-effort read-through hit but avoid repopulating state/telemetry for the
      // new generation.
      if (generation === this.cacheGeneration) {
        this.telemetry.cacheHits++;
        this.rangeSet.insert(r.start, r.end);
      }
      return cached;
    }

    // Heal: the cache entry disappeared or was corrupt.
    if (generation === this.cacheGeneration) {
      this.rangeSet.remove(r.start, r.end);
      this.cachedBytes = (await this.opfsCache.getStats()).totalBytes;
    }

    const existing = this.inflight.get(blockIndex);
    if (existing) {
      this.telemetry.inflightJoins++;
      return await existing;
    }

    const fetchGeneration = this.cacheGeneration;
    const task = (async () => {
      const start = performance.now();
      if (fetchGeneration === this.cacheGeneration) {
        this.telemetry.cacheMisses++;
        this.telemetry.requests++;
        this.telemetry.lastFetchRange = { ...r };
      }
      onLog?.(`cache miss: fetching bytes=${r.start}-${r.end - 1}`);
      const buf = await this.fetchRange(r);
      // If the caller cleared the cache while this fetch was in-flight, allow the read to
      // complete but avoid repopulating the cache/telemetry for the new generation.
      if (fetchGeneration !== this.cacheGeneration) {
        return buf;
      }
      this.telemetry.bytesDownloaded += buf.byteLength;

      const put = await this.opfsCache!.putChunk(blockIndex, buf);
      if (put.stored) {
        this.rangeSet.insert(r.start, r.end);
      }
      for (const evicted of put.evicted) {
        const evictedRange = this.blockRange(evicted);
        this.rangeSet.remove(evictedRange.start, evictedRange.end);
      }
      this.cachedBytes = (await this.opfsCache!.getStats()).totalBytes;
      this.telemetry.lastFetchMs = performance.now() - start;
      this.telemetry.lastFetchAtMs = Date.now();
      return buf;
    })();

    this.inflight.set(blockIndex, task);
    try {
      return await task;
    } finally {
      if (this.inflight.get(blockIndex) === task) {
        this.inflight.delete(blockIndex);
      }
    }
  }

  private async getBlockNoCache(blockIndex: number, onLog?: (msg: string) => void): Promise<Uint8Array> {
    const r = this.blockRange(blockIndex);

    const existing = this.inflight.get(blockIndex);
    if (existing) {
      this.telemetry.inflightJoins++;
      return await existing;
    }

    const generation = this.cacheGeneration;
    const task = (async () => {
      const start = performance.now();
      this.telemetry.cacheMisses++;
      this.telemetry.requests++;
      this.telemetry.lastFetchRange = { ...r };
      onLog?.(`fetching bytes=${r.start}-${r.end - 1}`);
      const buf = await this.fetchRange(r);

      if (generation !== this.cacheGeneration) {
        return buf;
      }

      this.telemetry.bytesDownloaded += buf.byteLength;
      this.telemetry.lastFetchMs = performance.now() - start;
      this.telemetry.lastFetchAtMs = Date.now();
      return buf;
    })();

    this.inflight.set(blockIndex, task);
    try {
      return await task;
    } finally {
      if (this.inflight.get(blockIndex) === task) {
        this.inflight.delete(blockIndex);
      }
    }
  }

  private async getBlockIdb(blockIndex: number, onLog?: (msg: string) => void): Promise<Uint8Array> {
    if (!this.idbCache) throw new Error("Remote disk IDB cache not initialized");

    const r = this.blockRange(blockIndex);

    const existing = this.inflight.get(blockIndex);
    if (existing) {
      this.telemetry.inflightJoins++;
      return await existing;
    }

    const generation = this.cacheGeneration;
    const task = (async () => {
      const start = performance.now();
      const cached = await this.idbCache!.get(blockIndex);
      if (cached) {
        if (cached.byteLength === r.end - r.start) {
          // Only attribute hits to the current generation; the cache might have been cleared
          // while we were awaiting IndexedDB.
          if (generation === this.cacheGeneration) {
            this.telemetry.cacheHits++;
          }
          return cached;
        }
        // Heal: cached but wrong size.
        // If the cache was cleared while we were awaiting IndexedDB, skip healing so we
        // don't write into a new generation.
        if (generation === this.cacheGeneration) {
          await this.idbCache!.delete(blockIndex);
        }
      }

      if (generation === this.cacheGeneration) {
        this.telemetry.cacheMisses++;
        this.telemetry.requests++;
        this.telemetry.lastFetchRange = { ...r };
      }
      onLog?.(`cache miss: fetching bytes=${r.start}-${r.end - 1}`);
      const buf = await this.fetchRange(r);

      if (generation !== this.cacheGeneration) {
        return buf;
      }

      await this.idbCache!.put(blockIndex, buf);
      const status = await this.idbCache!.getStatus();
      this.cachedBytes = status.bytesUsed;
      this.telemetry.bytesDownloaded += buf.byteLength;
      this.telemetry.lastFetchMs = performance.now() - start;
      this.telemetry.lastFetchAtMs = Date.now();
      return buf;
    })();

    this.inflight.set(blockIndex, task);
    try {
      return await task;
    } finally {
      if (this.inflight.get(blockIndex) === task) {
        this.inflight.delete(blockIndex);
      }
    }
  }

  private async maybeRefreshLease(): Promise<void> {
    const expiresAt = this.lease.expiresAt;
    if (!expiresAt) return;
    const refreshAtMs = expiresAt.getTime() - this.leaseRefreshMarginMs;
    if (!Number.isFinite(refreshAtMs) || Date.now() < refreshAtMs) return;
    await this.lease.refresh();
  }

  private expectedValidator(): string | null {
    return this.remoteEtag ?? this.remoteLastModified;
  }

  private ifRangeHeaderValue(): string | null {
    if (this.remoteEtag && !isWeakEtag(this.remoteEtag)) {
      return this.remoteEtag;
    }
    return this.remoteLastModified;
  }

  private async fetchRange(r: ByteRange): Promise<Uint8Array> {
    const expectedLen = r.end - r.start;
    const headers: Record<string, string> = {
      Range: `bytes=${r.start}-${r.end - 1}`,
    };

    const ifRange = this.ifRangeHeaderValue();
    if (ifRange) headers["If-Range"] = ifRange;

    const expectedValidator = this.expectedValidator();

    await this.maybeRefreshLease();
    const resp = await fetchWithDiskAccessLease(this.lease, { headers }, { retryAuthOnce: true });

    if (resp.status === 200 || resp.status === 412) {
      await cancelBody(resp);
      // A server will return 200 (full representation) when an If-Range validator does not match.
      // Some implementations use 412 instead. Avoid mislabeling: only treat 200 as a mismatch when
      // the response provides a validator that differs from what we expected.
      if (ifRange && expectedValidator) {
        if (resp.status === 412) {
          throw new RemoteValidatorMismatchError(resp.status);
        }
        const actual = extractValidatorFromHeaders(resp.headers);
        if (actual && !validatorsMatch(expectedValidator, actual)) {
          throw new RemoteValidatorMismatchError(resp.status);
        }
      }
      throw new Error(`Expected 206 Partial Content, got ${resp.status}`);
    }

    if (resp.status !== 206) {
      await cancelBody(resp);
      throw new Error(`Expected 206 Partial Content, got ${resp.status}`);
    }

    // Servers that don't implement If-Range may still return 206 after the representation has
    // changed. When the response includes a validator (ETag / Last-Modified), detect mismatches to
    // avoid mixing bytes from different versions under one cache identity.
    if (expectedValidator) {
      const actual = extractValidatorFromHeaders(resp.headers);
      if (actual && !validatorsMatch(expectedValidator, actual)) {
        await cancelBody(resp);
        throw new RemoteValidatorMismatchError(206);
      }
    }

    const buf = new Uint8Array(await resp.arrayBuffer());
    if (buf.length !== expectedLen) {
      throw new Error(`Unexpected range length: expected ${expectedLen}, got ${buf.length}`);
    }
    return buf;
  }

  private async reprobeValidatorAndClearCache(): Promise<void> {
    if (this.validatorReprobePromise) {
      return await this.validatorReprobePromise;
    }

    this.validatorReprobePromise = (async () => {
      // Invalidate local caches to avoid mixing old and new bytes under one identity.
      this.cacheGeneration += 1;
      this.rangeSet = new RangeSet();
      this.cachedBytes = 0;
      this.lastReadEnd = null;
      this.inflight.clear();

      if (this.cacheLimitBytes !== 0) {
        if (this.cacheBackend === "idb") {
          await this.idbCache?.clear();
        } else {
          await this.opfsCache?.clear();
        }
      }

      await this.maybeRefreshLease();
      const probe = await probeRemoteDisk(this.lease.url, { credentials: this.lease.credentialsMode });
      if (!probe.partialOk) {
        throw new Error(`Remote server ignored Range probe (expected 206, got ${probe.rangeProbeStatus})`);
      }
      if (probe.size !== this.totalSize) {
        throw new Error(`Remote disk size mismatch: expected=${this.totalSize} actual=${probe.size}`);
      }
      this.remoteEtag = probe.etag;
      this.remoteLastModified = probe.lastModified;
    })();

    try {
      await this.validatorReprobePromise;
    } finally {
      this.validatorReprobePromise = null;
    }
  }

  // OPFS cache eviction is handled by `OpfsLruChunkCache` during `putChunk()`.
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

export async function stableCacheKey(url: string, options: RemoteDiskOptions = {}): Promise<string> {
  const blockSize = options.blockSize ?? RANGE_STREAM_CHUNK_SIZE;
  return await RemoteCacheManager.deriveCacheKey(cacheKeyPartsFromUrl(url, options, blockSize));
}

// Backwards-compatible alias: this disk implementation uses HTTP Range requests.
export { RemoteStreamingDisk as RemoteRangeDisk };
