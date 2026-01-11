import type { AsyncSectorDisk } from "../storage/disk";
import { IdbRemoteChunkCache } from "../storage/idb_remote_chunk_cache";
import { pickDefaultBackend, type DiskBackend } from "../storage/metadata";
import { RemoteCacheManager, type RemoteCacheDirectoryHandle, type RemoteCacheKeyParts, type RemoteCacheMetaV1 } from "../storage/remote_cache_manager";

export type ByteRange = { start: number; end: number };

export const REMOTE_DISK_SECTOR_SIZE = 512;

function rangeLen(r: ByteRange): number {
  return r.end - r.start;
}

function overlapsOrAdjacent(a: ByteRange, b: ByteRange): boolean {
  return a.start <= b.end && b.start <= a.end;
}

function mergeRanges(a: ByteRange, b: ByteRange): ByteRange {
  return { start: Math.min(a.start, b.start), end: Math.max(a.end, b.end) };
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

export async function probeRemoteDisk(url: string): Promise<RemoteDiskProbeResult> {
  let acceptRanges = "";
  let size: number | null = null;
  let etag: string | null = null;
  let lastModified: string | null = null;

  // Prefer HEAD for a cheap size probe, but fall back to a Range GET for servers that
  // disallow HEAD (or omit Content-Length from HEAD).
  try {
    const head = await fetch(url, { method: "HEAD" });
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

  const probe = await fetch(url, { method: "GET", headers: { Range: "bytes=0-0" } });
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

type CacheMeta = RemoteCacheMetaV1;

export type RemoteDiskCacheStatus = {
  totalSize: number;
  cachedBytes: number;
  cachedRanges: ByteRange[];
  cacheLimitBytes: number | null;
};

export type RemoteDiskOptions = {
  blockSize?: number;
  cacheLimitBytes?: number | null;
  prefetchSequentialBlocks?: number;
  cacheBackend?: DiskBackend;
};

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

function cacheKeyPartsFromUrl(url: string): RemoteCacheKeyParts {
  return {
    imageId: stableImageIdFromUrl(url),
    // Without an explicit control-plane version, treat this as a single logical stream
    // and rely on validators (ETag/Last-Modified/size) for safe invalidation.
    version: "1",
    deliveryType: "range",
  };
}

function metaFromParts(
  parts: RemoteCacheKeyParts,
  validators: { sizeBytes: number; etag: string | null; lastModified: string | null },
  chunkSizeBytes: number,
): CacheMeta {
  const now = Date.now();
  return {
    version: 1,
    imageId: parts.imageId,
    imageVersion: parts.version,
    deliveryType: parts.deliveryType,
    validators: {
      sizeBytes: validators.sizeBytes,
      etag: validators.etag ?? undefined,
      lastModified: validators.lastModified ?? undefined,
    },
    chunkSizeBytes,
    createdAtMs: now,
    lastAccessedAtMs: now,
    accessCounter: 0,
    chunkLastAccess: {},
    cachedRanges: [],
  };
}

export class RemoteStreamingDisk implements AsyncSectorDisk {
  readonly sectorSize = REMOTE_DISK_SECTOR_SIZE;
  readonly capacityBytes: number;
  private readonly url: string;
  private readonly totalSize: number;
  private readonly blockSize: number;
  private readonly cacheLimitBytes: number | null;
  private readonly prefetchSequentialBlocks: number;
  private readonly cacheKey: string;
  private readonly cacheBackend: DiskBackend;
  private readonly cacheKeyParts: RemoteCacheKeyParts;
  private readonly cacheValidators: { sizeBytes: number; etag: string | null; lastModified: string | null };

  private readonly cacheManager: RemoteCacheManager | null;
  private cacheDir: RemoteCacheDirectoryHandle | null = null;
  private blocksDir: RemoteCacheDirectoryHandle | null = null;

  private meta: CacheMeta;
  private rangeSet: RangeSet;
  private cachedBytes = 0;
  private lastReadEnd: number | null = null;
  private readonly inflight = new Map<number, Promise<Uint8Array>>();
  private cacheGeneration = 0;
  private idbCache: IdbRemoteChunkCache | null = null;

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
    url: string,
    totalSize: number,
    cacheKey: string,
    parts: RemoteCacheKeyParts,
    validators: { sizeBytes: number; etag: string | null; lastModified: string | null },
    options: Required<RemoteDiskOptions>,
    opfsCache?: { manager: RemoteCacheManager; dir: RemoteCacheDirectoryHandle; blocksDir: RemoteCacheDirectoryHandle; meta: CacheMeta },
  ) {
    this.url = url;
    this.totalSize = totalSize;
    this.capacityBytes = totalSize;
    this.blockSize = options.blockSize;
    this.cacheLimitBytes = options.cacheLimitBytes;
    this.prefetchSequentialBlocks = options.prefetchSequentialBlocks;
    this.cacheBackend = options.cacheBackend;

    this.cacheKeyParts = parts;
    this.cacheValidators = validators;
    this.cacheKey = cacheKey;

    this.cacheManager = opfsCache?.manager ?? null;
    this.cacheDir = opfsCache?.dir ?? null;
    this.blocksDir = opfsCache?.blocksDir ?? null;

    this.meta = opfsCache?.meta ?? metaFromParts(parts, validators, options.blockSize);
    this.meta.accessCounter ??= 0;
    this.meta.chunkLastAccess ??= {};

    this.rangeSet = new RangeSet();
    for (const r of this.meta.cachedRanges) this.rangeSet.insert(r.start, r.end);
    this.cachedBytes = this.rangeSet.totalLen();
  }

  static async open(url: string, options: RemoteDiskOptions = {}): Promise<RemoteStreamingDisk> {
    const probe = await probeRemoteDisk(url);
    if (!probe.partialOk) {
      throw new Error(
        "Remote server does not appear to support HTTP Range requests (required). " +
          "Ensure it returns 206 Partial Content and exposes Content-Range via CORS.",
      );
    }

    const resolved: Required<RemoteDiskOptions> = {
      blockSize: options.blockSize ?? 1024 * 1024,
      cacheLimitBytes: options.cacheLimitBytes ?? 512 * 1024 * 1024,
      prefetchSequentialBlocks: options.prefetchSequentialBlocks ?? 2,
      cacheBackend: options.cacheBackend ?? pickDefaultBackend(),
    };

    if (!Number.isSafeInteger(resolved.blockSize) || resolved.blockSize <= 0) {
      throw new Error(`Invalid blockSize=${resolved.blockSize}`);
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

    const parts = cacheKeyPartsFromUrl(url);
    const cacheKey = await RemoteCacheManager.deriveCacheKey(parts);
    const validators = { sizeBytes: probe.size, etag: probe.etag, lastModified: probe.lastModified };

    if (resolved.cacheBackend === "idb") {
      const disk = new RemoteStreamingDisk(url, probe.size, cacheKey, parts, validators, resolved);
      disk.idbCache = await IdbRemoteChunkCache.open({
        cacheKey,
        signature: {
          imageId: parts.imageId,
          version: parts.version,
          etag: probe.etag,
          sizeBytes: probe.size,
          chunkSize: resolved.blockSize,
        },
        cacheLimitBytes: resolved.cacheLimitBytes,
      });
      const status = await disk.idbCache.getStatus();
      disk.cachedBytes = status.bytesUsed;
      return disk;
    }

    const manager = await RemoteCacheManager.openOpfs();
    const cache = await manager.openCache(parts, { chunkSizeBytes: resolved.blockSize, validators });
    const cacheDir = cache.dir as unknown as RemoteCacheDirectoryHandle;
    const blocksDir = (await cacheDir.getDirectoryHandle("blocks", { create: true })) as unknown as RemoteCacheDirectoryHandle;

    return new RemoteStreamingDisk(url, probe.size, cacheKey, parts, validators, resolved, {
      manager,
      dir: cacheDir,
      blocksDir,
      meta: cache.meta,
    });
  }

  async getCacheStatus(): Promise<RemoteDiskCacheStatus> {
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

    return {
      totalSize: this.totalSize,
      cachedBytes: this.cachedBytes,
      cachedRanges: this.rangeSet.getRanges(),
      cacheLimitBytes: this.cacheLimitBytes,
    };
  }

  getTelemetrySnapshot(): RemoteDiskTelemetrySnapshot {
    return {
      url: this.url,
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
    if (this.cacheBackend === "idb") return;
    await this.persistMeta();
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

    const startBlock = Math.floor(offset / this.blockSize);
    const endBlock = Math.floor((offset + length - 1) / this.blockSize);

    let written = 0;
    for (let block = startBlock; block <= endBlock; block++) {
      const bytes = await this.getBlock(block, onLog);
      const blockStart = block * this.blockSize;
      const inBlockStart = offset > blockStart ? offset - blockStart : 0;
      const toCopy = Math.min(length - written, bytes.length - inBlockStart);
      dest.set(bytes.subarray(inBlockStart, inBlockStart + toCopy), written);
      written += toCopy;
    }

    await this.maybePrefetch(offset, length, onLog);
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
    if (this.cacheBackend === "idb") {
      if (!this.idbCache) throw new Error("Remote disk IDB cache not initialized");
      await this.idbCache.clear();
      const status = await this.idbCache.getStatus();
      this.cachedBytes = status.bytesUsed;
    } else {
      if (!this.cacheManager) throw new Error("Remote disk OPFS cache manager not initialized");
      await this.cacheManager.clearCache(this.cacheKey);
      const reopened = await this.cacheManager.openCache(this.cacheKeyParts, {
        chunkSizeBytes: this.blockSize,
        validators: this.cacheValidators,
      });
      this.cacheDir = reopened.dir as unknown as RemoteCacheDirectoryHandle;
      this.blocksDir = (await this.cacheDir.getDirectoryHandle("blocks", { create: true })) as unknown as RemoteCacheDirectoryHandle;
      this.meta = reopened.meta;
      this.meta.accessCounter ??= 0;
      this.meta.chunkLastAccess ??= {};
    }
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
  }

  async close(): Promise<void> {
    this.idbCache?.close();
    this.idbCache = null;
  }

  private async maybePrefetch(offset: number, length: number, onLog?: (msg: string) => void): Promise<void> {
    const sequential = this.lastReadEnd !== null && this.lastReadEnd === offset;
    this.lastReadEnd = offset + length;
    if (!sequential) return;

    const nextOffset = offset + length;
    const nextBlock = Math.floor(nextOffset / this.blockSize);
    for (let i = 0; i < this.prefetchSequentialBlocks; i++) {
      const block = nextBlock + i;
      if (block * this.blockSize >= this.totalSize) break;
      try {
        await this.getBlock(block, onLog);
      } catch {
        // best-effort prefetch
      }
    }
  }

  private blockRange(blockIndex: number): ByteRange {
    const start = blockIndex * this.blockSize;
    const end = Math.min(start + this.blockSize, this.totalSize);
    return { start, end };
  }

  private blockFileName(blockIndex: number): string {
    return `${blockIndex}.bin`;
  }

  private noteAccess(blockIndex: number): void {
    this.meta.accessCounter = (this.meta.accessCounter ?? 0) + 1;
    (this.meta.chunkLastAccess ??= {})[String(blockIndex)] = this.meta.accessCounter;
    this.meta.lastAccessedAtMs = Date.now();
  }

  private async getBlock(blockIndex: number, onLog?: (msg: string) => void): Promise<Uint8Array> {
    this.telemetry.blockRequests++;
    if (this.cacheBackend === "idb") {
      return await this.getBlockIdb(blockIndex, onLog);
    }

    const blocksDir = this.blocksDir;
    if (!blocksDir) throw new Error("Remote disk OPFS blocks directory not initialized");

    const r = this.blockRange(blockIndex);
    if (this.rangeSet.containsRange(r.start, r.end)) {
      try {
        const handle = await blocksDir.getFileHandle(this.blockFileName(blockIndex), { create: false });
        const file = await handle.getFile();
        const bytes = new Uint8Array(await file.arrayBuffer());
        if (bytes.length === r.end - r.start) {
          this.telemetry.cacheHits++;
          this.noteAccess(blockIndex);
          await this.persistMeta();
          return bytes;
        }
      } catch {
        // treat as cache miss and heal metadata below
      }

      // Heal: metadata said cached but file missing/corrupt.
      this.rangeSet.remove(r.start, r.end);
      delete (this.meta.chunkLastAccess ?? {})[String(blockIndex)];
      this.meta.cachedRanges = this.rangeSet.getRanges();
      this.cachedBytes = this.rangeSet.totalLen();
      await this.persistMeta();
    }

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
      onLog?.(`cache miss: fetching bytes=${r.start}-${r.end - 1}`);
      const resp = await fetch(this.url, { headers: { Range: `bytes=${r.start}-${r.end - 1}` } });
      if (resp.status !== 206) {
        throw new Error(`Expected 206 Partial Content, got ${resp.status}`);
      }
      const buf = new Uint8Array(await resp.arrayBuffer());
      if (buf.length !== r.end - r.start) {
        throw new Error(`Unexpected range length: expected ${r.end - r.start}, got ${buf.length}`);
      }
      // If the caller cleared the cache while this fetch was in-flight, allow the read to
      // complete but avoid repopulating the cache/telemetry for the new generation.
      if (generation !== this.cacheGeneration) {
        return buf;
      }
      this.telemetry.bytesDownloaded += buf.byteLength;

      const handle = await blocksDir.getFileHandle(this.blockFileName(blockIndex), { create: true });
      const writable = await handle.createWritable({ keepExistingData: false });
      await writable.write(buf);
      await writable.close();

      this.rangeSet.insert(r.start, r.end);
      this.cachedBytes = this.rangeSet.totalLen();
      this.noteAccess(blockIndex);
      this.meta.cachedRanges = this.rangeSet.getRanges();
      await this.persistMeta();
      await this.enforceCacheLimit(blockIndex);
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
          this.telemetry.cacheHits++;
          return cached;
        }
        // Heal: cached but wrong size.
        await this.idbCache!.delete(blockIndex);
      }

      this.telemetry.cacheMisses++;
      this.telemetry.requests++;
      this.telemetry.lastFetchRange = { ...r };
      onLog?.(`cache miss: fetching bytes=${r.start}-${r.end - 1}`);
      const resp = await fetch(this.url, { headers: { Range: `bytes=${r.start}-${r.end - 1}` } });
      if (resp.status !== 206) {
        throw new Error(`Expected 206 Partial Content, got ${resp.status}`);
      }
      const buf = new Uint8Array(await resp.arrayBuffer());
      if (buf.length !== r.end - r.start) {
        throw new Error(`Unexpected range length: expected ${r.end - r.start}, got ${buf.length}`);
      }

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

  private async enforceCacheLimit(protectedBlock: number): Promise<void> {
    if (this.cacheLimitBytes === null) return;
    const blocksDir = this.blocksDir;
    if (!blocksDir) return;

    while (this.cachedBytes > this.cacheLimitBytes) {
      let lruBlock: number | null = null;
      let lruCounter = Number.POSITIVE_INFINITY;
      for (const [blockStr, counter] of Object.entries(this.meta.chunkLastAccess ?? {})) {
        const block = Number(blockStr);
        if (!Number.isFinite(block) || block === protectedBlock) continue;
        if (counter < lruCounter) {
          lruCounter = counter;
          lruBlock = block;
        }
      }

      if (lruBlock === null) break;

      const r = this.blockRange(lruBlock);
      try {
        await blocksDir.removeEntry(this.blockFileName(lruBlock));
      } catch (err) {
        // ignore missing entries
        if ((err as { name?: unknown }).name !== "NotFoundError") throw err;
      }
      this.rangeSet.remove(r.start, r.end);
      delete (this.meta.chunkLastAccess ?? {})[String(lruBlock)];
      this.meta.cachedRanges = this.rangeSet.getRanges();
      this.cachedBytes = this.rangeSet.totalLen();
      await this.persistMeta();
    }
  }

  private async persistMeta(): Promise<void> {
    if (!this.cacheManager) return;
    await this.cacheManager.writeMeta(this.cacheKey, this.meta);
  }
}

// Backwards-compatible alias: this disk implementation uses HTTP Range requests.
export { RemoteStreamingDisk as RemoteRangeDisk };
